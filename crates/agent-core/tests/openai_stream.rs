//! End-to-end tests of the OpenAI Responses adapter against the in-process SSE server,
//! including a stored-turn round trip: what one response streams must replay as the next
//! request's `input` items in the same order.

mod common;

use std::collections::HashMap;
use std::time::Duration;

use agent_core::agent::context;
use agent_core::domain::{ContentBlock, Message, Role, StopReason};
use agent_core::providers::openai::{OpenAiConfig, OpenAiProvider, FAMILY};
use agent_core::providers::{CompletionRequest, ModelProvider};
use serde_json::json;

fn request(model: &str) -> CompletionRequest {
    CompletionRequest {
        model: model.into(),
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
) -> (OpenAiProvider, common::MockServer) {
    let server = common::serve_once(status, body, chunk, Duration::ZERO).await;
    let mut config = OpenAiConfig::new("o", format!("{}/v1", server.origin));
    config.api_key = Some("sk-test".into());
    (
        OpenAiProvider::new(config, agent_core::http::client().unwrap()),
        server,
    )
}

const TEXT_BODY: &[u8] = r#"event: response.created
data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_1","status":"in_progress"}}

event: response.output_item.added
data: {"type":"response.output_item.added","output_index":0,"item":{"id":"rs_1","type":"reasoning","summary":[]}}

event: response.reasoning_summary_part.added
data: {"type":"response.reasoning_summary_part.added","output_index":0,"summary_index":0,"part":{"type":"summary_text","text":""}}

event: response.reasoning_summary_text.delta
data: {"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0,"delta":"Thinking about greetings"}

event: response.output_item.done
data: {"type":"response.output_item.done","output_index":0,"item":{"id":"rs_1","type":"reasoning","status":"completed","summary":[{"type":"summary_text","text":"Thinking about greetings"}],"encrypted_content":"gAAAA-enc"}}

event: response.output_item.added
data: {"type":"response.output_item.added","output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","content":[]}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Hel"}

event: response.output_text.delta
data: {"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"lo → 世界"}

event: response.output_text.done
data: {"type":"response.output_text.done","output_index":1,"content_index":0,"text":"Hello → 世界"}

event: response.output_item.done
data: {"type":"response.output_item.done","output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello → 世界","annotations":[]}]}}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":12,"input_tokens_details":{"cached_tokens":2},"output_tokens":8,"output_tokens_details":{"reasoning_tokens":3}}}}

"#
.as_bytes();

#[tokio::test]
async fn streams_identically_at_every_chunk_size() {
    for chunk in [1usize, 7, 64, 4096] {
        let (p, server) = provider("200 OK", TEXT_BODY, chunk).await;
        let c = common::collect(&p, &request("gpt-5-mini")).await;
        assert_eq!(c.err, None, "chunk={chunk}");
        assert_eq!(c.thinking, "Thinking about greetings", "chunk={chunk}");
        assert_eq!(c.text, "Hello → 世界", "chunk={chunk}");
        assert_eq!(c.order, ["thinking", "opaque", "text", "usage", "done"]);
        assert_eq!(c.opaque.len(), 1);
        assert_eq!(c.opaque[0].0, FAMILY);
        assert_eq!(c.opaque[0].1["item"]["encrypted_content"], "gAAAA-enc");
        assert!(
            c.opaque[0].1["item"].get("id").is_none(),
            "ids are not stored"
        );
        assert_eq!(c.done, Some(StopReason::EndTurn));
        let usage = c.usage.unwrap();
        assert_eq!(
            (
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_tokens
            ),
            (Some(12), Some(8), Some(2))
        );

        let raw = server.request.await.unwrap();
        let head = raw.to_ascii_lowercase();
        assert!(head.starts_with("post /v1/responses "), "{raw}");
        assert!(head.contains("authorization: bearer sk-test"));
        assert!(head.contains("accept: text/event-stream"));
        assert!(!raw.contains("?"), "nothing in the query string");
        let body = common::request_json(&raw);
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["instructions"], "sys");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    }
}

#[tokio::test]
async fn invalid_key_maps_to_auth_hint() {
    let (p, _server) = provider(
        "401 Unauthorized",
        br#"{"error":{"message":"Incorrect API key provided: sk-test.","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#,
        4096,
    )
    .await;
    let c = common::collect(&p, &request("gpt-5")).await;
    let err = c.err.unwrap();
    assert!(
        err.contains("authentication failed") && err.contains("Incorrect API key"),
        "{err}"
    );
    assert_eq!(c.done, None);
}

#[tokio::test]
async fn truncated_stream_is_reported() {
    let body: &[u8] = br#"event: response.output_text.delta
data: {"type":"response.output_text.delta","output_index":0,"delta":"partial"}

"#;
    let (p, _server) = provider("200 OK", body, 4096).await;
    let c = common::collect(&p, &request("gpt-4.1")).await;
    assert_eq!(c.text, "partial");
    assert_eq!(c.done, None);
    assert!(c.err.unwrap().contains("before the response completed"));
}

#[tokio::test]
async fn server_error_event_surfaces_its_message() {
    let body: &[u8] = br#"event: error
data: {"type":"error","code":"rate_limit_exceeded","message":"Rate limit reached for gpt-5","param":null}

"#;
    let (p, _server) = provider("200 OK", body, 4096).await;
    let c = common::collect(&p, &request("gpt-5")).await;
    assert!(c.err.unwrap().contains("Rate limit reached"));
}

const TOOL_BODY: &[u8] = br#"event: response.output_item.done
data: {"type":"response.output_item.done","output_index":0,"item":{"id":"rs_1","type":"reasoning","summary":[],"encrypted_content":"ENC1"}}

event: response.output_item.added
data: {"type":"response.output_item.added","output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","content":[]}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","output_index":1,"delta":"Reading both."}

event: response.output_item.done
data: {"type":"response.output_item.done","output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"Reading both."}]}}

event: response.output_item.added
data: {"type":"response.output_item.added","output_index":2,"item":{"id":"fc_1","type":"function_call","call_id":"call_a","name":"fs__read","arguments":""}}

event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"path\":\"a.txt\"}"}

event: response.output_item.done
data: {"type":"response.output_item.done","output_index":2,"item":{"id":"fc_1","type":"function_call","call_id":"call_a","name":"fs__read","arguments":"{\"path\":\"a.txt\"}","status":"completed"}}

event: response.output_item.done
data: {"type":"response.output_item.done","output_index":3,"item":{"id":"fc_2","type":"function_call","call_id":"call_b","name":"fs__read","arguments":"{\"path\":\"b.txt\"}","status":"completed"}}

event: response.completed
data: {"type":"response.completed","response":{"status":"completed"}}

"#;

/// Simulates the coordinator storing a step: opaque, text and calls in stream order, then
/// the tool results, and checks the next request replays it item for item.
#[tokio::test]
async fn tool_turn_round_trips_into_next_request_items() {
    let (p, _server) = provider("200 OK", TOOL_BODY, 5).await;
    let c = common::collect(&p, &request("o4-mini")).await;
    assert_eq!(c.err, None);
    assert_eq!(c.done, Some(StopReason::ToolUse));
    assert_eq!(c.calls.len(), 2);
    assert_eq!(c.order, ["opaque", "text", "opaque", "tool_call", "done"]);

    let opaque = |i: usize| ContentBlock::ProviderOpaque {
        provider: FAMILY.into(),
        data: c.opaque[i].1.clone(),
    };
    let mut blocks = vec![
        opaque(0),
        ContentBlock::Text {
            text: c.text.clone(),
        },
        opaque(1),
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
    let body = OpenAiProvider::build_body(&CompletionRequest {
        messages,
        ..request("o4-mini")
    });
    let input = body["input"].as_array().unwrap();
    let types: Vec<&str> = input.iter().map(|s| s["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        [
            "message",
            "reasoning",
            "message",
            "function_call",
            "function_call",
            "function_call_output",
            "function_call_output"
        ]
    );
    assert_eq!(
        input[1],
        json!({ "type": "reasoning", "summary": [], "encrypted_content": "ENC1" })
    );
    assert_eq!(input[2]["phase"], "commentary");
    assert_eq!(input[2]["content"][0]["text"], "Reading both.");
    assert_eq!(input[3]["call_id"], "call_a");
    assert_eq!(input[3]["arguments"], r#"{"path":"a.txt"}"#);
    assert_eq!(input[5]["call_id"], "call_a");
    assert_eq!(input[5]["output"], "contents of call_a");
    assert_eq!(input[6]["call_id"], "call_b");
    assert!(
        input.iter().all(|i| i.get("id").is_none()),
        "no item ids under store:false"
    );
}

#[tokio::test]
async fn discovery_lists_chat_models_with_metadata() {
    let (p, server) = provider(
        "200 OK",
        br#"{"object":"list","data":[{"id":"gpt-5","object":"model"},{"id":"text-embedding-3-small","object":"model"},{"id":"gpt-4.1-mini","object":"model"},{"id":"whisper-1","object":"model"}]}"#,
        4096,
    )
    .await;
    let d = p.discover().await.unwrap();
    assert_eq!(d.models, ["gpt-4.1-mini", "gpt-5"]);
    assert_eq!(d.details["gpt-5"].reasoning, Some(true));
    assert_eq!(d.details["gpt-4.1-mini"].context_window, Some(1_047_576));
    let raw = server.request.await.unwrap();
    assert!(raw.starts_with("GET /v1/models "), "{raw}");
    assert!(raw
        .to_ascii_lowercase()
        .contains("authorization: bearer sk-test"));
}

/// One adapter instance serves several requests over a reused connection pool.
#[tokio::test]
async fn sequential_requests_share_one_adapter() {
    let done = r#"event: response.output_item.done
data: {"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","content":[]}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","output_index":0,"delta":"ok"}

event: response.completed
data: {"type":"response.completed","response":{"status":"completed"}}

"#;
    let (origin, requests) = common::serve_many(vec![done, done]).await;
    let mut config = OpenAiConfig::new("o", format!("{origin}/v1"));
    config.api_key = Some("sk-test".into());
    let p = OpenAiProvider::new(config, agent_core::http::client().unwrap());
    for _ in 0..2 {
        let c = common::collect(&p, &request("gpt-4.1")).await;
        assert_eq!((c.text.as_str(), c.done), ("ok", Some(StopReason::EndTurn)));
    }
    let requests = requests.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(common::request_json(&requests[1])
        .get("reasoning")
        .is_none());
}
