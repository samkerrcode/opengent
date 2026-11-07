//! Anthropic adapter for the Messages API (`POST {base}/messages`), authenticated with an
//! Anthropic API key sent in the `x-api-key` header (never the URL).
//!
//! Requests are stateless: the full history is sent each turn as alternating
//! `user`/`assistant` messages, which maps onto stored content blocks with two folds —
//! `Role::Tool` messages become `tool_result` blocks inside a `user` message (there is no
//! tool role), and consecutive same-role messages merge because the API requires strict
//! alternation.
//!
//! Claude signs its reasoning into `thinking` blocks whose `signature` must be replayed
//! unchanged, at the same position, in an unbroken sequence. Each signed block rides in a
//! `ProviderOpaque` block as `{"block": <content block>}` and is re-inserted verbatim when
//! history is rebuilt. The stored form keeps only `type` and `signature`: the readable text
//! already lives in the turn's `Thinking` block, the signature is the same whatever the
//! `display` setting produced it, and the server ignores the `thinking` field of a
//! round-tripped block. `redacted_thinking` blocks store their `data` field the same way.
//! A block that cannot fit `MAX_OPAQUE_BYTES` ends signature storage for that response —
//! a gap in the sequence would invalidate every later block anyway.
//!
//! Newer models (the Claude 5.1 generation on) additionally bind each thinking block to
//! the request prefix it was produced under — system prompt, tools, and every earlier
//! message — and fail the request when history was edited (trimmed turns, shortened tool
//! results, a changed tool set). Requests therefore send
//! `thinking.block_binding.prefix_mismatch_behavior: "drop_block"` so a stale prefix costs
//! the affected reasoning instead of the turn, and log what was dropped. Making the
//! compiled history append-only is the lasting fix; it is Phase 6 work.
//!
//! Thinking is configured per model: Claude 4.6+ and every 5.x model accept
//! `thinking: {"type": "adaptive"}` (the model decides when and how much to think, and
//! interleaves reasoning between tool calls); `display: "summarized"` asks for the readable
//! summary the Thinking panel shows. The manual `type: "enabled"` + `budget_tokens` mode
//! is deprecated on 4.6 and rejected by 4.7+, so older models get no thinking config and
//! simply answer directly.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::common::{
    arguments_object, extract_error_message, map_transport, own_opaque as opaque_of, pump_sse,
    push_bounded, read_json, secret_header, send_checked, spawn_stream, status_error, str_at,
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
pub const FAMILY: &str = "anthropic";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";
/// Pinned protocol revision. This version rejects `temperature`/`top_p`/`top_k`, which the
/// adapter never sends.
const API_VERSION: &str = "2026-10-21";
/// Lets thinking requests carry `block_binding`, which keeps a stale request prefix from
/// failing the whole turn on models that run the thinking prefix check.
const BETA_FLAGS: &str = "thinking-binding-controls-2026-08-01";
/// Listing pages are large (`limit=1000`); this only bounds a misbehaving server.
const MAX_LISTING_PAGES: usize = 10;
/// Bounds per-stream bookkeeping against a hostile server inventing block indices.
const MAX_OPEN_BLOCKS: usize = 256;
/// `max_tokens` is required; the compiler always sets it, so this is only a fallback.
const FALLBACK_MAX_TOKENS: u32 = 8_192;

#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub id: String,
    /// e.g. `https://api.anthropic.com/v1` — `/messages` and `/models` are appended.
    pub base_url: String,
    pub api_key: Option<String>,
    pub connect_timeout: Duration,
    /// Maximum silence between stream events. Generous: deep thinking can pause the stream
    /// for a long time before the first block arrives.
    pub idle_timeout: Duration,
}

impl AnthropicConfig {
    pub fn new(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            api_key: None,
            connect_timeout: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(180),
        }
    }

    fn request_timeout(&self) -> Duration {
        super::common::request_timeout(self.connect_timeout, self.idle_timeout)
    }
}

pub struct AnthropicProvider {
    config: AnthropicConfig,
    http: reqwest::Client,
}

/// Bare model name: aggregator-style `anthropic/` prefixes stripped, lowercased.
fn model_name(model: &str) -> String {
    let lower = model.to_ascii_lowercase();
    lower
        .strip_prefix("anthropic/")
        .map(str::to_owned)
        .unwrap_or(lower)
}

/// Whether the model accepts `thinking: {"type": "adaptive"}`. Claude 4.6 and every later
/// model does; the 4.5 generation and older only support the deprecated manual mode (or no
/// thinking), so they get no `thinking` config. Unknown names are treated as current so
/// models newer than this list still get thinking.
pub fn supports_adaptive_thinking(model: &str) -> bool {
    let m = model_name(model);
    !m.starts_with("claude") || version(&m).is_none_or(|v| v >= (4, 6))
}

/// `(major, minor)` of a bare Claude name in either scheme — `claude-3-5-sonnet-…`,
/// `claude-sonnet-4-5-…`, `claude-2.1`, `claude-opus-4-6@…`. Date suffixes are 8-digit
/// tokens, so only 1–2 digit tokens count. `None` when the name carries no version.
fn version(m: &str) -> Option<(u32, u32)> {
    let num = |t: &str| {
        (!t.is_empty() && t.len() <= 2 && t.bytes().all(|b| b.is_ascii_digit()))
            .then(|| t.parse().ok())
            .flatten()
    };
    let mut tokens = m.split(['-', '.', '@']).skip(1);
    let major = tokens.by_ref().find_map(num)?;
    Some((major, tokens.next().and_then(num).unwrap_or(0)))
}

/// Best-known output cap per generation. The compiler's ~25%-of-window budget can exceed a
/// model's real cap (a rejected `max_tokens`); clamp it here. Discovery reports the exact
/// `max_tokens` per model, which the compiler prefers; this is the fallback for models it
/// never listed. `None` = don't clamp (unknown names are left to the server).
pub fn known_max_output(model: &str) -> Option<u32> {
    let m = model_name(model);
    if !m.starts_with("claude") {
        return None;
    }
    Some(match version(&m) {
        _ if supports_adaptive_thinking(model) => 128_000, // Claude 4.6 and later
        Some((3, 7)) => 64_000,
        Some((major, _)) if major < 4 => 4_096, // the floor of the 1.x–3.x caps
        _ => 64_000,                            // Claude 4.0–4.5
    })
}

fn build_request(
    config: &AnthropicConfig,
    http: &reqwest::Client,
    method: reqwest::Method,
    url: String,
    timeout: Duration,
) -> reqwest::RequestBuilder {
    let mut req = http
        .request(method, url)
        .timeout(timeout)
        .header("anthropic-version", API_VERSION)
        .header("anthropic-beta", BETA_FLAGS);
    if let Some(key) = &config.api_key {
        req = secret_header(req, "x-api-key", key);
    }
    req
}

/// Flushes the pending message when the role changes and starts a new one. Same-role
/// messages merge into one content array (the API requires alternation, and `Role::Tool`
/// results must share a `user` message with the text that follows them).
fn flush_role(
    messages: &mut Vec<Value>,
    role: &mut &'static str,
    content: &mut Vec<Value>,
    next: &'static str,
) {
    if *role != next && !content.is_empty() {
        messages.push(json!({ "role": *role, "content": std::mem::take(content) }));
    }
    *role = next;
}

/// The payload of an opaque block this adapter produced.
fn own_opaque(b: &ContentBlock) -> Option<&Value> {
    opaque_of(b, FAMILY)
}

impl AnthropicProvider {
    pub fn new(config: AnthropicConfig, http: reqwest::Client) -> Self {
        Self { config, http }
    }

    fn request(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        build_request(
            &self.config,
            &self.http,
            method,
            url,
            self.config.request_timeout(),
        )
    }

    /// Builds the `messages.create` body. Pure, so the block mapping is unit-tested.
    pub fn build_body(request: &CompletionRequest) -> Value {
        let mut messages: Vec<Value> = Vec::new();
        let mut role = "";
        let mut content: Vec<Value> = Vec::new();
        for m in &request.messages {
            match m.role {
                Role::User | Role::System => {
                    flush_role(&mut messages, &mut role, &mut content, "user");
                    for b in &m.blocks {
                        match b {
                            ContentBlock::Text { text } if !text.is_empty() => {
                                content.push(json!({ "type": "text", "text": text }));
                            }
                            ContentBlock::Image { mime, data } => content.push(json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": mime,
                                    "data": data,
                                },
                            })),
                            _ => {}
                        }
                    }
                }
                Role::Assistant => {
                    flush_role(&mut messages, &mut role, &mut content, "assistant");
                    for b in &m.blocks {
                        match b {
                            ContentBlock::Text { text } if !text.trim().is_empty() => {
                                content.push(json!({ "type": "text", "text": text }));
                            }
                            ContentBlock::ToolCall {
                                id,
                                name,
                                arguments,
                                ..
                            } => content.push(json!({
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": arguments_object(arguments),
                            })),
                            b => {
                                // A signed thinking/redacted_thinking block, replayed
                                // verbatim at its position. Only those two types: stored
                                // state must never inject other content into the request.
                                if let Some(block) =
                                    own_opaque(b).and_then(|d| d.get("block")).filter(|v| {
                                        matches!(
                                            str_at(v, "type"),
                                            "thinking" | "redacted_thinking"
                                        )
                                    })
                                {
                                    content.push(block.clone());
                                }
                            }
                        }
                    }
                }
                Role::Tool => {
                    flush_role(&mut messages, &mut role, &mut content, "user");
                    for b in &m.blocks {
                        if let ContentBlock::ToolResult {
                            call_id,
                            content: result,
                            is_error,
                            ..
                        } = b
                        {
                            let mut block = json!({
                                "type": "tool_result",
                                "tool_use_id": call_id,
                                "content": tool_result_text(result),
                            });
                            if *is_error {
                                block["is_error"] = json!(true);
                            }
                            content.push(block);
                        }
                    }
                }
            }
        }
        flush_role(&mut messages, &mut role, &mut content, "");

        let max = request.max_output_tokens.unwrap_or(FALLBACK_MAX_TOKENS);
        let max = known_max_output(&request.model).map_or(max, |cap| max.min(cap));
        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "max_tokens": max,
            "stream": true,
        });
        if let Some(system) = request.system.as_deref().filter(|s| !s.is_empty()) {
            body["system"] = json!(system);
        }
        if supports_adaptive_thinking(&request.model) {
            body["thinking"] = json!({
                "type": "adaptive",
                "display": "summarized",
                "block_binding": { "prefix_mismatch_behavior": "drop_block" },
            });
        }
        if !request.tools.is_empty() {
            // `tool_choice` is omitted (auto): the newest models reject forced tool use,
            // and `strict` is off so MCP schemas are accepted as-is.
            body["tools"] = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    })
                })
                .collect();
        }
        body
    }
}

/// Parses one `GET /models` page into models with their reported limits and capabilities,
/// plus the `after_id` cursor for the next page. Retired models fail at request time, so
/// they are left out; `deprecated` ones still work.
pub fn parse_models_page(doc: &Value) -> (Vec<(String, DiscoveredModel)>, Option<String>) {
    let supported = |m: &Value, cap: &str| {
        m.get("capabilities")
            .and_then(|c| c.get(cap))
            .and_then(|c| c.get("supported"))
            .and_then(Value::as_bool)
    };
    let models = doc
        .get("data")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|m| {
                    let id = m.get("id")?.as_str()?;
                    if m.get("lifecycle").and_then(Value::as_str) == Some("retired") {
                        return None;
                    }
                    let details = DiscoveredModel {
                        context_window: m
                            .get("max_input_tokens")
                            .and_then(Value::as_u64)
                            .map(|n| n.min(u32::MAX as u64) as u32),
                        max_output_tokens: m
                            .get("max_tokens")
                            .and_then(Value::as_u64)
                            .map(|n| n.min(u32::MAX as u64) as u32),
                        vision: supported(m, "image_input"),
                        reasoning: supported(m, "thinking"),
                    };
                    Some((id.to_owned(), details))
                })
                .collect()
        })
        .unwrap_or_default();
    let next = (doc.get("has_more").and_then(Value::as_bool) == Some(true))
        .then(|| doc.get("last_id").and_then(Value::as_str))
        .flatten()
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    (models, next)
}

impl ModelProvider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn capabilities(&self) -> Capabilities {
        super::registry::ProviderKind::Anthropic.capabilities()
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
            .request(
                reqwest::Method::POST,
                format!("{}/messages", self.config.base_url),
            )
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
            let mut discovery = ModelDiscovery::default();
            let mut after: Option<String> = None;
            for _ in 0..MAX_LISTING_PAGES {
                let mut url = reqwest::Url::parse(&format!("{}/models", self.config.base_url))
                    .map_err(|e| Error::InvalidInput(format!("invalid base URL: {e}")))?;
                url.query_pairs_mut().append_pair("limit", "1000");
                if let Some(a) = &after {
                    url.query_pairs_mut().append_pair("after_id", a);
                }
                let resp = self
                    .request(reqwest::Method::GET, url.into())
                    .send()
                    .await
                    .map_err(|e| map_transport(&self.config.base_url, e))?;
                if !resp.status().is_success() {
                    return Err(status_error(resp).await);
                }
                let (models, next) = parse_models_page(&read_json(resp, "/models").await?);
                for (id, details) in models {
                    if !discovery.details.contains_key(&id) {
                        discovery.models.push(id.clone());
                    }
                    discovery.details.insert(id, details);
                }
                match next {
                    Some(a) => after = Some(a),
                    None => break,
                }
            }
            discovery.models.sort();
            Ok(discovery)
        })
    }
}

/// A content block between `content_block_start` and `content_block_stop`.
enum OpenBlock {
    Text,
    /// Signature accumulates from `signature_delta` events and lands just before the stop.
    Thinking {
        signature: String,
    },
    /// `data` arrives whole in the start event.
    Redacted {
        data: Value,
    },
    Call {
        id: String,
        name: String,
        arguments: String,
    },
    /// Blocks this adapter does not produce (server tools, `fallback` markers, new types).
    Other,
}

/// Messages SSE state machine. Opaque thinking blocks and tool calls are emitted when
/// their block stops, so stored blocks keep the exact order the server produced them in.
#[derive(Default)]
struct StreamState {
    open: BTreeMap<u64, OpenBlock>,
    call_ids: CallIds,
    usage: Option<Usage>,
    stop_reason: Option<String>,
    /// `stop_details.explanation`, carried by a `refusal` stop.
    refusal: Option<String>,
    /// A signed block failed to store: replaying later ones would leave a gap in the
    /// sequence that invalidates them, so storage ends for this response.
    thinking_dropped: bool,
    /// A terminal event was handled (`message_stop`, or an error was produced).
    finished: bool,
}

impl StreamState {
    fn handle(&mut self, ev: SseEvent, out: &mut Vec<ProviderEvent>) -> Result<()> {
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
            return Ok(()); // pings, keep-alives and anything we don't model
        };
        if self.finished {
            return Ok(());
        }
        let kind = v
            .get("type")
            .and_then(Value::as_str)
            .or(ev.event.as_deref())
            .unwrap_or("");
        let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
        match kind {
            "message_start" => {
                let message = v.get("message").unwrap_or(&Value::Null);
                if let Some(u) = message.get("usage") {
                    self.usage = Some(parse_usage(u));
                }
                // Present only under the binding beta: which replayed thinking blocks the
                // API dropped (prefix mismatch) or rewrote. Useful signal for Phase 6.
                if let Some(t) = message
                    .get("input_transformations")
                    .and_then(Value::as_array)
                    .filter(|t| !t.is_empty())
                {
                    tracing::debug!(transformations = ?t, "Anthropic dropped replayed thinking blocks");
                }
            }
            "content_block_start" => {
                self.start(index, v.get("content_block").unwrap_or(&Value::Null), out)
            }
            "content_block_delta" => self.delta(index, v.get("delta").unwrap_or(&Value::Null), out),
            "content_block_stop" => {
                if let Some(block) = self.open.remove(&index) {
                    self.close(block, out);
                }
            }
            "message_delta" => {
                let delta = v.get("delta").unwrap_or(&Value::Null);
                if let Some(r) = delta.get("stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(r.to_owned());
                    if r == "refusal" {
                        self.refusal = delta
                            .pointer("/stop_details/explanation")
                            .or_else(|| v.pointer("/message/stop_details/explanation"))
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                    }
                }
                if let Some(u) = v.get("usage") {
                    merge_usage(&mut self.usage, u);
                }
            }
            "message_stop" => {
                self.finish(out)?;
                self.finished = true;
            }
            "error" => {
                self.finished = true;
                return Err(Error::Provider(
                    extract_error_message(&v).unwrap_or_else(|| "Anthropic stream error".into()),
                ));
            }
            // `ping` and anything newer than this adapter: nothing to do.
            _ => {}
        }
        Ok(())
    }

    fn start(&mut self, index: u64, block: &Value, out: &mut Vec<ProviderEvent>) {
        if self.open.len() >= MAX_OPEN_BLOCKS || self.open.contains_key(&index) {
            return;
        }
        let open = match str_at(block, "type") {
            "text" => {
                let text = str_at(block, "text");
                if !text.is_empty() {
                    out.push(ProviderEvent::TextDelta(text.to_owned()));
                }
                OpenBlock::Text
            }
            "thinking" => {
                // The start event normally carries empty fields; take any initial text
                // (e.g. a progress update) and signature.
                let text = str_at(block, "thinking");
                if !text.is_empty() {
                    out.push(ProviderEvent::ReasoningDelta(text.to_owned()));
                }
                OpenBlock::Thinking {
                    signature: str_at(block, "signature").to_owned(),
                }
            }
            "redacted_thinking" => OpenBlock::Redacted {
                data: block.get("data").cloned().unwrap_or(Value::Null),
            },
            "tool_use" => OpenBlock::Call {
                id: str_at(block, "id").to_owned(),
                name: str_at(block, "name").to_owned(),
                arguments: String::new(),
            },
            _ => OpenBlock::Other,
        };
        self.open.insert(index, open);
    }

    fn delta(&mut self, index: u64, delta: &Value, out: &mut Vec<ProviderEvent>) {
        match (self.open.get_mut(&index), str_at(delta, "type")) {
            (Some(OpenBlock::Text) | None, "text_delta") => {
                let text = str_at(delta, "text");
                if !text.is_empty() {
                    out.push(ProviderEvent::TextDelta(text.to_owned()));
                }
            }
            (Some(OpenBlock::Thinking { .. }), "thinking_delta") => {
                let text = str_at(delta, "thinking");
                if !text.is_empty() {
                    out.push(ProviderEvent::ReasoningDelta(text.to_owned()));
                }
            }
            (Some(OpenBlock::Thinking { signature }), "signature_delta") => {
                push_bounded(
                    signature,
                    str_at(delta, "signature"),
                    super::MAX_OPAQUE_BYTES,
                );
            }
            (Some(OpenBlock::Call { arguments, .. }), "input_json_delta") => {
                push_bounded(arguments, str_at(delta, "partial_json"), MAX_TOOL_ARG_CHARS);
            }
            _ => {}
        }
    }

    fn close(&mut self, block: OpenBlock, out: &mut Vec<ProviderEvent>) {
        match block {
            OpenBlock::Thinking { signature } => {
                if signature.is_empty() || self.thinking_dropped {
                    // A signature-less block cannot be replayed; storing later blocks
                    // would create a gap that invalidates them anyway.
                    self.thinking_dropped = true;
                    return;
                }
                // The signature is display-independent and the server ignores the
                // `thinking` field on round-tripped blocks, so the stored form keeps
                // only what replay needs; the readable text is in the Thinking block.
                let data = json!({
                    "block": { "type": "thinking", "thinking": "", "signature": signature },
                });
                if opaque_within_limit(&data) {
                    out.push(ProviderEvent::Opaque {
                        provider: FAMILY,
                        data,
                    });
                } else {
                    self.thinking_dropped = true;
                }
            }
            OpenBlock::Redacted { data } => {
                if self.thinking_dropped {
                    return;
                }
                let data = json!({ "block": { "type": "redacted_thinking", "data": data } });
                if opaque_within_limit(&data) {
                    out.push(ProviderEvent::Opaque {
                        provider: FAMILY,
                        data,
                    });
                } else {
                    self.thinking_dropped = true;
                }
            }
            OpenBlock::Call {
                id,
                name,
                arguments,
            } => {
                if name.is_empty() {
                    return;
                }
                out.push(ProviderEvent::ToolCall {
                    id: self.call_ids.unique(id),
                    name,
                    arguments: if arguments.trim().is_empty() {
                        "{}".into()
                    } else {
                        arguments
                    },
                });
            }
            OpenBlock::Text | OpenBlock::Other => {}
        }
    }

    /// Terminal `message_stop`: close anything left open, then emit usage and `Done`
    /// (or the error a `refusal` stop stands for).
    fn finish(&mut self, out: &mut Vec<ProviderEvent>) -> Result<()> {
        for (_, block) in std::mem::take(&mut self.open) {
            self.close(block, out);
        }
        if self.stop_reason.as_deref() == Some("refusal") {
            let detail = self
                .refusal
                .as_deref()
                .map(|e| format!(": {e}"))
                .unwrap_or_default();
            return Err(Error::Provider(format!(
                "Claude declined to respond{detail}"
            )));
        }
        if let Some(u) = self.usage.take() {
            out.push(ProviderEvent::Usage(u));
        }
        let reason = match self.stop_reason.as_deref() {
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") | Some("model_context_window_exceeded") => StopReason::MaxTokens,
            _ if self.call_ids.count() > 0 => StopReason::ToolUse,
            // end_turn, stop_sequence, pause_turn, and anything unexpected.
            _ => StopReason::EndTurn,
        };
        out.push(ProviderEvent::Done(reason));
        Ok(())
    }
}

fn parse_usage(u: &Value) -> Usage {
    let n = |k: &str| u.get(k).and_then(Value::as_u64);
    Usage {
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_read_tokens: n("cache_read_input_tokens"),
        cache_write_tokens: n("cache_creation_input_tokens"),
    }
}

/// `message_delta` usage fields overwrite the partial values `message_start` reported.
fn merge_usage(usage: &mut Option<Usage>, u: &Value) {
    let Some(current) = usage else {
        *usage = Some(parse_usage(u));
        return;
    };
    let n = |k: &str| u.get(k).and_then(Value::as_u64);
    for (field, slot) in [
        ("input_tokens", &mut current.input_tokens),
        ("output_tokens", &mut current.output_tokens),
        ("cache_read_input_tokens", &mut current.cache_read_tokens),
        (
            "cache_creation_input_tokens",
            &mut current.cache_write_tokens,
        ),
    ] {
        if let Some(v) = n(field) {
            *slot = Some(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Message;
    use crate::providers::ToolSpec;

    fn opaque(block: Value) -> ContentBlock {
        ContentBlock::ProviderOpaque {
            provider: FAMILY.into(),
            data: json!({ "block": block }),
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

    fn request(messages: Vec<Message>) -> CompletionRequest {
        CompletionRequest {
            model: "claude-opus-5-5".into(),
            system: Some("sys".into()),
            messages,
            max_output_tokens: Some(4096),
            tools: vec![ToolSpec {
                name: "fs__read".into(),
                description: "Read a file".into(),
                parameters: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            }],
        }
    }

    #[test]
    fn body_shape_and_block_mapping() {
        let thinking = json!({ "type": "thinking", "thinking": "", "signature": "sig-t" });
        let redacted = json!({ "type": "redacted_thinking", "data": "enc" });
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
                    opaque(thinking.clone()),
                    ContentBlock::Text {
                        text: "Reading.".into(),
                    },
                    opaque(redacted.clone()),
                    call("c1", "fs__read", json!({ "path": "a.txt" })),
                    call("c2", "fs__read", Value::String("not json".into())),
                    ContentBlock::ProviderOpaque {
                        provider: "other_family".into(),
                        data: json!({ "block": { "type": "thinking" } }),
                    },
                    opaque(json!({ "type": "text", "text": "injected" })),
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
            Message::text(Role::User, "and?"),
        ];
        let body = AnthropicProvider::build_body(&request(messages));
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["system"], "sys");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["stream"], true);
        assert_eq!(
            body["thinking"],
            json!({
                "type": "adaptive",
                "display": "summarized",
                "block_binding": { "prefix_mismatch_behavior": "drop_block" },
            })
        );
        assert_eq!(
            body["tools"][0],
            json!({ "name": "fs__read", "description": "Read a file",
                    "input_schema": { "type": "object", "properties": { "path": { "type": "string" } } } })
        );

        let msgs = body["messages"].as_array().unwrap();
        let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "user"], "strict alternation");
        let types = |i: usize| -> Vec<&str> {
            msgs[i]["content"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b["type"].as_str().unwrap())
                .collect()
        };
        assert_eq!(types(0), ["text", "image"]);
        assert_eq!(
            msgs[0]["content"][1]["source"],
            json!({
                "type": "base64", "media_type": "image/png", "data": "aGk=",
            })
        );
        assert_eq!(
            types(1),
            [
                "thinking",
                "text",
                "redacted_thinking",
                "tool_use",
                "tool_use"
            ],
            "foreign or non-thinking opaque state is dropped; own state replays in place"
        );
        assert_eq!(msgs[1]["content"][0], thinking, "thinking block verbatim");
        assert_eq!(msgs[1]["content"][2], redacted, "redacted block verbatim");
        assert_eq!(msgs[1]["content"][3]["input"], json!({ "path": "a.txt" }));
        assert_eq!(
            msgs[1]["content"][4]["input"],
            json!({}),
            "malformed args become {{}}"
        );
        assert_eq!(types(2), ["tool_result", "tool_result", "text"]);
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "c1");
        assert_eq!(msgs[2]["content"][0]["content"], "hello");
        assert!(msgs[2]["content"][0].get("is_error").is_none());
        assert_eq!(msgs[2]["content"][1]["is_error"], true);
        assert_eq!(msgs[2]["content"][1]["content"], "boom");
    }

    #[test]
    fn thinking_config_by_generation() {
        for (model, adaptive) in [
            ("claude-opus-5-5", true),
            ("claude-sonnet-5-5", true),
            ("claude-haiku-5-5", true),
            ("claude-fable-5-1", true),
            ("claude-mythos-5-1", true),
            ("claude-opus-4-6", true),
            ("claude-sonnet-4-6", true),
            ("claude-opus-4-8", true),
            ("anthropic/claude-opus-5-5", true),
            ("claude-sonnet-5-4", true),
            ("claude-opus-5-4-20260115", true),
            ("claude-sonnet-5-20261101", true),
            ("claude-opus-4-6@20260205", true),
            ("claude-opus-4-10", true),
            ("future-model", true),
            ("claude-opus-4-5@20251101", false),
            ("claude-2.1", false),
            ("claude-instant-1.2", false),
            ("claude-sonnet-4-5", false),
            ("claude-opus-4-5-20251101", false),
            ("claude-haiku-4-5-20251001", false),
            ("claude-sonnet-4-20250514", false),
            ("claude-opus-4-1-20250805", false),
            ("claude-sonnet-4", false),
            ("claude-3-7-sonnet-20250224", false),
            ("claude-3-5-sonnet-20241022", false),
        ] {
            assert_eq!(supports_adaptive_thinking(model), adaptive, "{model}");
        }
        let mut req = request(vec![Message::text(Role::User, "hi")]);
        req.model = "claude-sonnet-4-5".into();
        let body = AnthropicProvider::build_body(&req);
        assert!(
            body.get("thinking").is_none(),
            "legacy model gets no thinking config"
        );
    }

    #[test]
    fn output_cap_is_clamped_per_generation() {
        let mut req = request(vec![Message::text(Role::User, "hi")]);
        req.max_output_tokens = Some(250_000); // the compiler's 25%-of-1M budget
        assert_eq!(AnthropicProvider::build_body(&req)["max_tokens"], 128_000);
        req.model = "claude-sonnet-4-5".into();
        assert_eq!(AnthropicProvider::build_body(&req)["max_tokens"], 64_000);
        req.model = "claude-3-7-sonnet-20250224".into();
        assert_eq!(AnthropicProvider::build_body(&req)["max_tokens"], 64_000);
        req.model = "claude-3-5-sonnet-20241022".into();
        assert_eq!(AnthropicProvider::build_body(&req)["max_tokens"], 4_096);
        req.model = "claude-sonnet-5-4".into();
        assert_eq!(AnthropicProvider::build_body(&req)["max_tokens"], 128_000);
        req.model = "proxy-model".into();
        assert_eq!(
            AnthropicProvider::build_body(&req)["max_tokens"],
            250_000,
            "unknown names are left to the server"
        );
    }

    #[test]
    fn models_page_maps_limits_and_capabilities() {
        let page = json!({
            "data": [
                {
                    "id": "claude-opus-5-5",
                    "lifecycle": "active",
                    "max_input_tokens": 1_000_000,
                    "max_tokens": 128_000,
                    "capabilities": {
                        "image_input": { "supported": true },
                        "thinking": { "supported": true },
                    },
                },
                { "id": "claude-3-5-sonnet-20241022", "lifecycle": "retired" },
                {
                    "id": "claude-sonnet-4-5",
                    "lifecycle": "deprecated",
                    "max_input_tokens": 200_000,
                    "max_tokens": 64_000,
                    "capabilities": {
                        "image_input": { "supported": true },
                        "thinking": { "supported": true },
                    },
                },
            ],
            "has_more": true,
            "last_id": "claude-sonnet-4-5",
        });
        let (models, next) = parse_models_page(&page);
        assert_eq!(
            models.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["claude-opus-5-5", "claude-sonnet-4-5"],
            "retired models are left out; deprecated ones stay"
        );
        let d = &models[0].1;
        assert_eq!(d.context_window, Some(1_000_000));
        assert_eq!(d.max_output_tokens, Some(128_000));
        assert_eq!(d.vision, Some(true));
        assert_eq!(d.reasoning, Some(true));
        assert_eq!(next.as_deref(), Some("claude-sonnet-4-5"));

        let (_, next) = parse_models_page(&json!({ "data": [], "has_more": false }));
        assert_eq!(next, None);
    }
}
