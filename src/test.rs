use serde_json::Value;

pub use super::CachedCerts;

#[tokio::test]
async fn google() {
    // let client = reqwest::Client::new();
    // let proxy = reqwest::Proxy::http("http://0.0.0.0:8888").unwrap();

    // let client = reqwest::Client::builder()
    //     .connect_timeout(std::time::Duration::from_secs(10))
    //     .proxy(proxy)
    //     .build()
    //     .unwrap();
    // let url = "https://www.googleapis.com/oauth2/v2/certs";
    // let url = "https://baidu.com";
    // let res = client
    //     .get(url)
    //     .send()
    //     .await
    //     .unwrap()
    //     .text()
    //     .await
    //     .unwrap();
    // eprintln!("rq1:{:?}", res);
    let mut certs = CachedCerts::new();

    let first = certs.refresh_if_needed().await.expect("failed");
    let second = certs.refresh_if_needed().await.expect("failed");
    assert_eq!(first, true);
    assert_eq!(second, false);
}
