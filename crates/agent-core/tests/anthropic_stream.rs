//! End-to-end tests of the Anthropic Messages adapter against the in-process SSE server,
//! including a stored-turn round trip: what one response streams must replay in the next
//! request's `messages` at the same position — signed thinking blocks verbatim.

mod common;

use std::time::Duration;

use agent_core::domain::{ContentBlock, Message, Role, StopReason};
use agent_core::providers::anthropic::{AnthropicConfig, AnthropicProvider, FAMILY};
use agent_core::providers::{CompletionRequest, ToolSpec};
use serde_json::json;

fn request() -> CompletionRequest {
    CompletionRequest {
        model: "claude-opus-5-5".into(),
        system: Some("sys".into()),
        messages: vec![Message::text(Role::User, "hello")],
        max_output_tokens: Some(4096),
        tools: vec![ToolSpec {
            name: "fs__read".into(),
            description: "Read a file".into(),
            parameters: json!({ "type": "object" }),
        }],
    }
}

async fn provider(
    status: &'static str,
    body: &'static [u8],
    chunk: usize,
) -> (AnthropicProvider, common::MockServer) {
    let server = common::serve_once(status, body, chunk, Duration::ZERO).await;
    let mut config = AnthropicConfig::new("a", format!("{}/v1", server.origin));
    config.api_key = Some("test-key".into());
    (
        AnthropicProvider::new(config, agent_core::http::client().unwrap()),
        server,
    )
}

const TEXT_BODY: &[u8] = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-opus-5-5","content":[],"stop_reason":null,"usage":{"input_tokens":25,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Thinking about "}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"greetings"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQBCgIY"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hello → 世界"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":21}}

event: message_stop
data: {"type":"message_stop"}

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
        assert_eq!(
            c.opaque[0].1,
            json!({ "block": { "type": "thinking", "thinking": "", "signature": "EqQBCgIY" } }),
            "the stored block keeps only the signature; text lives in the Thinking block"
        );
        assert_eq!(c.done, Some(StopReason::EndTurn));
        let usage = c.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(25));
        assert_eq!(usage.output_tokens, Some(21));
        assert_eq!(usage.cache_read_tokens, Some(0));

        let raw = server.request.await.unwrap();
        let head = raw.to_ascii_lowercase();
        assert!(head.starts_with("post /v1/messages "), "{raw}");
        assert!(head.contains("x-api-key: test-key"));
        assert!(head.contains("anthropic-version: 2026-10-21"));
        assert!(head.contains("anthropic-beta: thinking-binding-controls"));
        assert!(!head.contains("key=test-key"), "key never in the URL");
        let body = common::request_json(&raw);
        assert_eq!(body["stream"], true);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["thinking"]["display"], "summarized");
        assert_eq!(
            body["thinking"]["block_binding"]["prefix_mismatch_behavior"],
            "drop_block"
        );
    }
}

const TOOL_BODY: &[u8] = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_2","type":"message","role":"assistant","model":"claude-opus-5-5","content":[],"usage":{"input_tokens":12,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Check the file first."}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-t"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"fs__read","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"a.txt\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}

event: message_stop
data: {"type":"message_stop"}

"#
.as_bytes();

const AFTER_TOOL: &[u8] = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_3","type":"message","role":"assistant","content":[],"usage":{"input_tokens":40,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"It says hello."}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9}}

event: message_stop
data: {"type":"message_stop"}

"#
.as_bytes();

#[tokio::test]
async fn tool_call_streams_and_replays_with_its_thinking_block() {
    let (origin, requests) = common::serve_many(vec![
        std::str::from_utf8(TOOL_BODY).unwrap(),
        std::str::from_utf8(AFTER_TOOL).unwrap(),
    ])
    .await;
    let mut config = AnthropicConfig::new("a", format!("{origin}/v1"));
    config.api_key = Some("test-key".into());
    let p = AnthropicProvider::new(config, agent_core::http::client().unwrap());

    let c = common::collect(&p, &request()).await;
    assert_eq!(c.err, None);
    assert_eq!(c.thinking, "Check the file first.");
    assert_eq!(c.calls.len(), 1);
    assert_eq!(c.calls[0].0, "toolu_1");
    assert_eq!(c.calls[0].1, "fs__read");
    assert_eq!(c.calls[0].2, r#"{"path":"a.txt"}"#);
    assert_eq!(c.done, Some(StopReason::ToolUse));
    assert_eq!(
        c.order,
        ["thinking", "opaque", "tool_call", "usage", "done"]
    );

    // What the coordinator stores, in stream order, plus the tool result — then the next
    // request must replay the signed thinking block verbatim at its position.
    let mut blocks = Vec::new();
    blocks.push(ContentBlock::ProviderOpaque {
        provider: FAMILY.into(),
        data: c.opaque[0].1.clone(),
    });
    let (id, name, arguments) = c.calls[0].clone();
    blocks.push(ContentBlock::ToolCall {
        id: id.clone(),
        name,
        arguments: serde_json::from_str(&arguments).unwrap(),
        server: None,
        tool: None,
    });
    let mut history = request().messages;
    history.push(Message {
        role: Role::Assistant,
        blocks,
    });
    history.push(Message {
        role: Role::Tool,
        blocks: vec![ContentBlock::ToolResult {
            call_id: id,
            content: json!([{ "type": "text", "text": "hello" }]),
            is_error: false,
            status: None,
        }],
    });
    let next = CompletionRequest {
        messages: history,
        ..request()
    };
    let c2 = common::collect(&p, &next).await;
    assert_eq!(c2.err, None);
    assert_eq!(c2.text, "It says hello.");
    assert_eq!(c2.done, Some(StopReason::EndTurn));

    let reqs = requests.await.unwrap();
    assert_eq!(reqs.len(), 2);
    let body = common::request_json(&reqs[1]);
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(
        msgs[1]["content"][0],
        json!({ "type": "thinking", "thinking": "", "signature": "sig-t" }),
        "signed thinking block replays verbatim"
    );
    assert_eq!(
        msgs[1]["content"][1],
        json!({ "type": "tool_use", "id": "toolu_1", "name": "fs__read",
                "input": { "path": "a.txt" } })
    );
    assert_eq!(msgs[2]["role"], "user");
    assert_eq!(
        msgs[2]["content"][0],
        json!({ "type": "tool_result", "tool_use_id": "toolu_1", "content": "hello" })
    );
}

#[tokio::test]
async fn refusal_stop_reason_surfaces_the_explanation() {
    const REFUSAL: &[u8] = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_4","type":"message","role":"assistant","content":[],"usage":{"input_tokens":9,"output_tokens":0}}}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber","explanation":"This request was declined because it could enable cyber harm."}},"usage":{"output_tokens":0}}

event: message_stop
data: {"type":"message_stop"}

"#
    .as_bytes();
    let (p, _server) = provider("200 OK", REFUSAL, 4096).await;
    let c = common::collect(&p, &request()).await;
    let err = c.err.unwrap();
    assert!(
        err.contains("declined") && err.contains("cyber harm"),
        "{err}"
    );
}

#[tokio::test]
async fn stream_error_event_and_early_eof_fail() {
    const ERR: &[u8] = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_5","type":"message","role":"assistant","content":[],"usage":{"input_tokens":9,"output_tokens":0}}}

event: error
data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}

"#
    .as_bytes();
    let (p, _s) = provider("200 OK", ERR, 4096).await;
    let c = common::collect(&p, &request()).await;
    assert!(c.err.unwrap().contains("Overloaded"));

    // A clean HTTP close without message_stop is an incomplete response.
    const TRUNCATED: &[u8] = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_6","type":"message","role":"assistant","content":[],"usage":{"input_tokens":9,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"half a"}}

"#
    .as_bytes();
    let (p, _s) = provider("200 OK", TRUNCATED, 4096).await;
    let c = common::collect(&p, &request()).await;
    assert!(c.err.unwrap().contains("stream ended"));
}

#[tokio::test]
async fn invalid_key_maps_to_auth_hint() {
    let (p, _server) = provider(
        "401 Unauthorized",
        br#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
        4096,
    )
    .await;
    let c = common::collect(&p, &request()).await;
    let err = c.err.unwrap();
    assert!(
        err.contains("authentication failed") && err.contains("invalid x-api-key"),
        "{err}"
    );
}

#[tokio::test]
async fn oversized_signature_drops_the_block_and_the_tail() {
    let sig = "s".repeat(70 * 1024);
    let body = format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"m\",\"role\":\"assistant\",\"content\":[],\"usage\":{{\"input_tokens\":1,\"output_tokens\":0}}}}}}\n\n\
         event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}}}\n\n\
         event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"signature_delta\",\"signature\":\"{sig}\"}}}}\n\n\
         event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
         event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":1,\"content_block\":{{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"sig2\"}}}}\n\n\
         event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":1}}\n\n\
         event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":3}}}}\n\n\
         event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
    );
    let body: &'static [u8] = Box::leak(body.into_bytes().into_boxed_slice());
    let (p, _s) = provider("200 OK", body, 4096).await;
    let c = common::collect(&p, &request()).await;
    assert_eq!(c.err, None);
    assert!(
        c.opaque.is_empty(),
        "an oversized block ends storage for the response, so no gap is replayed"
    );
    assert_eq!(c.done, Some(StopReason::EndTurn));
}
