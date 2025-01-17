use hyper::{
    body::Buf,
    client::{Client as HyperClient, HttpConnector},
};
#[cfg(feature = "with-openssl")]
use hyper_openssl::HttpsConnector;
#[cfg(feature = "with-hypertls")]
use hyper_tls::HttpsConnector;
use serde;
use serde_json::{self, Value};
use tokio::sync::Mutex;

use std::collections::btree_map::Range;
use std::collections::BTreeMap;
use std::ops::{
    Bound,
    Bound::{Included, Unbounded},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::error::Error;
use crate::token::IdInfo;

pub struct Client {
    client: reqwest::Client,
    pub audiences: Vec<String>,
    pub hosted_domains: Vec<String>,
    certs: Arc<Mutex<CachedCerts>>,
}

#[derive(Debug, Clone, Deserialize)]
struct CertsObject {
    keys: Vec<Cert>,
}

#[derive(Debug, Clone, Deserialize)]
struct Cert {
    kid: String,
    e: String,
    kty: String,
    alg: String,
    n: String,
    r#use: String,
}

type Key = String;

#[derive(Clone)]
pub struct CachedCerts {
    keys: BTreeMap<Key, Cert>,
    pub expiry: Option<Instant>,
}

impl CachedCerts {
    pub fn new() -> Self {
        Self {
            keys: BTreeMap::new(),
            expiry: None,
        }
    }

    fn certs_url() -> &'static str {
        "https://www.googleapis.com/oauth2/v2/certs"
    }

    fn get_range<'a>(&'a self, kid: &Option<String>) -> Result<Range<'a, Key, Cert>, Error> {
        match kid {
            None => Ok(self
                .keys
                .range::<String, (Bound<&String>, Bound<&String>)>((Unbounded, Unbounded))),
            Some(kid) => {
                if !self.keys.contains_key(kid) {
                    return Err(Error::InvalidKey);
                }
                Ok(self
                    .keys
                    .range::<String, (Bound<&String>, Bound<&String>)>((
                        Included(kid),
                        Included(kid),
                    )))
            }
        }
    }

    /// Downloads the public Google certificates if it didn't do so already, or based on expiry of
    /// their Cache-Control. Returns `true` if the certificates were updated.
    pub async fn refresh_if_needed(&mut self) -> Result<bool, Error> {
        let check = match self.expiry {
            None => true,
            Some(expiry) => expiry <= Instant::now(),
        };

        if !check {
            return Ok(false);
        }

        let client = Client::new()?;
        let certs: CertsObject = client.get_any(Self::certs_url(), &mut self.expiry).await?;
        self.keys = BTreeMap::new();

        for cert in certs.keys {
            self.keys.insert(cert.kid.clone(), cert);
        }

        Ok(true)
    }
}

impl Client {
    pub fn new() -> Result<Self, Error> {
        #[cfg(feature = "with-hypertls")]
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .http2_max_frame_size(0x2000)
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|e| Error::ConnectionError(Box::new(e)))?;
        // let ssl = HttpsConnector::new();
        // #[cfg(feature = "with-openssl")]
        // let ssl = HttpsConnector::new().expect("unable to build HttpsConnector");
        // let client = HyperClient::builder()
        //     .http2_max_frame_size(0x2000)
        //     .pool_max_idle_per_host(0)
        //     .build(ssl);
        Ok(Client {
            certs: Arc::new(Mutex::new(CachedCerts::new())),
            client,
            audiences: vec![],
            hosted_domains: vec![],
        })
    }

    /// Verifies that the token is signed by Google's OAuth cerificate,
    /// and check that it has a valid issuer, audience, and hosted domain.
    ///
    /// Returns an error if the client has no configured audiences.
    pub async fn verify(
        &self,
        id_token: &str,
        cached_certs: &CachedCerts,
    ) -> Result<IdInfo, Error> {
        let unverified_header = jsonwebtoken::decode_header(&id_token)?;

        use jsonwebtoken::{Algorithm, DecodingKey, Validation};

        for (_, cert) in cached_certs.get_range(&unverified_header.kid)? {
            // Check each certificate

            let mut validation = Validation::new(Algorithm::RS256);
            validation.set_audience(&self.audiences);
            let token_data = jsonwebtoken::decode::<IdInfo>(
                &id_token,
                &DecodingKey::from_rsa_components(&cert.n, &cert.e),
                &validation,
            )?;

            token_data.claims.verify(self)?;

            return Ok(token_data.claims);
        }

        Err(Error::InvalidToken)
    }

    pub async fn verify_token(&self, id_token: &str) -> Result<IdInfo, Error> {
        let mut certs = self.certs.lock().await;
        if certs.keys.len() == 0 {
            certs.refresh_if_needed().await?;
        }
        let v = self.verify(id_token, &certs).await?;
        Ok(v)
    }

    /// Checks the token using Google's slow OAuth-like authentication flow.
    ///
    /// This checks that the token is signed using Google's OAuth certificate,
    /// but does not check the issuer, audience, or other application-specific verifications.
    ///
    /// This is NOT the recommended way to use the library, but can be used in combination with
    /// [IdInfo.verify](https://docs.rs/google-signin/latest/google_signin/struct.IdInfo.html#impl)
    /// for applications with more complex error-handling requirements.
    pub async fn get_slow_unverified(&self, id_token: &str) -> Result<IdInfo<bool, u64>, Error> {
        self.get_any(
            &format!(
                "https://www.googleapis.com/oauth2/v3/tokeninfo?id_token={}",
                id_token
            ),
            &mut None,
        )
        .await
    }

    async fn get_any<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        cache: &mut Option<Instant>,
    ) -> Result<T, Error> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| Error::ConnectionError(Box::new(e)))?;

        let status = response.status().as_u16();
        match status {
            200..=299 => {}
            _ => {
                return Err(Error::InvalidToken);
            }
        }

        if let Some(value) = response.headers().get("Cache-Control") {
            if let Ok(value) = value.to_str() {
                if let Some(cc) = cache_control::CacheControl::from_value(value) {
                    if let Some(max_age) = cc.max_age {
                        let seconds = max_age.num_seconds();
                        if seconds >= 0 {
                            *cache = Some(Instant::now() + Duration::from_secs(seconds as u64));
                        }
                    }
                }
            }
        }

        let rs = response
            .json::<T>()
            .await
            .map_err(|e| Error::ConnectionError(Box::new(e)))?;

        Ok(rs)
    }
}
