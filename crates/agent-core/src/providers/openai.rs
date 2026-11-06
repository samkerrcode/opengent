//! OpenAI adapter for the Responses API (`POST {base}/responses`), authenticated with an
//! OpenAI API key.
//!
//! Requests are stateless (`store: false`): the full history is sent as an ordered list of
//! input items (`message`, `reasoning`, `function_call`, `function_call_output`), which maps
//! 1:1 onto stored content blocks. Reasoning models return their chain of thought as
//! encrypted `reasoning` items (`include: ["reasoning.encrypted_content"]`); replaying them
//! lets the model continue from its earlier reasoning across tool calls. That state rides in
//! `ProviderOpaque` blocks, at the position it was streamed:
//!
//! - `{"item": <reasoning item>}` — `summary` + `encrypted_content`, replayed verbatim. If it
//!   would exceed `MAX_OPAQUE_BYTES` the summary is emptied (the readable text is stored as
//!   the turn's `Thinking` block anyway); if still too large it is not stored — the model
//!   then simply reasons again.
//! - `{"phase": "commentary" | "final_answer"}` — the `phase` of the assistant message just
//!   before it, which newer models expect back on replayed messages.
//!
//! Item ids are never replayed: under `store: false` the server keeps nothing they could
//! reference, and omitting them is what OpenAI's own stateless clients do.

use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::capabilities::looks_like_vision_model;
use super::common::{
    arguments_string, extract_error_message, is_chat_model, map_transport, own_opaque, pump_sse,
    push_bounded, read_json, request_timeout, send_checked, spawn_stream, status_error, str_at,
    CallIds, StreamEnd, MAX_TOOL_ARG_CHARS,
};
use super::sse::SseEvent;
use super::{
    opaque_within_limit, Capabilities, CompletionRequest, DiscoveredModel, ModelDiscovery,
    ModelProvider, ProviderEvent, ProviderFuture, PROVIDER_EVENT_BUFFER,
};
use crate::domain::{tool_result_text, ContentBlock, Role, StopReason, Usage};
use crate::{Error, Result};

/// Opaque-state family tag. Stable: stored history references it.
pub const FAMILY: &str = "openai_responses";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
/// Bounds per-stream bookkeeping against a hostile server inventing output indices.
const MAX_OPEN_ITEMS: usize = 256;
/// Listed model families that are not text chat models on the Responses API.
const NON_CHAT_MARKERS: &[&str] = &[
    "embedding",
    "whisper",
    "tts",
    "dall-e",
    "gpt-image",
    "image",
    "sora",
    "audio",
    "realtime",
    "transcribe",
    "moderation",
    "search",
    "deep-research",
    "computer-use",
    "instruct",
    "babbage",
    "davinci",
];

#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    pub id: String,
    /// e.g. `https://api.openai.com/v1` — `/responses` and `/models` are appended.
    pub base_url: String,
    pub api_key: Option<String>,
    pub connect_timeout: Duration,
    /// Maximum silence between stream events. Generous: reasoning models can think for a
    /// long time before the first event when summaries are sparse.
    pub idle_timeout: Duration,
}

impl OpenAiConfig {
    pub fn new(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            api_key: None,
            connect_timeout: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(180),
        }
    }
}

pub struct OpenAiProvider {
    config: OpenAiConfig,
    http: reqwest::Client,
}

/// Bare model name: aggregator-style `openai/` prefixes stripped, lowercased.
fn model_name(model: &str) -> String {
    let lower = model.to_ascii_lowercase();
    lower
        .strip_prefix("openai/")
        .map(str::to_owned)
        .unwrap_or(lower)
}

/// Reasoning families accept `reasoning` options and return reasoning items; sending those
/// options to other models is rejected. `-chat` variants of GPT-5 are non-reasoning.
pub fn is_reasoning_model(model: &str) -> bool {
    let m = model_name(model);
    if m.contains("-chat") {
        return false;
    }
    ["o1", "o3", "o4", "gpt-5", "gpt-6"]
        .iter()
        .any(|p| m.starts_with(p))
        || m.contains("codex")
        || m.contains("reasoning")
}

/// Best-known input budget per family. The listing reports no limits, so without this every
/// OpenAI model would fall back to the 8K default; values are conservative input sizes and
/// the user's per-model setting always wins.
pub fn known_context_window(model: &str) -> Option<u32> {
    let m = model_name(model);
    let starts = |p: &str| m.starts_with(p);
    Some(if starts("gpt-4.1") {
        1_047_576
    } else if starts("gpt-6") {
        // 1.05M context window, 922K max input: the compiler's 75% input budget fits.
        1_050_000
    } else if starts("gpt-5") && m.contains("-chat") {
        128_000
    } else if starts("gpt-5") {
        272_000
    } else if starts("o1") || starts("o3") || starts("o4") {
        200_000
    } else if starts("gpt-4o") || starts("chatgpt-4o") || starts("gpt-4-turbo") || starts("gpt-4.5")
    {
        128_000
    } else if starts("gpt-3.5-turbo") {
        16_385
    } else if starts("gpt-4") {
        8_192
    } else {
        return None;
    })
}

/// Best-known output cap per family. The compiler budgets ~25% of the window for output,
/// which can exceed a model's real cap (a rejected `max_output_tokens`); clamp it here.
/// `None` = don't clamp (unknown families are left to the server).
pub fn known_max_output(model: &str) -> Option<u32> {
    let m = model_name(model);
    let starts = |p: &str| m.starts_with(p);
    Some(if starts("gpt-5") || starts("gpt-6") {
        128_000
    } else if starts("o3") || starts("o4") {
        100_000
    } else if starts("o1") {
        65_536 // o1-mini is the floor of the family
    } else if starts("gpt-4.1") {
        32_768
    } else if starts("gpt-4o") || starts("chatgpt-4o") {
        16_384
    } else if starts("gpt-4") {
        8_192
    } else if starts("gpt-3.5") {
        4_096
    } else {
        return None;
    })
}

/// Vision-capable OpenAI families (the small o-series minis are text-only).
fn accepts_images(model: &str) -> bool {
    let m = model_name(model);
    if (m.starts_with("o1-mini") || m.starts_with("o3-mini")) || m.starts_with("gpt-3.5") {
        return false;
    }
    looks_like_vision_model(&m) || ["o1", "o3", "gpt-6"].iter().any(|p| m.starts_with(p))
}

/// Parses a `GET /models` document into chat models with their known metadata.
pub fn parse_models(doc: &Value) -> Vec<(String, DiscoveredModel)> {
    let mut models: Vec<(String, DiscoveredModel)> = doc
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("id")?.as_str())
        .filter(|id| is_chat_model(id, NON_CHAT_MARKERS))
        .map(|id| {
            let details = DiscoveredModel {
                context_window: known_context_window(id),
                max_output_tokens: known_max_output(id),
                vision: Some(accepts_images(id)),
                reasoning: Some(is_reasoning_model(id)),
            };
            (id.to_owned(), details)
        })
        .collect();
    models.sort_by(|a, b| a.0.cmp(&b.0));
    models.dedup_by(|a, b| a.0 == b.0);
    models
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig, http: reqwest::Client) -> Self {
        Self { config, http }
    }

    /// The key travels in the `Authorization` header, never the URL.
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.config.base_url))
            .timeout(request_timeout(
                self.config.connect_timeout,
                self.config.idle_timeout,
            ));
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }
        req
    }

    /// Builds the `responses.create` body. Pure, so the item mapping is unit-tested.
    pub fn build_body(request: &CompletionRequest) -> Value {
        let mut input: Vec<Value> = Vec::new();
        for m in &request.messages {
            match m.role {
                Role::User | Role::System => {
                    let content: Vec<Value> = m
                        .blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } if !text.is_empty() => {
                                Some(json!({ "type": "input_text", "text": text }))
                            }
                            ContentBlock::Image { mime, data } => Some(json!({
                                "type": "input_image",
                                "image_url": format!("data:{mime};base64,{data}"),
                            })),
                            _ => None,
                        })
                        .collect();
                    if !content.is_empty() {
                        let role = if m.role == Role::System {
                            "system"
                        } else {
                            "user"
                        };
                        input.push(json!({ "type": "message", "role": role, "content": content }));
                    }
                }
                Role::Assistant => {
                    for b in &m.blocks {
                        match b {
                            ContentBlock::Text { text } if !text.trim().is_empty() => {
                                input.push(json!({
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{ "type": "output_text", "text": text }],
                                }));
                            }
                            ContentBlock::ToolCall {
                                id,
                                name,
                                arguments,
                                ..
                            } => input.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": arguments_string(arguments),
                            })),
                            b => {
                                let Some(data) = own_opaque(b, FAMILY) else {
                                    continue;
                                };
                                if let Some(item) = data
                                    .get("item")
                                    .filter(|i| i.is_object() && str_at(i, "type") == "reasoning")
                                {
                                    input.push(item.clone());
                                } else if let Some(phase) = data.get("phase") {
                                    // Belongs to the assistant message streamed just before.
                                    if let Some(last) = input.last_mut().filter(|l| {
                                        str_at(l, "type") == "message"
                                            && str_at(l, "role") == "assistant"
                                    }) {
                                        last["phase"] = phase.clone();
                                    }
                                }
                            }
                        }
                    }
                }
                Role::Tool => {
                    for b in &m.blocks {
                        if let ContentBlock::ToolResult {
                            call_id, content, ..
                        } = b
                        {
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": call_id,
                                "output": tool_result_text(content),
                            }));
                        }
                    }
                }
            }
        }

        let mut body = json!({
            "model": request.model,
            "input": input,
            "store": false,
            "stream": true,
        });
        if let Some(system) = request.system.as_deref().filter(|s| !s.is_empty()) {
            body["instructions"] = json!(system);
        }
        if is_reasoning_model(&request.model) {
            body["reasoning"] = json!({ "summary": "auto" });
            body["include"] = json!(["reasoning.encrypted_content"]);
        }
        if let Some(max) = request.max_output_tokens {
            let max = known_max_output(&request.model).map_or(max, |cap| max.min(cap));
            body["max_output_tokens"] = json!(max);
        }
        if !request.tools.is_empty() {
            body["tools"] = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                        // Responses defaults to strict schemas, which MCP tool schemas
                        // (optional fields, open objects) generally don't satisfy.
                        "strict": false,
                    })
                })
                .collect();
        }
        body
    }
}

impl ModelProvider for OpenAiProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn capabilities(&self) -> Capabilities {
        super::registry::ProviderKind::OpenAi.capabilities()
    }

    fn family(&self) -> Option<&'static str> {
        Some(FAMILY)
    }

    fn stream(
        &self,
        request: &CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ProviderEvent>> {
        let (tx, rx) = mpsc::channel(PROVIDER_EVENT_BUFFER);
        let req = self
            .request(reqwest::Method::POST, "/responses")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&Self::build_body(request));
        let config = self.config.clone();
        spawn_stream(tx.clone(), cancel.clone(), async move {
            let resp = send_checked(req, &config.base_url, config.connect_timeout).await?;
            let mut state = StreamState::default();
            let end = pump_sse(resp, config.idle_timeout, &cancel, &tx, |ev, out| {
                state.handle(ev, out)
            })
            .await?;
            if end == StreamEnd::Eof && !state.finished {
                return Err(Error::Provider(
                    "stream ended before the response completed".into(),
                ));
            }
            Ok(())
        });
        Ok(rx)
    }

    fn discover(&self) -> ProviderFuture<'_, ModelDiscovery> {
        Box::pin(async move {
            let resp = self
                .request(reqwest::Method::GET, "/models")
                .send()
                .await
                .map_err(|e| map_transport(&self.config.base_url, e))?;
            if !resp.status().is_success() {
                return Err(status_error(resp).await);
            }
            let models = parse_models(&read_json(resp, "/models").await?);
            Ok(ModelDiscovery {
                models: models.iter().map(|(id, _)| id.clone()).collect(),
                details: models.into_iter().collect(),
            })
        })
    }
}

/// An output item between `output_item.added` and `output_item.done`.
enum OpenItem {
    /// Arguments accumulated from deltas, used when the final item omits them.
    Call {
        arguments: String,
    },
    /// Summary parts seen so far, to separate them in the Thinking block.
    Reasoning {
        parts: usize,
    },
    Other,
}

/// Responses SSE state machine. Calls and reasoning items are emitted when their item is
/// done, so stored blocks keep the exact order the server produced them in.
#[derive(Default)]
struct StreamState {
    open: std::collections::BTreeMap<u64, OpenItem>,
    call_ids: CallIds,
    /// A terminal event was handled (`Done` or an error was produced).
    finished: bool,
}

impl StreamState {
    fn handle(&mut self, ev: SseEvent, out: &mut Vec<ProviderEvent>) -> Result<()> {
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
            return Ok(()); // keep-alives and anything we don't model
        };
        if self.finished {
            return Ok(());
        }
        let kind = v
            .get("type")
            .and_then(Value::as_str)
            .or(ev.event.as_deref())
            .unwrap_or("");
        let index = v.get("output_index").and_then(Value::as_u64).unwrap_or(0);
        match kind {
            "response.output_item.added" => {
                if self.open.len() < MAX_OPEN_ITEMS {
                    let item = v.get("item").unwrap_or(&Value::Null);
                    let open = match str_at(item, "type") {
                        "function_call" => OpenItem::Call {
                            arguments: String::new(),
                        },
                        "reasoning" => OpenItem::Reasoning { parts: 0 },
                        _ => OpenItem::Other,
                    };
                    self.open.insert(index, open);
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let delta = str_at(&v, "delta");
                if !delta.is_empty() {
                    out.push(ProviderEvent::TextDelta(delta.to_owned()));
                }
            }
            "response.reasoning_summary_part.added" => {
                if let Some(OpenItem::Reasoning { parts }) = self.open.get_mut(&index) {
                    if *parts > 0 {
                        out.push(ProviderEvent::ReasoningDelta("\n\n".into()));
                    }
                    *parts += 1;
                }
            }
            "response.reasoning_summary_text.delta" => {
                let delta = str_at(&v, "delta");
                if !delta.is_empty() {
                    out.push(ProviderEvent::ReasoningDelta(delta.to_owned()));
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(OpenItem::Call { arguments }) = self.open.get_mut(&index) {
                    push_bounded(arguments, str_at(&v, "delta"), MAX_TOOL_ARG_CHARS);
                }
            }
            "response.output_item.done" => {
                let open = self.open.remove(&index);
                self.close(v.get("item").unwrap_or(&Value::Null), open, out);
            }
            "response.completed" | "response.incomplete" => {
                let response = v.get("response").unwrap_or(&Value::Null);
                if let Some(u) = response.get("usage") {
                    out.push(ProviderEvent::Usage(parse_usage(u)));
                }
                let reason = if kind == "response.completed" {
                    if self.call_ids.count() > 0 {
                        StopReason::ToolUse
                    } else {
                        StopReason::EndTurn
                    }
                } else {
                    match str_at(
                        response.get("incomplete_details").unwrap_or(&Value::Null),
                        "reason",
                    ) {
                        "content_filter" => {
                            self.finished = true;
                            return Err(Error::Provider(
                                "the response was stopped by OpenAI's content filter".into(),
                            ));
                        }
                        _ => StopReason::MaxTokens,
                    }
                };
                out.push(ProviderEvent::Done(reason));
                self.finished = true;
            }
            "response.failed" => {
                self.finished = true;
                let response = v.get("response").unwrap_or(&Value::Null);
                return Err(Error::Provider(
                    extract_error_message(response)
                        .unwrap_or_else(|| "OpenAI reported the response failed".into()),
                ));
            }
            "error" => {
                self.finished = true;
                return Err(Error::Provider(
                    extract_error_message(&v).unwrap_or_else(|| "OpenAI stream error".into()),
                ));
            }
            // Lifecycle (`created`, `in_progress`), per-part bookkeeping, `*.done` echoes of
            // already-streamed text, and anything newer than this adapter.
            _ => {}
        }
        Ok(())
    }

    fn close(&mut self, item: &Value, open: Option<OpenItem>, out: &mut Vec<ProviderEvent>) {
        match str_at(item, "type") {
            "function_call" => {
                let name = str_at(item, "name");
                // A call cut off by the output limit has truncated arguments: never run it.
                if name.is_empty() || str_at(item, "status") == "incomplete" {
                    return;
                }
                let mut arguments = String::new();
                match (str_at(item, "arguments"), open) {
                    (
                        "",
                        Some(OpenItem::Call {
                            arguments: streamed,
                        }),
                    ) => arguments = streamed,
                    (raw, _) => push_bounded(&mut arguments, raw, MAX_TOOL_ARG_CHARS),
                }
                out.push(ProviderEvent::ToolCall {
                    id: self.call_ids.unique(str_at(item, "call_id").to_owned()),
                    name: name.to_owned(),
                    arguments: if arguments.trim().is_empty() {
                        "{}".into()
                    } else {
                        arguments
                    },
                });
            }
            "reasoning" => {
                if let Some(data) = reasoning_state(item) {
                    out.push(ProviderEvent::Opaque {
                        provider: FAMILY,
                        data,
                    });
                }
            }
            "message" => {
                if let Some(phase) = item.get("phase").filter(|p| p.is_string()) {
                    out.push(ProviderEvent::Opaque {
                        provider: FAMILY,
                        data: json!({ "phase": phase }),
                    });
                }
            }
            _ => {}
        }
    }
}

/// The replayable form of a finished reasoning item: `type`, `summary` and
/// `encrypted_content` only (ids and status refer to server state that `store: false` never
/// creates). `None` when there is nothing to replay or it cannot fit the opaque cap.
fn reasoning_state(item: &Value) -> Option<Value> {
    let encrypted = item.get("encrypted_content").filter(|e| e.is_string())?;
    let mut lean = Map::new();
    lean.insert("type".into(), json!("reasoning"));
    lean.insert("summary".into(), json!([]));
    lean.insert("encrypted_content".into(), encrypted.clone());
    let lean = json!({ "item": lean });
    let summary = item
        .get("summary")
        .filter(|s| s.as_array().is_some_and(|a| !a.is_empty()));
    if let Some(summary) = summary {
        let mut full = lean.clone();
        full["item"]["summary"] = summary.clone();
        if opaque_within_limit(&full) {
            return Some(full);
        }
    }
    opaque_within_limit(&lean).then_some(lean)
}

/// `output_tokens` already includes reasoning tokens.
fn parse_usage(u: &Value) -> Usage {
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64);
    Usage {
        input_tokens: n(u, "input_tokens"),
        output_tokens: n(u, "output_tokens"),
        cache_read_tokens: u
            .get("input_tokens_details")
            .and_then(|d| n(d, "cached_tokens")),
        cache_write_tokens: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Message;
    use crate::providers::ToolSpec;

    fn opaque(data: Value) -> ContentBlock {
        ContentBlock::ProviderOpaque {
            provider: FAMILY.into(),
            data,
        }
    }

    fn call(id: &str, name: &str, args: Value) -> ContentBlock {
        ContentBlock::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args,
            server: None,
            tool: None,
        }
    }

    fn request(model: &str, messages: Vec<Message>) -> CompletionRequest {
        CompletionRequest {
            model: model.into(),
            system: Some("sys".into()),
            messages,
            max_output_tokens: Some(256),
            tools: vec![ToolSpec {
                name: "fs__read".into(),
                description: "Read a file".into(),
                parameters: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            }],
        }
    }

    #[test]
    fn body_shape_and_item_sequence() {
        let reasoning = json!({ "type": "reasoning", "summary": [{ "type": "summary_text", "text": "plan" }], "encrypted_content": "ENC" });
        let messages = vec![
            Message {
                role: Role::User,
                blocks: vec![
                    ContentBlock::Text {
                        text: "read a.txt".into(),
                    },
                    ContentBlock::Image {
                        mime: "image/png".into(),
                        data: "aGk=".into(),
                    },
                ],
            },
            Message {
                role: Role::Assistant,
                blocks: vec![
                    opaque(json!({ "item": reasoning.clone() })),
                    ContentBlock::Text {
                        text: "Reading.".into(),
                    },
                    opaque(json!({ "phase": "commentary" })),
                    call("c1", "fs__read", json!({ "path": "a.txt" })),
                    call("c2", "fs__read", Value::String("not json".into())),
                    ContentBlock::ProviderOpaque {
                        provider: "google_gemini".into(),
                        data: json!({ "item": { "type": "reasoning" } }),
                    },
                ],
            },
            Message {
                role: Role::Tool,
                blocks: vec![
                    ContentBlock::ToolResult {
                        call_id: "c1".into(),
                        content: json!([{ "type": "text", "text": "hello" }]),
                        is_error: false,
                        status: None,
                    },
                    ContentBlock::ToolResult {
                        call_id: "c2".into(),
                        content: json!("boom"),
                        is_error: true,
                        status: None,
                    },
                ],
            },
        ];
        let body = OpenAiProvider::build_body(&request("gpt-5.2", messages));
        assert_eq!(body["model"], "gpt-5.2");
        assert_eq!(body["instructions"], "sys");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_output_tokens"], 256);
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(
            body["tools"][0],
            json!({ "type": "function", "name": "fs__read", "description": "Read a file",
                    "parameters": { "type": "object", "properties": { "path": { "type": "string" } } },
                    "strict": false })
        );
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
            ],
            "foreign opaque state is dropped; own state replays in place"
        );
        assert_eq!(input[0]["role"], "user");
        assert_eq!(
            input[0]["content"][1],
            json!({ "type": "input_image", "image_url": "data:image/png;base64,aGk=" })
        );
        assert_eq!(input[1], reasoning, "reasoning item replayed verbatim");
        assert_eq!(
            input[2],
            json!({ "type": "message", "role": "assistant", "phase": "commentary",
                    "content": [{ "type": "output_text", "text": "Reading." }] })
        );
        assert_eq!(
            input[3],
            json!({ "type": "function_call", "call_id": "c1", "name": "fs__read",
                    "arguments": r#"{"path":"a.txt"}"# })
        );
        assert_eq!(input[4]["arguments"], "not json", "raw text passes through");
        assert_eq!(
            input[5],
            json!({ "type": "function_call_output", "call_id": "c1", "output": "hello" })
        );
        assert_eq!(input[6]["output"], "boom");
    }

    #[test]
    fn non_reasoning_models_get_no_reasoning_options() {
        let mut req = request("gpt-4.1-mini", vec![Message::text(Role::User, "hi")]);
        req.system = None;
        req.tools.clear();
        req.max_output_tokens = None;
        let body = OpenAiProvider::build_body(&req);
        for key in [
            "instructions",
            "tools",
            "reasoning",
            "include",
            "max_output_tokens",
        ] {
            assert!(body.get(key).is_none(), "{key}: {body}");
        }
        assert!(
            OpenAiProvider::build_body(&request("gpt-5-chat-latest", vec![]))
                .get("reasoning")
                .is_none()
        );
    }

    #[test]
    fn model_families() {
        for m in [
            "o3",
            "o4-mini",
            "gpt-5",
            "gpt-5.2-pro",
            "openai/gpt-6-astra",
            "gpt-5.1-codex",
        ] {
            assert!(is_reasoning_model(m), "{m}");
        }
        for m in [
            "gpt-4o",
            "gpt-4.1",
            "gpt-5-chat-latest",
            "chatgpt-4o-latest",
        ] {
            assert!(!is_reasoning_model(m), "{m}");
        }
        assert_eq!(known_context_window("gpt-4.1-nano"), Some(1_047_576));
        for m in ["gpt-6-astra", "gpt-6.1-sol", "gpt-6.1-terra", "gpt-6-luna"] {
            assert_eq!(known_context_window(m), Some(1_050_000), "{m}");
        }
        assert_eq!(known_context_window("gpt-5-mini"), Some(272_000));
        assert_eq!(known_context_window("gpt-5.2-chat-latest"), Some(128_000));
        assert_eq!(known_context_window("o4-mini"), Some(200_000));
        assert_eq!(known_context_window("gpt-4o-mini"), Some(128_000));
        assert_eq!(known_context_window("gpt-4-0613"), Some(8_192));
        assert_eq!(known_context_window("mystery"), None);
        assert!(accepts_images("gpt-5") && accepts_images("o3") && accepts_images("gpt-4o"));
        assert!(accepts_images("gpt-6.1-terra"));
        assert!(!accepts_images("o3-mini") && !accepts_images("gpt-3.5-turbo"));
    }

    #[test]
    fn output_cap_is_clamped_to_the_model() {
        let mut req = request("gpt-4o", vec![Message::text(Role::User, "hi")]);
        req.max_output_tokens = Some(262_144); // the compiler's 25%-of-1M budget
        assert_eq!(
            OpenAiProvider::build_body(&req)["max_output_tokens"],
            16_384
        );
        req.model = "gpt-6-astra".into();
        assert_eq!(
            OpenAiProvider::build_body(&req)["max_output_tokens"],
            128_000
        );
        req.model = "mystery".into();
        assert_eq!(
            OpenAiProvider::build_body(&req)["max_output_tokens"],
            262_144
        );
        req.model = "gpt-5".into();
        req.max_output_tokens = Some(4_096);
        assert_eq!(OpenAiProvider::build_body(&req)["max_output_tokens"], 4_096);
    }

    #[test]
    fn listing_filters_non_chat_models() {
        let doc = json!({ "object": "list", "data": [
            { "id": "gpt-5", "object": "model" },
            { "id": "text-embedding-3-large" },
            { "id": "gpt-4o-mini-tts" },
            { "id": "gpt-image-1" },
            { "id": "whisper-1" },
            { "id": "gpt-4o-realtime-preview" },
            { "id": "omni-moderation-latest" },
            { "id": "gpt-3.5-turbo-instruct" },
            { "id": "gpt-4.1-mini" },
            { "id": "gpt-5" }
        ]});
        let models = parse_models(&doc);
        let ids: Vec<&str> = models.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["gpt-4.1-mini", "gpt-5"]);
        assert_eq!(models[1].1.reasoning, Some(true));
        assert_eq!(models[1].1.vision, Some(true));
        assert_eq!(models[0].1.context_window, Some(1_047_576));
    }

    fn sse(data: Value) -> SseEvent {
        SseEvent {
            event: data["type"].as_str().map(str::to_owned),
            data: data.to_string(),
        }
    }

    fn run(events: Vec<Value>) -> (Vec<ProviderEvent>, Result<()>, bool) {
        let mut state = StreamState::default();
        let mut out = Vec::new();
        for ev in events {
            if let Err(e) = state.handle(sse(ev), &mut out) {
                return (out, Err(e), state.finished);
            }
        }
        (out, Ok(()), state.finished)
    }

    fn completed() -> Value {
        json!({ "type": "response.completed", "response": { "status": "completed", "usage": {
            "input_tokens": 10, "input_tokens_details": { "cached_tokens": 4 },
            "output_tokens": 12, "output_tokens_details": { "reasoning_tokens": 7 } } } })
    }

    #[test]
    fn reasoning_text_and_usage_stream_in_order() {
        let (out, res, finished) = run(vec![
            json!({ "type": "response.created", "response": { "status": "in_progress" } }),
            json!({ "type": "response.output_item.added", "output_index": 0,
                    "item": { "type": "reasoning", "id": "rs_1", "summary": [] } }),
            json!({ "type": "response.reasoning_summary_part.added", "output_index": 0, "summary_index": 0 }),
            json!({ "type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "**Plan**" }),
            json!({ "type": "response.reasoning_summary_part.added", "output_index": 0, "summary_index": 1 }),
            json!({ "type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "Then answer." }),
            json!({ "type": "response.output_item.done", "output_index": 0, "item": {
                "type": "reasoning", "id": "rs_1", "status": "completed", "encrypted_content": "ENC",
                "summary": [{ "type": "summary_text", "text": "**Plan**" }, { "type": "summary_text", "text": "Then answer." }] } }),
            json!({ "type": "response.output_item.added", "output_index": 1,
                    "item": { "type": "message", "id": "msg_1", "role": "assistant", "content": [] } }),
            json!({ "type": "response.output_text.delta", "output_index": 1, "delta": "Hel" }),
            json!({ "type": "response.output_text.delta", "output_index": 1, "delta": "lo → 世界" }),
            json!({ "type": "response.output_text.done", "output_index": 1, "text": "Hello → 世界" }),
            json!({ "type": "response.output_item.done", "output_index": 1, "item": {
                "type": "message", "id": "msg_1", "role": "assistant", "phase": "final_answer",
                "content": [{ "type": "output_text", "text": "Hello → 世界" }] } }),
            completed(),
        ]);
        res.unwrap();
        assert!(finished);
        assert_eq!(
            out,
            vec![
                ProviderEvent::ReasoningDelta("**Plan**".into()),
                ProviderEvent::ReasoningDelta("\n\n".into()),
                ProviderEvent::ReasoningDelta("Then answer.".into()),
                ProviderEvent::Opaque {
                    provider: FAMILY,
                    data: json!({ "item": { "type": "reasoning", "encrypted_content": "ENC", "summary": [
                        { "type": "summary_text", "text": "**Plan**" },
                        { "type": "summary_text", "text": "Then answer." }
                    ] } }),
                },
                ProviderEvent::TextDelta("Hel".into()),
                ProviderEvent::TextDelta("lo → 世界".into()),
                ProviderEvent::Opaque {
                    provider: FAMILY,
                    data: json!({ "phase": "final_answer" }),
                },
                ProviderEvent::Usage(Usage {
                    input_tokens: Some(10),
                    output_tokens: Some(12),
                    cache_read_tokens: Some(4),
                    cache_write_tokens: None,
                }),
                ProviderEvent::Done(StopReason::EndTurn),
            ]
        );
    }

    #[test]
    fn calls_are_emitted_in_order_with_unique_ids() {
        let (out, res, _) = run(vec![
            json!({ "type": "response.output_item.added", "output_index": 0,
                    "item": { "type": "function_call", "call_id": "a", "name": "fs__read", "arguments": "" } }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 0, "delta": "{\"path\":" }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 0, "delta": "\"a\"}" }),
            // Final item without arguments: the streamed ones are used.
            json!({ "type": "response.output_item.done", "output_index": 0,
                    "item": { "type": "function_call", "call_id": "a", "name": "fs__read", "status": "completed" } }),
            json!({ "type": "response.output_item.done", "output_index": 1,
                    "item": { "type": "function_call", "call_id": "a", "name": "time__now", "arguments": "{\"tz\":\"UTC\"}" } }),
            json!({ "type": "response.output_item.done", "output_index": 2,
                    "item": { "type": "function_call", "call_id": "z", "name": "x", "arguments": "{\"tru", "status": "incomplete" } }),
            json!({ "type": "response.output_item.done", "output_index": 3,
                    "item": { "type": "reasoning", "summary": [] } }),
            completed(),
        ]);
        res.unwrap();
        let calls: Vec<(&str, &str, &str)> = out
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } => Some((id.as_str(), name.as_str(), arguments.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 2, "incomplete call dropped: {out:?}");
        assert_eq!(calls[0], ("a", "fs__read", r#"{"path":"a"}"#));
        assert_eq!(calls[1].1, "time__now");
        assert_ne!(calls[1].0, "a", "duplicate id replaced");
        assert!(
            !out.iter()
                .any(|e| matches!(e, ProviderEvent::Opaque { .. })),
            "reasoning without encrypted content is not stored"
        );
        assert_eq!(out.last(), Some(&ProviderEvent::Done(StopReason::ToolUse)));
    }

    #[test]
    fn terminal_statuses() {
        let (out, res, _) = run(vec![json!({ "type": "response.incomplete", "response": {
            "status": "incomplete", "incomplete_details": { "reason": "max_output_tokens" } } })]);
        res.unwrap();
        assert_eq!(
            out.last(),
            Some(&ProviderEvent::Done(StopReason::MaxTokens))
        );

        let (_, res, _) = run(vec![json!({ "type": "response.incomplete", "response": {
            "incomplete_details": { "reason": "content_filter" } } })]);
        assert!(res.unwrap_err().to_string().contains("content filter"));

        let (_, res, _) = run(vec![json!({ "type": "response.failed", "response": {
            "status": "failed", "error": { "code": "server_error", "message": "upstream broke" } } })]);
        assert!(res.unwrap_err().to_string().contains("upstream broke"));

        let (_, res, _) = run(vec![
            json!({ "type": "error", "code": "rate_limit_exceeded",
            "message": "slow down", "param": null }),
        ]);
        assert!(res.unwrap_err().to_string().contains("slow down"));

        let (_, res, finished) = run(vec![
            json!({ "type": "response.output_text.delta", "output_index": 0, "delta": "partial" }),
        ]);
        res.unwrap();
        assert!(
            !finished,
            "no terminal event: the driver reports truncation"
        );
    }

    #[test]
    fn oversized_reasoning_sheds_summary_then_block() {
        let big = "x".repeat(super::super::MAX_OPAQUE_BYTES);
        let item = json!({ "type": "reasoning", "encrypted_content": "ENC",
            "summary": [{ "type": "summary_text", "text": big }] });
        assert_eq!(
            reasoning_state(&item),
            Some(
                json!({ "item": { "type": "reasoning", "summary": [], "encrypted_content": "ENC" } })
            )
        );
        let huge = json!({ "type": "reasoning", "encrypted_content": big, "summary": [] });
        assert_eq!(reasoning_state(&huge), None);
    }
}
