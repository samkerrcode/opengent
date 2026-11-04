//! Shared harness for provider adapter tests: a one-shot in-process HTTP/1.1 server that
//! replays a canned body in awkward chunk sizes and captures the raw request, plus a
//! collector that folds a provider stream into a comparable summary.
#![allow(dead_code)]

use std::time::Duration;

use agent_core::domain::StopReason;
use agent_core::providers::{CompletionRequest, ModelProvider, ProviderEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub struct MockServer {
    /// `http://127.0.0.1:<port>` — append the API root path.
    pub origin: String,
    /// The raw request (head + body) once the server has read it.
    pub request: oneshot::Receiver<String>,
}

/// Serves exactly one request with `status` and `body`, written `chunk` bytes at a time with
/// `delay` between writes.
pub async fn serve_once(
    status: &'static str,
    body: &'static [u8],
    chunk: usize,
    delay: Duration,
) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (req_tx, req_rx) = oneshot::channel();
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
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length").then_some(v)
                    })
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if req.len() >= pos + 4 + len {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let _ = req_tx.send(String::from_utf8_lossy(&req).into_owned());
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
    MockServer {
        origin: format!("http://127.0.0.1:{}", addr.port()),
        request: req_rx,
    }
}

#[derive(Debug, Default)]
pub struct Collected {
    pub text: String,
    pub thinking: String,
    pub calls: Vec<(String, String, String)>,
    /// (family, data) in stream order.
    pub opaque: Vec<(&'static str, serde_json::Value)>,
    /// Every event kind in arrival order, for ordering assertions.
    pub order: Vec<&'static str>,
    pub done: Option<StopReason>,
    pub err: Option<String>,
    pub usage: Option<agent_core::domain::Usage>,
}

pub async fn collect(provider: &dyn ModelProvider, request: &CompletionRequest) -> Collected {
    let mut rx = provider.stream(request, CancellationToken::new()).unwrap();
    let mut c = Collected::default();
    while let Some(ev) = rx.recv().await {
        let kind = match ev {
            ProviderEvent::TextDelta(t) => {
                c.text.push_str(&t);
                "text"
            }
            ProviderEvent::ReasoningDelta(t) => {
                c.thinking.push_str(&t);
                "thinking"
            }
            ProviderEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                c.calls.push((id, name, arguments));
                "tool_call"
            }
            ProviderEvent::Opaque { provider, data } => {
                c.opaque.push((provider, data));
                "opaque"
            }
            ProviderEvent::Usage(u) => {
                c.usage = Some(u);
                "usage"
            }
            ProviderEvent::Done(r) => {
                c.done = Some(r);
                "done"
            }
            ProviderEvent::Error(e) => {
                c.err = Some(e);
                "error"
            }
        };
        if c.order.last() != Some(&kind) {
            c.order.push(kind);
        }
    }
    c
}
