//! Opt-in: proves the rustls/ring provider is installed and the platform verifier trusts a
//! public CA. Needs network; run with `--ignored`.

#[tokio::test]
#[ignore = "requires network access"]
async fn https_request_succeeds_with_ring_provider() {
    let client = agent_core::http::client().unwrap();
    let resp = client
        .get("https://crates.io/api/v1/summary")
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success() || resp.status().as_u16() == 403,
        "status {}",
        resp.status()
    );
}
