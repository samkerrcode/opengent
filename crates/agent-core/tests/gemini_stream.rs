//! End-to-end tests of the Gemini Interactions adapter against the in-process SSE server,
//! including a stored-turn round trip: what one response streams must replay as the next
//! request's `input` steps in the same order.

mod common;

use std::collections::HashMap;
use std::time::Duration;

use agent_core::agent::context;
use agent_core::domain::{ContentBlock, Message, Role, StopReason};
use agent_core::providers::gemini::{GeminiConfig, GeminiProvider, FAMILY};
use agent_core::providers::{CompletionRequest, ModelProvider};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

fn request() -> CompletionRequest {
    CompletionRequest {
        model: "gemini-flash-latest".into(),
        system: Some("sys".into()),
        messages: vec![Message::text(Role::User, "hello")],
        max_output_tokens: Some(64),
        tools: Vec::new(),
    }
}

async fn provider(
    status: &'static str,
    body: &'static [u8],
    chunk: usize,
) -> (GeminiProvider, common::MockServer) {
    let server = common::serve_once(status, body, chunk, Duration::ZERO).await;
    let mut config = GeminiConfig::new("g", format!("{}/v1", server.origin));
    config.api_key = Some("test-key".into());
    (
        GeminiProvider::new(config, agent_core::http::client().unwrap()),
        server,
    )
}

const TEXT_BODY: &[u8] = r#"event: interaction.created
data: {"interaction":{"status":"in_progress","model":"gemini-flash-latest"},"event_type":"interaction.created"}

event: step.start
data: {"index":0,"step":{"type":"thought","signature":"","summary":[{"type":"text","text":"Thinking about "}]},"event_type":"step.start"}

event: step.delta
data: {"index":0,"delta":{"type":"thought_summary","content":{"type":"text","text":"greetings"}},"event_type":"step.delta"}

event: step.delta
data: {"index":0,"delta":{"type":"thought_signature","signature":"EpoGCpcG"},"event_type":"step.delta"}

event: step.stop
data: {"index":0,"event_type":"step.stop"}

event: step.start
data: {"index":1,"step":{"type":"model_output","content":[{"type":"text","text":"Hel"}]},"event_type":"step.start"}

event: step.delta
data: {"index":1,"delta":{"type":"text","text":"lo → 世界"},"event_type":"step.delta"}

event: step.stop
data: {"index":1,"event_type":"step.stop"}

event: interaction.completed
data: {"interaction":{"status":"completed","usage":{"total_input_tokens":12,"total_output_tokens":5,"total_thought_tokens":3}},"event_type":"interaction.completed"}

"#
.as_bytes();

#[tokio::test]
async fn streams_identically_at_every_chunk_size() {
    for chunk in [1usize, 7, 64, 4096] {
        let (p, server) = provider("200 OK", TEXT_BODY, chunk).await;
        let c = common::collect(&p, &request()).await;
        assert_eq!(c.err, None, "chunk={chunk}");
        assert_eq!(c.thinking, "Thinking about greetings", "chunk={chunk}");
        assert_eq!(c.text, "Hello → 世界", "chunk={chunk}");
        assert_eq!(c.order, ["thinking", "opaque", "text", "usage", "done"]);
        assert_eq!(c.opaque.len(), 1);
        assert_eq!(c.opaque[0].0, FAMILY);
        assert_eq!(c.opaque[0].1["step"]["signature"], "EpoGCpcG");
        assert_eq!(c.done, Some(StopReason::EndTurn));
        let usage = c.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(
            usage.output_tokens,
            Some(8),
            "thought tokens count as output"
        );

        let raw = server.request.await.unwrap();
        let head = raw.to_ascii_lowercase();
        assert!(head.starts_with("post /v1/interactions "), "{raw}");
        assert!(head.contains("x-goog-api-key: test-key"));
        assert!(head.contains("api-revision: "));
        assert!(!head.contains("key=test-key"), "key never in the URL");
        let body: Value = serde_json::from_str(&raw[raw.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
    }
}

#[tokio::test]
async fn invalid_key_maps_to_auth_hint() {
    let (p, _server) = provider(
        "400 Bad Request",
        br#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID"}]}}"#,
        4096,
    )
    .await;
    let c = common::collect(&p, &request()).await;
    let err = c.err.unwrap();
    assert!(
        err.contains("authentication failed") && err.contains("API key not valid"),
        "{err}"
    );
}

#[tokio::test]
async fn missing_completion_fails_after_retry() {
    let body: &[u8] = br#"event: step.start
data: {"index":0,"step":{"type":"model_output","content":[{"type":"text","text":"partial"}]},"event_type":"step.start"}

"#;
    let (p, _server) = provider("200 OK", body, 4096).await;
    let c = common::collect(&p, &request()).await;
    assert_eq!(c.text, "partial");
    assert_eq!(c.done, None);
    // No interaction id, so the adapter retries the create; the one-shot server is gone by
    // then, so some error (connect or truncated stream) must surface, not a silent success.
    assert!(c.err.is_some());
}

const TOOL_BODY: &[u8] = br#"event: step.start
data: {"index":0,"step":{"type":"thought","signature":"T1"},"event_type":"step.start"}

event: step.stop
data: {"index":0,"event_type":"step.stop"}

event: step.start
data: {"index":1,"step":{"type":"model_output","content":[{"type":"text","text":"Reading both."}]},"event_type":"step.start"}

event: step.stop
data: {"index":1,"event_type":"step.stop"}

event: step.start
data: {"index":2,"step":{"type":"function_call","id":"c1","name":"fs__read"},"event_type":"step.start"}

event: step.delta
data: {"index":2,"delta":{"type":"arguments_delta","arguments":"{\"path\":\"a.txt\"}","signature":"S1"},"event_type":"step.delta"}

event: step.stop
data: {"index":2,"event_type":"step.stop"}

event: step.start
data: {"index":3,"step":{"type":"function_call","id":"c2","name":"fs__read","arguments":{"path":"b.txt"}},"event_type":"step.start"}

event: step.stop
data: {"index":3,"event_type":"step.stop"}

event: interaction.completed
data: {"interaction":{"status":"requires_action"},"event_type":"interaction.completed"}

"#;

/// Simulates the coordinator storing a step: opaque, text and calls in stream order, then
/// the tool results.
#[tokio::test]
async fn tool_turn_round_trips_into_next_request_steps() {
    let (p, _server) = provider("200 OK", TOOL_BODY, 5).await;
    let c = common::collect(&p, &request()).await;
    assert_eq!(c.err, None);
    assert_eq!(c.done, Some(StopReason::ToolUse));
    assert_eq!(c.calls.len(), 2);

    // Rebuild the stored assistant message in stream order. `collect` groups kinds, so
    // reconstruct from what the adapter promises: thought, text, call-signature, call, call.
    let mut blocks = vec![
        ContentBlock::ProviderOpaque {
            provider: FAMILY.into(),
            data: c.opaque[0].1.clone(),
        },
        ContentBlock::Text {
            text: c.text.clone(),
        },
        ContentBlock::ProviderOpaque {
            provider: FAMILY.into(),
            data: c.opaque[1].1.clone(),
        },
    ];
    for (id, name, args) in &c.calls {
        blocks.push(ContentBlock::ToolCall {
            id: id.clone(),
            name: name.clone(),
            arguments: serde_json::from_str(args).unwrap(),
            server: None,
            tool: None,
        });
    }
    for (id, _, _) in &c.calls {
        blocks.push(ContentBlock::ToolResult {
            call_id: id.clone(),
            content: json!([{ "type": "text", "text": format!("contents of {id}") }]),
            is_error: false,
            status: None,
        });
    }
    let stored = Message {
        role: Role::Assistant,
        blocks,
    };
    let mut messages = vec![Message::text(Role::User, "read a and b")];
    messages.extend(context::render_turn(&stored, &HashMap::new(), false));
    let next = CompletionRequest {
        messages,
        ..request()
    };
    let body = GeminiProvider::build_body(&next);
    let input = body["input"].as_array().unwrap();
    let types: Vec<&str> = input.iter().map(|s| s["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        [
            "user_input",
            "thought",
            "model_output",
            "function_call",
            "function_call",
            "function_result",
            "function_result"
        ]
    );
    assert_eq!(input[1], json!({ "type": "thought", "signature": "T1" }));
    assert_eq!(input[3]["signature"], "S1");
    assert_eq!(input[3]["arguments"], json!({ "path": "a.txt" }));
    assert!(input[4].get("signature").is_none());
    assert_eq!(input[5]["call_id"], "c1");
    assert_eq!(input[5]["name"], "fs__read");
    assert_eq!(input[6]["call_id"], "c2");
}

#[tokio::test]
async fn discovery_lists_chat_models() {
    // Page-token handling is covered by the `parse_models_page` unit test; the one-shot
    // server here checks the request shape and the filter end to end.
    let (p, server) = provider(
        "200 OK",
        br#"{"models":[{"name":"models/gemini-flash-latest","inputTokenLimit":1048576,"supportedGenerationMethods":["generateContent"],"thinking":true},{"name":"models/text-embedding-004","supportedGenerationMethods":["embedContent"]}]}"#,
        4096,
    )
    .await;
    let d = p.discover().await.unwrap();
    assert_eq!(d.models, ["gemini-flash-latest"]);
    assert_eq!(
        d.details["gemini-flash-latest"].context_window,
        Some(1_048_576)
    );
    let raw = server.request.await.unwrap();
    assert!(raw.starts_with("GET /v1/models?pageSize=1000 "), "{raw}");
    assert!(raw
        .to_ascii_lowercase()
        .contains("x-goog-api-key: test-key"));
}

/// Accepts one connection per entry, serving `responses[i]` (an SSE body) to request `i`.
/// Returns the origin and every captured request.
async fn serve_many(responses: Vec<&'static str>) -> (String, oneshot::Receiver<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let mut requests = Vec::new();
        for body in responses {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let mut buf = vec![0u8; 64 * 1024];
            let mut req = Vec::new();
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
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
                    }
                }
            }
            requests.push(String::from_utf8_lossy(&req).into_owned());
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            if sock.write_all(head.as_bytes()).await.is_err()
                || sock.write_all(body.as_bytes()).await.is_err()
            {
                break;
            }
        }
        let _ = tx.send(requests);
    });
    (origin, rx)
}

fn provider_at(origin: String) -> GeminiProvider {
    let mut config = GeminiConfig::new("g", format!("{origin}/v1"));
    config.api_key = Some("test-key".into());
    GeminiProvider::new(config, agent_core::http::client().unwrap())
}

#[tokio::test]
async fn dropped_stream_resumes_from_last_event_id() {
    let truncated: &'static str = r#"event: interaction.created
data: {"interaction":{"id":"i_1","status":"in_progress"},"event_id":"e0","event_type":"interaction.created"}

event: step.start
data: {"index":0,"step":{"type":"model_output","content":[{"type":"text","text":"par"}]},"event_id":"e1","event_type":"step.start"}

"#;
    let resumed: &'static str = r#"event: step.delta
data: {"index":0,"delta":{"type":"text","text":"tial"},"event_id":"e2","event_type":"step.delta"}

event: step.stop
data: {"index":0,"event_id":"e3","event_type":"step.stop"}

event: interaction.completed
data: {"interaction":{"status":"completed","usage":{"total_input_tokens":4}},"event_type":"interaction.completed"}

"#;
    let (origin, requests) = serve_many(vec![truncated, resumed]).await;
    let c = common::collect(&provider_at(origin), &request()).await;
    let requests = requests.await.unwrap();
    assert_eq!(c.err, None, "requests: {requests:?}");
    assert_eq!(c.text, "partial");
    assert_eq!(c.done, Some(StopReason::EndTurn));
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].starts_with("GET /v1/interactions/i_1?stream=true&last_event_id=e1 "),
        "{}",
        requests[1]
    );
}

/// A server-sent `error` is a verdict, not a dropped connection: it must surface as-is
/// instead of being retried into a generic failure.
#[tokio::test]
async fn error_event_is_not_retried() {
    let body: &'static str = r#"event: interaction.created
data: {"interaction":{"id":"i_1","status":"in_progress"},"event_id":"e0","event_type":"interaction.created"}

event: error
data: {"error":{"code":"RESOURCE_EXHAUSTED","message":"quota exceeded"},"event_id":"e1","event_type":"error"}

"#;
    let (origin, requests) = serve_many(vec![body]).await;
    let c = common::collect(&provider_at(origin), &request()).await;
    let err = c.err.expect("stream must fail");
    assert!(err.contains("quota exceeded"), "{err}");
    assert_eq!(requests.await.unwrap().len(), 1, "no resume or retry");
}

#[tokio::test]
async fn requires_continuation_resends_the_token() {
    let first: &'static str = r#"event: interaction.created
data: {"interaction":{"id":"i_1","status":"in_progress"},"event_id":"e0","event_type":"interaction.created"}

event: interaction.completed
data: {"interaction":{"status":"requires_continuation","continuation_token":"tok-42"},"event_type":"interaction.completed"}

"#;
    let rest: &'static str = r#"event: step.start
data: {"index":0,"step":{"type":"model_output","content":[{"type":"text","text":"done."}]},"event_type":"step.start"}

event: step.stop
data: {"index":0,"event_type":"step.stop"}

event: interaction.completed
data: {"interaction":{"status":"completed"},"event_type":"interaction.completed"}

"#;
    let (origin, requests) = serve_many(vec![first, rest]).await;
    let c = common::collect(&provider_at(origin), &request()).await;
    let requests = requests.await.unwrap();
    assert_eq!(c.err, None);
    assert_eq!(c.text, "done.");
    assert_eq!(c.done, Some(StopReason::EndTurn));
    let second: Value =
        serde_json::from_str(&requests[1][requests[1].find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(second["continuation_token"], "tok-42");
    assert!(
        requests[1].starts_with("POST /v1/interactions "),
        "{}",
        requests[1]
    );
}
