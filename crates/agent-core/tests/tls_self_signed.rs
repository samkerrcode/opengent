//! TLS behaviour for self-signed LAN endpoints: the default (OS-verifier) client must reject
//! them with a clear certificate error, while the per-provider insecure and pinned-CA clients
//! must connect.

use std::sync::Arc;

use agent_core::providers::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
use rustls_pki_types::PrivateKeyDer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// Serves a fixed `GET /models` JSON response over a self-signed TLS connection.
/// Returns the base URL plus the server's PEM certificate (for the pinning test).
async fn serve_tls_once() -> (String, String) {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".into(), "localhost".into()]).unwrap();
    let cert_der = cert.der().clone();
    let cert_pem = cert.pem();
    let key_der = PrivateKeyDer::try_from(signing_key.serialize_der()).unwrap();

    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(tls_config));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut sock = acceptor.accept(sock).await.unwrap();
        let mut buf = vec![0u8; 16 * 1024];
        let mut req = Vec::new();
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            if n == 0 {
                return;
            }
            req.extend_from_slice(&buf[..n]);
            if req.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let body = br#"{"object":"list","data":[{"id":"tls-model","object":"model"}]}"#;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        sock.write_all(head.as_bytes()).await.unwrap();
        sock.write_all(body).await.unwrap();
        sock.shutdown().await.ok();
    });
    (format!("https://127.0.0.1:{port}/v1"), cert_pem)
}

fn provider(http: reqwest::Client, base_url: &str) -> OpenAiCompatProvider {
    OpenAiCompatProvider::new(OpenAiCompatConfig::new("t", base_url), http)
}

#[tokio::test]
async fn self_signed_cert_is_rejected_with_a_clear_error() {
    let (base_url, _cert) = serve_tls_once().await;
    let p = provider(agent_core::http::client().unwrap(), &base_url);
    let err = p.list_models().await.unwrap_err().to_string();
    assert!(
        err.contains("TLS certificate verification failed"),
        "expected certificate error, got: {err}"
    );
}

#[tokio::test]
async fn insecure_client_connects_to_self_signed() {
    let (base_url, _cert) = serve_tls_once().await;
    let p = provider(agent_core::http::client_insecure().unwrap(), &base_url);
    let models = p.list_models().await.unwrap();
    assert_eq!(models, vec!["tls-model"]);
}

#[tokio::test]
async fn pinned_ca_client_connects_to_self_signed() {
    let (base_url, pem) = serve_tls_once().await;
    let http = agent_core::http::client_with_extra_ca(&pem).unwrap();
    let p = provider(http, &base_url);
    let models = p.list_models().await.unwrap();
    assert_eq!(models, vec!["tls-model"]);
}
