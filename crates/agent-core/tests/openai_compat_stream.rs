//! End-to-end test of the OpenAI-compatible adapter against a minimal in-process HTTP/1.1
//! server that replays canned SSE bodies in awkward chunk sizes.

use std::time::Duration;

use agent_core::domain::{Message, Role, StopReason};
use agent_core::providers::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
use agent_core::providers::{CompletionRequest, ModelProvider, ProviderEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

async fn serve_once(
    status: &'static str,
    body: &'static [u8],
    chunk: usize,
    delay: Duration,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 64 * 1024];
        let mut req = Vec::new();
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            req.extend_from_slice(&buf[..n]);
            if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&req[..pos]).to_string();
                let len = head
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("content-length: ")
                            .or_else(|| l.strip_prefix("Content-Length: "))
                    })
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if req.len() >= pos + 4 + len {
                    break;
                }
            }
        }
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        sock.write_all(head.as_bytes()).await.unwrap();
        for part in body.chunks(chunk.max(1)) {
            sock.write_all(part).await.unwrap();
            sock.flush().await.unwrap();
            tokio::time::sleep(delay).await;
        }
        sock.shutdown().await.ok();
    });
    format!("http://127.0.0.1:{}/v1", addr.port())
}

fn request() -> CompletionRequest {
    CompletionRequest {
        model: "test-model".into(),
        system: Some("sys".into()),
        messages: vec![Message::text(Role::User, "hello")],
        max_output_tokens: Some(64),
    }
}

async fn collect(
    provider: &OpenAiCompatProvider,
) -> (String, Option<StopReason>, Option<String>, bool) {
    let mut rx = provider
        .stream(request(), CancellationToken::new())
        .unwrap();
    let (mut text, mut done, mut err, mut usage) = (String::new(), None, None, false);
    while let Some(ev) = rx.recv().await {
        match ev {
            ProviderEvent::TextDelta(t) => text.push_str(&t),
            ProviderEvent::Usage(u) => usage = u.input_tokens.is_some(),
            ProviderEvent::Done(r) => done = Some(r),
            ProviderEvent::Error(e) => err = Some(e),
        }
    }
    (text, done, err, usage)
}

const OK_BODY: &[u8] = r#"data: {"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}

data: {"id":"1","choices":[{"index":0,"delta":{"content":"Hel"},"finish_reason":null}]}

data: {"id":"1","choices":[{"index":0,"delta":{"content":"lo → 世界"},"finish_reason":null}]}

data: {"id":"1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: {"id":"1","choices":[],"usage":{"prompt_tokens":12,"completion_tokens":5}}

data: [DONE]

"#
.as_bytes();

#[tokio::test]
async fn streams_text_usage_and_done_across_odd_chunking() {
    for chunk in [1usize, 7, 64, 4096] {
        let base = serve_once("200 OK", OK_BODY, chunk, Duration::from_millis(1)).await;
        let provider = OpenAiCompatProvider::new(
            OpenAiCompatConfig::new("t", base),
            agent_core::http::client().unwrap(),
        );
        let (text, done, err, usage) = collect(&provider).await;
        assert_eq!(err, None, "chunk={chunk}");
        assert_eq!(text, "Hello → 世界", "chunk={chunk}");
        assert_eq!(done, Some(StopReason::EndTurn));
        assert!(usage, "usage missing at chunk={chunk}");
    }
}

#[tokio::test]
async fn maps_auth_error_with_server_message() {
    let base = serve_once(
        "401 Unauthorized",
        br#"{"error":{"message":"Incorrect API key","type":"invalid_request_error"}}"#,
        4096,
        Duration::ZERO,
    )
    .await;
    let provider = OpenAiCompatProvider::new(
        OpenAiCompatConfig::new("t", base),
        agent_core::http::client().unwrap(),
    );
    let (_, done, err, _) = collect(&provider).await;
    assert_eq!(done, None);
    let err = err.unwrap();
    assert!(
        err.contains("authentication failed") && err.contains("Incorrect API key"),
        "{err}"
    );
}

#[tokio::test]
async fn truncated_stream_is_reported() {
    let body: &[u8] = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n";
    let base = serve_once("200 OK", body, 4096, Duration::ZERO).await;
    let provider = OpenAiCompatProvider::new(
        OpenAiCompatConfig::new("t", base),
        agent_core::http::client().unwrap(),
    );
    let (text, done, err, _) = collect(&provider).await;
    assert_eq!(text, "partial");
    assert_eq!(done, None);
    assert!(err.unwrap().contains("without a completion marker"));
}

#[tokio::test]
async fn connection_refused_is_friendly() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    drop(listener);
    let provider = OpenAiCompatProvider::new(
        OpenAiCompatConfig::new("t", base),
        agent_core::http::client().unwrap(),
    );
    let (_, _, err, _) = collect(&provider).await;
    assert!(err.unwrap().contains("could not connect"));
}

#[tokio::test]
async fn cancellation_stops_the_stream() {
    let base = serve_once("200 OK", OK_BODY, 8, Duration::from_millis(30)).await;
    let provider = OpenAiCompatProvider::new(
        OpenAiCompatConfig::new("t", base),
        agent_core::http::client().unwrap(),
    );
    let cancel = CancellationToken::new();
    let mut rx = provider.stream(request(), cancel.clone()).unwrap();
    let first = rx.recv().await;
    assert!(matches!(first, Some(ProviderEvent::TextDelta(_))));
    cancel.cancel();
    let mut rest = 0;
    while rx.recv().await.is_some() {
        rest += 1;
    }
    assert!(rest <= 2, "stream kept going after cancel: {rest} events");
}
