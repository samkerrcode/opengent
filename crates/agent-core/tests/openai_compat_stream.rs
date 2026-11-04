//! End-to-end test of the OpenAI-compatible adapter against a minimal in-process HTTP/1.1
//! server that replays canned SSE bodies in awkward chunk sizes.

mod common;

use std::time::Duration;

use agent_core::domain::{Message, Role, StopReason};
use agent_core::providers::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
use agent_core::providers::{CompletionRequest, ModelProvider, ProviderEvent};
use common::Collected;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// One-shot server; returns the `/v1` API root.
async fn serve_once(
    status: &'static str,
    body: &'static [u8],
    chunk: usize,
    delay: Duration,
) -> String {
    format!(
        "{}/v1",
        common::serve_once(status, body, chunk, delay).await.origin
    )
}

fn request() -> CompletionRequest {
    CompletionRequest {
        model: "test-model".into(),
        system: Some("sys".into()),
        messages: vec![Message::text(Role::User, "hello")],
        max_output_tokens: Some(64),
        tools: Vec::new(),
    }
}

async fn collect(provider: &OpenAiCompatProvider) -> Collected {
    common::collect(provider, &request()).await
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
        let c = collect(&provider).await;
        assert_eq!(c.err, None, "chunk={chunk}");
        assert_eq!(c.text, "Hello → 世界", "chunk={chunk}");
        assert_eq!(c.done, Some(StopReason::EndTurn));
        assert!(
            c.usage.is_some_and(|u| u.input_tokens.is_some()),
            "usage missing at chunk={chunk}"
        );
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
    let c = collect(&provider).await;
    assert_eq!(c.done, None);
    let err = c.err.unwrap();
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
    let c = collect(&provider).await;
    assert_eq!(c.text, "partial");
    assert_eq!(c.done, None);
    assert!(c.err.unwrap().contains("without a completion marker"));
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
    let c = collect(&provider).await;
    assert!(c.err.unwrap().contains("could not connect"));
}

#[tokio::test]
async fn cancellation_stops_the_stream() {
    let base = serve_once("200 OK", OK_BODY, 8, Duration::from_millis(30)).await;
    let provider = OpenAiCompatProvider::new(
        OpenAiCompatConfig::new("t", base),
        agent_core::http::client().unwrap(),
    );
    let cancel = CancellationToken::new();
    let mut rx = provider.stream(&request(), cancel.clone()).unwrap();
    let first = rx.recv().await;
    assert!(matches!(first, Some(ProviderEvent::TextDelta(_))));
    cancel.cancel();
    let mut rest = 0;
    while rx.recv().await.is_some() {
        rest += 1;
    }
    assert!(rest <= 2, "stream kept going after cancel: {rest} events");
}

#[tokio::test]
async fn tool_calls_are_assembled_across_chunks() {
    let body: &[u8] = br#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"Checking."},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"fs__read","arguments":""}}]},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"time__now","arguments":"{}"}}]},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: [DONE]

"#;
    for chunk in [3usize, 4096] {
        let base = serve_once("200 OK", body, chunk, Duration::ZERO).await;
        let provider = OpenAiCompatProvider::new(
            OpenAiCompatConfig::new("t", base),
            agent_core::http::client().unwrap(),
        );
        let c = collect(&provider).await;
        assert_eq!(c.err, None);
        assert_eq!(c.text, "Checking.");
        assert_eq!(c.done, Some(StopReason::ToolUse));
        assert_eq!(
            c.calls,
            vec![
                (
                    "call_a".to_string(),
                    "fs__read".to_string(),
                    r#"{"path":"a.txt"}"#.to_string()
                ),
                (
                    "call_b".to_string(),
                    "time__now".to_string(),
                    "{}".to_string()
                ),
            ]
        );
    }
}

#[tokio::test]
async fn tool_calls_with_stop_finish_reason_still_report_tool_use() {
    let body: &[u8] = br#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"function":{"name":"x","arguments":"{}"}}]},"finish_reason":"stop"}]}

data: [DONE]

"#;
    let base = serve_once("200 OK", body, 4096, Duration::ZERO).await;
    let provider = OpenAiCompatProvider::new(
        OpenAiCompatConfig::new("t", base),
        agent_core::http::client().unwrap(),
    );
    let c = collect(&provider).await;
    assert_eq!(c.done, Some(StopReason::ToolUse));
    assert_eq!(c.calls.len(), 1);
    assert!(
        c.calls[0].0.starts_with("call_0_"),
        "missing ids are synthesized"
    );
}

#[tokio::test]
async fn reasoning_content_is_streamed_separately() {
    let body: &[u8] = br#"data: {"choices":[{"index":0,"delta":{"reasoning_content":"let me think"},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"reasoning_content":" about this"},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"content":"the answer"},"finish_reason":"stop"}]}

data: [DONE]

"#;
    let base = serve_once("200 OK", body, 4096, Duration::ZERO).await;
    let provider = OpenAiCompatProvider::new(
        OpenAiCompatConfig::new("t", base),
        agent_core::http::client().unwrap(),
    );
    let c = collect(&provider).await;
    assert_eq!(c.err, None);
    assert_eq!(c.thinking, "let me think about this");
    assert_eq!(c.text, "the answer");
    assert_eq!(c.done, Some(StopReason::EndTurn));
}
