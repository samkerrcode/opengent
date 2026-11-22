//! End-to-end tests of the xAI adapter (the Responses protocol with xAI's dialect) against
//! the in-process SSE server: opaque state tagged `xai_responses`, a `/language-models`
//! discovery, and a stored-turn round trip replayed as the next request's `input` items.

mod common;

use std::collections::HashMap;
use std::time::Duration;

use agent_core::agent::context;
use agent_core::domain::{ContentBlock, Message, Role, StopReason};
use agent_core::providers::xai::{XAiConfig, XAiProvider, FAMILY};
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
) -> (XAiProvider, common::MockServer) {
    let server = common::serve_once(status, body, chunk, Duration::ZERO).await;
    let mut config = XAiConfig::new("x", format!("{}/v1", server.origin));
    config.api_key = Some("xai-test".into());
    (
        XAiProvider::new(config, agent_core::http::client().unwrap()),
        server,
    )
}

const TEXT_BODY: &[u8] = r#"data: {"type":"response.created","response":{"id":"resp_1","status":"in_progress"}}

data: {"type":"response.output_item.added","output_index":0,"item":{"id":"rs_1","type":"reasoning","summary":[]}}

data: {"type":"response.output_item.done","output_index":0,"item":{"id":"rs_1","type":"reasoning","status":"completed","summary":[],"encrypted_content":"XENC"}}

data: {"type":"response.output_item.added","output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","content":[]}}

data: {"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Hello → 世界"}

data: {"type":"response.output_item.done","output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello → 世界","annotations":[]}]}}

data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":12,"output_tokens":8}}}

data: [DONE]

"#
.as_bytes();

/// xAI streams data-only SSE terminated by `[DONE]`; summary-free reasoning items still
/// round-trip their encrypted content.
#[tokio::test]
async fn streams_identically_at_every_chunk_size() {
    for chunk in [1usize, 7, 64, 4096] {
        let (p, server) = provider("200 OK", TEXT_BODY, chunk).await;
        let c = common::collect(&p, &request("grok-4.7")).await;
        assert_eq!(c.err, None, "chunk={chunk}");
        assert_eq!(c.text, "Hello → 世界", "chunk={chunk}");
        assert_eq!(c.order, ["opaque", "text", "usage", "done"]);
        assert_eq!(c.opaque.len(), 1);
        assert_eq!(c.opaque[0].0, FAMILY);
        assert_eq!(c.opaque[0].1["item"]["encrypted_content"], "XENC");
        assert_eq!(c.done, Some(StopReason::EndTurn));

        let raw = server.request.await.unwrap();
        let head = raw.to_ascii_lowercase();
        assert!(head.starts_with("post /v1/responses "), "{raw}");
        assert!(head.contains("authorization: bearer xai-test"));
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
        br#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#,
        4096,
    )
    .await;
    let c = common::collect(&p, &request("grok-4.7")).await;
    let err = c.err.unwrap();
    assert!(
        err.contains("authentication failed") && err.contains("Incorrect API key"),
        "{err}"
    );
}

#[tokio::test]
async fn stream_errors_name_xai() {
    let (p, _server) = provider(
        "200 OK",
        b"data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\"}}\n\n",
        4096,
    )
    .await;
    let err = common::collect(&p, &request("grok-4.7")).await.err.unwrap();
    assert!(
        err.contains("xAI reported") && !err.contains("OpenAI"),
        "{err}"
    );
}

const TOOL_BODY: &[u8] = br#"data: {"type":"response.output_item.done","output_index":0,"item":{"id":"rs_1","type":"reasoning","summary":[],"encrypted_content":"ENC1"}}

data: {"type":"response.output_item.added","output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","content":[]}}

data: {"type":"response.output_text.delta","output_index":1,"delta":"Reading both."}

data: {"type":"response.output_item.done","output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"output_text","text":"Reading both."}]}}

data: {"type":"response.output_item.added","output_index":2,"item":{"id":"fc_1","type":"function_call","call_id":"call_a","name":"fs__read","arguments":""}}

data: {"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"path\":\"a.txt\"}"}

data: {"type":"response.output_item.done","output_index":2,"item":{"id":"fc_1","type":"function_call","call_id":"call_a","name":"fs__read","arguments":"{\"path\":\"a.txt\"}","status":"completed"}}

data: {"type":"response.output_item.done","output_index":3,"item":{"id":"fc_2","type":"function_call","call_id":"call_b","name":"fs__read","arguments":"{\"path\":\"b.txt\"}","status":"completed"}}

data: {"type":"response.completed","response":{"status":"completed"}}

data: [DONE]

"#;

/// Simulates the coordinator storing a step: opaque, text and calls in stream order, then
/// the tool results, and checks the next request replays it item for item.
#[tokio::test]
async fn tool_turn_round_trips_into_next_request_items() {
    let (p, _server) = provider("200 OK", TOOL_BODY, 5).await;
    let c = common::collect(&p, &request("grok-4.7")).await;
    assert_eq!(c.err, None);
    assert_eq!(c.done, Some(StopReason::ToolUse));
    assert_eq!(c.calls.len(), 2);

    let mut blocks = vec![
        ContentBlock::ProviderOpaque {
            provider: FAMILY.into(),
            data: c.opaque[0].1.clone(),
        },
        ContentBlock::Text {
            text: c.text.clone(),
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
    let body = XAiProvider::build_body(&CompletionRequest {
        messages,
        ..request("grok-4.7")
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
        json!({ "type": "reasoning", "summary": [], "encrypted_content": "ENC1" }),
        "reasoning item replayed verbatim without ids"
    );
    assert_eq!(input[3]["call_id"], "call_a");
    assert_eq!(input[5]["output"], "contents of call_a");
    assert!(
        input.iter().all(|i| i.get("id").is_none()),
        "no item ids under store:false"
    );
}

#[tokio::test]
async fn discovery_uses_language_models_listing() {
    let (p, server) = provider(
        "200 OK",
        br#"{"models":[{"id":"grok-4.7","input_modalities":["text","image"],"output_modalities":["text"],"capabilities":{"reasoning_effort":["low","medium","high"],"default_reasoning_effort":"high"}},{"id":"grok-3-mini","input_modalities":["text"],"output_modalities":["text"],"capabilities":{"reasoning_effort":["low","high"]}},{"id":"grok-imagine-image","input_modalities":["text"],"output_modalities":["image"],"image_price":200000000}]}"#,
        4096,
    )
    .await;
    let d = p.discover().await.unwrap();
    assert_eq!(d.models, ["grok-3-mini", "grok-4.7"]);
    assert_eq!(d.details["grok-4.7"].vision, Some(true));
    assert_eq!(d.details["grok-4.7"].reasoning, Some(true));
    assert_eq!(d.details["grok-4.7"].context_window, Some(500_000));
    let raw = server.request.await.unwrap();
    assert!(raw.starts_with("GET /v1/language-models "), "{raw}");
    assert!(raw
        .to_ascii_lowercase()
        .contains("authorization: bearer xai-test"));
}
