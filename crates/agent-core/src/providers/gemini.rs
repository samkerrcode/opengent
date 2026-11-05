//! Google Gemini adapter for the Interactions API (`POST {base}/interactions`), authenticated
//! with a Google AI Studio API key.
//!
//! Requests are stateless (`store: false`): the full history is sent as an ordered list of
//! steps (`user_input`, `model_output`, `thought`, `function_call`, `function_result`), which
//! maps 1:1 onto stored content blocks. The API requires model steps to be replayed exactly
//! as received, so signature-bearing state rides in `ProviderOpaque` blocks:
//!
//! - `{"step": <thought step>}` — the complete `thought` step, inserted verbatim at its
//!   position. If it would exceed `MAX_OPAQUE_BYTES`, the summary is dropped and only the
//!   signature (what the server validates) is kept; the readable summary is stored separately
//!   as the turn's `Thinking` block either way.
//! - `{"call_signature": {"id", "signature"}}` — a `function_call` step's signature, attached
//!   back onto the matching `function_call` step when history is rebuilt.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::common::{
    extract_error_message, map_transport, pump_sse, read_json, send_checked, spawn_stream,
    status_error, StreamEnd, MAX_TOOL_ARG_CHARS,
};
use super::sse::SseEvent;
use super::{
    opaque_within_limit, Capabilities, CompletionRequest, DiscoveredModel, ModelDiscovery,
    ModelProvider, ProviderEvent, ProviderFuture, PROVIDER_EVENT_BUFFER,
};
use crate::domain::{tool_result_text, ContentBlock, Role, StopReason, Usage};
use crate::{Error, Result};

/// Opaque-state family tag. Stable: stored history references it.
pub const FAMILY: &str = "google_gemini";
/// Stable (GA) Gemini API root; `/v1beta` also works for beta-only features.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1";
/// Schema revision marker. The legacy schema was removed in June 2026 and the server now
/// ignores this header; it is sent so requests stay explicit about the `steps` schema.
const API_REVISION: &str = "2026-05-20";
/// Listing pages are large (`pageSize=1000`); this only bounds a misbehaving server.
const MAX_LISTING_PAGES: usize = 10;
/// Bounds per-stream bookkeeping against a hostile server inventing step indices.
const MAX_OPEN_STEPS: usize = 256;
/// Model families on the listing endpoint that are not chat models.
const NON_CHAT_MARKERS: &[&str] = &[
    "embedding",
    "-tts",
    "-image",
    "imagen",
    "veo",
    "lyria",
    "omni",
    "nano-banana",
    "robotics",
    "aqa",
];

#[derive(Debug, Clone)]
pub struct GeminiConfig {
    pub id: String,
    /// e.g. `https://generativelanguage.googleapis.com/v1` — `/interactions` and `/models`
    /// are appended.
    pub base_url: String,
    pub api_key: Option<String>,
    pub connect_timeout: Duration,
    /// Maximum silence between stream chunks. Generous: Pro models can think for a long
    /// time before the first step arrives when no thought summaries are streamed.
    pub idle_timeout: Duration,
}

impl GeminiConfig {
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

pub struct GeminiProvider {
    config: GeminiConfig,
    http: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(config: GeminiConfig, http: reqwest::Client) -> Self {
        Self { config, http }
    }

    fn request(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        // The key travels in a header, never the query string, so it can't leak into logs.
        let mut req = self
            .http
            .request(method, url)
            .timeout(self.config.idle_timeout.max(self.config.connect_timeout) * 4)
            .header("Api-Revision", API_REVISION);
        if let Some(key) = &self.config.api_key {
            req = req.header("x-goog-api-key", key);
        }
        req
    }

    /// Builds the `interactions.create` body. Pure, so the step mapping is unit-tested.
    pub fn build_body(request: &CompletionRequest) -> Value {
        // Signatures captured from `function_call` steps, re-attached by call id.
        let mut call_signatures: HashMap<&str, &str> = HashMap::new();
        for m in &request.messages {
            for b in &m.blocks {
                if let Some((id, sig)) = own_opaque(b).and_then(call_signature) {
                    call_signatures.insert(id, sig);
                }
            }
        }
        let mut call_names: HashMap<&str, &str> = HashMap::new();
        let mut input: Vec<Value> = Vec::new();
        for m in &request.messages {
            match m.role {
                Role::User | Role::System => {
                    let content: Vec<Value> = m
                        .blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } if !text.is_empty() => {
                                Some(json!({ "type": "text", "text": text }))
                            }
                            ContentBlock::Image { mime, data } => {
                                Some(json!({ "type": "image", "data": data, "mime_type": mime }))
                            }
                            _ => None,
                        })
                        .collect();
                    if !content.is_empty() {
                        input.push(json!({ "type": "user_input", "content": content }));
                    }
                }
                Role::Assistant => {
                    for b in &m.blocks {
                        match b {
                            ContentBlock::Text { text } if !text.trim().is_empty() => {
                                input.push(json!({
                                    "type": "model_output",
                                    "content": [{ "type": "text", "text": text }],
                                }));
                            }
                            ContentBlock::ToolCall {
                                id,
                                name,
                                arguments,
                                ..
                            } => {
                                call_names.insert(id, name);
                                let mut step = json!({
                                    "type": "function_call",
                                    "id": id,
                                    "name": name,
                                    "arguments": arguments_object(arguments),
                                });
                                if let Some(sig) = call_signatures.get(id.as_str()) {
                                    step["signature"] = json!(sig);
                                }
                                input.push(step);
                            }
                            b => {
                                if let Some(step) = own_opaque(b)
                                    .and_then(|d| d.get("step"))
                                    .filter(|s| s.is_object())
                                {
                                    input.push(step.clone());
                                }
                            }
                        }
                    }
                }
                Role::Tool => {
                    for b in &m.blocks {
                        if let ContentBlock::ToolResult {
                            call_id,
                            content,
                            is_error,
                            ..
                        } = b
                        {
                            let mut step = json!({
                                "type": "function_result",
                                "call_id": call_id,
                                "result": [{ "type": "text", "text": tool_result_text(content) }],
                                "is_error": is_error,
                            });
                            if let Some(name) = call_names.get(call_id.as_str()) {
                                step["name"] = json!(name);
                            }
                            input.push(step);
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
            body["system_instruction"] = json!(system);
        }
        let mut generation = Map::new();
        // Gemma models are not documented to support thought summaries; skip the option
        // rather than risk a rejected request.
        if !request.model.to_ascii_lowercase().contains("gemma") {
            generation.insert("thinking_summaries".into(), json!("auto"));
        }
        if let Some(max) = request.max_output_tokens {
            generation.insert("max_output_tokens".into(), json!(max));
        }
        if !generation.is_empty() {
            body["generation_config"] = Value::Object(generation);
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
                    })
                })
                .collect();
        }
        body
    }
}

/// The payload of an opaque block this adapter produced.
fn own_opaque(b: &ContentBlock) -> Option<&Value> {
    match b {
        ContentBlock::ProviderOpaque { provider, data } if provider == FAMILY => Some(data),
        _ => None,
    }
}

fn call_signature(data: &Value) -> Option<(&str, &str)> {
    let c = data.get("call_signature")?;
    Some((c.get("id")?.as_str()?, c.get("signature")?.as_str()?))
}

/// `function_call.arguments` must be an object. Stored arguments are an object when the
/// model produced valid JSON, else the raw text (which may still parse).
fn arguments_object(arguments: &Value) -> Value {
    match arguments {
        Value::Object(_) => arguments.clone(),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({})),
        _ => json!({}),
    }
}

/// Parses one `GET /models` page into chat models and the next page token.
pub fn parse_models_page(doc: &Value) -> (Vec<(String, DiscoveredModel)>, Option<String>) {
    let models = doc
        .get("models")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|m| {
                    let id = m.get("name")?.as_str()?;
                    let id = id.strip_prefix("models/").unwrap_or(id).to_owned();
                    let lower = id.to_ascii_lowercase();
                    if NON_CHAT_MARKERS.iter().any(|x| lower.contains(x)) {
                        return None;
                    }
                    // The listing has no Interactions marker; `generateContent` support is
                    // the closest proxy for "chat model". Entries without the field are kept.
                    let chat = m
                        .get("supportedGenerationMethods")
                        .and_then(Value::as_array)
                        .is_none_or(|ms| ms.iter().any(|x| x.as_str() == Some("generateContent")));
                    if !chat {
                        return None;
                    }
                    let details = DiscoveredModel {
                        context_window: m
                            .get("inputTokenLimit")
                            .and_then(Value::as_u64)
                            .map(|n| n.min(u32::MAX as u64) as u32),
                        // Gemini chat models accept image input; `ImageSupport` overrides it.
                        vision: Some(true),
                        reasoning: m.get("thinking").and_then(Value::as_bool),
                    };
                    Some((id, details))
                })
                .collect()
        })
        .unwrap_or_default();
    let next = doc
        .get("nextPageToken")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    (models, next)
}

impl ModelProvider for GeminiProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            tools: true,
            parallel_tool_calls: true,
            ..Default::default()
        }
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
                format!("{}/interactions", self.config.base_url),
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
            if end == StreamEnd::Abandoned {
                return Ok(());
            }
            for ev in state.finish()? {
                if tx.send(ev).await.is_err() {
                    break;
                }
            }
            Ok(())
        });
        Ok(rx)
    }

    fn discover(&self) -> ProviderFuture<'_, ModelDiscovery> {
        Box::pin(async move {
            let mut discovery = ModelDiscovery::default();
            let mut token: Option<String> = None;
            for _ in 0..MAX_LISTING_PAGES {
                let mut url = reqwest::Url::parse(&format!("{}/models", self.config.base_url))
                    .map_err(|e| Error::InvalidInput(format!("invalid base URL: {e}")))?;
                url.query_pairs_mut().append_pair("pageSize", "1000");
                if let Some(t) = &token {
                    url.query_pairs_mut().append_pair("pageToken", t);
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
                    Some(t) => token = Some(t),
                    None => break,
                }
            }
            discovery.models.sort();
            Ok(discovery)
        })
    }
}

/// A step between `step.start` and `step.stop`.
enum OpenStep {
    /// The step object as received (unknown fields preserved) plus streamed deltas.
    Thought {
        base: Value,
        signature: String,
        summary: Vec<Value>,
    },
    Call {
        id: String,
        name: String,
        arguments: String,
        signature: String,
    },
    Output,
    /// Steps this adapter does not consume (e.g. echoed `user_input`).
    Other,
}

/// Interactions SSE state machine. Thought and call steps are emitted when they stop, so
/// stored blocks keep the exact order the server produced them in.
#[derive(Default)]
struct StreamState {
    open: BTreeMap<u64, OpenStep>,
    call_ids: HashSet<String>,
    calls: usize,
    usage: Option<Usage>,
    status: Option<String>,
}

fn str_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Appends at most `limit` total chars; a hostile server must not grow state unboundedly.
fn push_bounded(buf: &mut String, s: &str, limit: usize) {
    let room = limit.saturating_sub(buf.len());
    buf.extend(s.chars().take(room));
}

impl StreamState {
    fn handle(&mut self, ev: SseEvent, out: &mut Vec<ProviderEvent>) -> Result<()> {
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
            return Ok(()); // keep-alives and anything we don't model
        };
        let kind = v
            .get("event_type")
            .and_then(Value::as_str)
            .or(ev.event.as_deref())
            .unwrap_or("");
        let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
        match kind {
            "step.start" => self.start(index, v.get("step").unwrap_or(&Value::Null), out),
            "step.delta" => self.delta(index, v.get("delta").unwrap_or(&Value::Null), out),
            "step.stop" => {
                if let Some(step) = self.open.remove(&index) {
                    self.close(step, out);
                }
            }
            "interaction.completed" => {
                let interaction = v.get("interaction").unwrap_or(&Value::Null);
                self.status = Some(str_at(interaction, "status").to_owned());
                if let Some(u) = interaction.get("usage") {
                    self.usage = Some(parse_usage(u));
                }
            }
            "error" => {
                return Err(Error::Provider(
                    extract_error_message(&v).unwrap_or_else(|| "Gemini stream error".into()),
                ))
            }
            // `interaction.created`, status updates: nothing to do. `event_id` (stream resume)
            // is not used yet.
            _ => {}
        }
        Ok(())
    }

    fn start(&mut self, index: u64, step: &Value, out: &mut Vec<ProviderEvent>) {
        if self.open.len() >= MAX_OPEN_STEPS {
            return;
        }
        // `step.start` already carries content: the first text chunk, the first summary
        // item, or a call's id/name/arguments.
        let open = match str_at(step, "type") {
            "thought" => {
                let summary: Vec<Value> = step
                    .get("summary")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for item in &summary {
                    push_reasoning(item, out);
                }
                OpenStep::Thought {
                    base: step.clone(),
                    signature: str_at(step, "signature").to_owned(),
                    summary,
                }
            }
            "function_call" => OpenStep::Call {
                id: str_at(step, "id").to_owned(),
                name: str_at(step, "name").to_owned(),
                arguments: match step.get("arguments") {
                    Some(a @ Value::Object(o)) if !o.is_empty() => a.to_string(),
                    Some(Value::String(s)) => s.chars().take(MAX_TOOL_ARG_CHARS).collect(),
                    _ => String::new(),
                },
                signature: str_at(step, "signature").to_owned(),
            },
            "model_output" => {
                for item in step
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    push_text(str_at(item, "type"), str_at(item, "text"), out);
                }
                OpenStep::Output
            }
            _ => OpenStep::Other,
        };
        self.open.insert(index, open);
    }

    fn delta(&mut self, index: u64, delta: &Value, out: &mut Vec<ProviderEvent>) {
        let kind = str_at(delta, "type");
        match (self.open.get_mut(&index), kind) {
            (
                Some(OpenStep::Thought {
                    summary, signature, ..
                }),
                _,
            ) => match kind {
                "thought_summary" => {
                    if let Some(item) = delta.get("content") {
                        push_reasoning(item, out);
                        summary.push(item.clone());
                    }
                }
                "thought_signature" => push_bounded(
                    signature,
                    str_at(delta, "signature"),
                    super::MAX_OPAQUE_BYTES,
                ),
                _ => {}
            },
            (
                Some(OpenStep::Call {
                    id,
                    arguments,
                    signature,
                    ..
                }),
                "arguments_delta",
            ) => {
                push_bounded(arguments, str_at(delta, "arguments"), MAX_TOOL_ARG_CHARS);
                if id.is_empty() {
                    *id = str_at(delta, "id").to_owned();
                }
                let sig = str_at(delta, "signature");
                if !sig.is_empty() {
                    *signature = sig.to_owned();
                }
            }
            (Some(OpenStep::Call { .. } | OpenStep::Other), _) => {}
            // Text for a model_output step, or for a step whose start we never saw.
            (Some(OpenStep::Output) | None, "text") => {
                push_text("text", str_at(delta, "text"), out);
            }
            _ => {}
        }
    }

    fn close(&mut self, step: OpenStep, out: &mut Vec<ProviderEvent>) {
        match step {
            OpenStep::Thought {
                base,
                signature,
                summary,
            } => {
                if signature.is_empty() && summary.is_empty() {
                    return;
                }
                let mut step = match base {
                    Value::Object(m) => m,
                    _ => Map::new(),
                };
                step.insert("type".into(), json!("thought"));
                step.remove("signature");
                step.remove("summary");
                if !signature.is_empty() {
                    step.insert("signature".into(), json!(signature));
                }
                let lean = json!({ "step": step.clone() });
                if !summary.is_empty() {
                    step.insert("summary".into(), Value::Array(summary));
                }
                let full = json!({ "step": step });
                // Keep the signature even when a long summary would push the block over the
                // opaque size cap: the server validates the signature, and the summary text
                // is already stored as the turn's Thinking block.
                let data = if opaque_within_limit(&full) {
                    full
                } else {
                    lean
                };
                out.push(ProviderEvent::Opaque {
                    provider: FAMILY,
                    data,
                });
            }
            OpenStep::Call {
                mut id,
                name,
                arguments,
                signature,
            } => {
                if name.is_empty() {
                    return;
                }
                // Approvals and result pairing key on the id: replace empty or repeated ones.
                if id.is_empty() || self.call_ids.contains(&id) {
                    id = format!("call_{}_{}", self.calls, uuid::Uuid::new_v4().simple());
                }
                self.call_ids.insert(id.clone());
                self.calls += 1;
                if !signature.is_empty() {
                    out.push(ProviderEvent::Opaque {
                        provider: FAMILY,
                        data: json!({ "call_signature": { "id": id, "signature": signature } }),
                    });
                }
                out.push(ProviderEvent::ToolCall {
                    id,
                    name,
                    arguments: if arguments.trim().is_empty() {
                        "{}".into()
                    } else {
                        arguments
                    },
                });
            }
            OpenStep::Output | OpenStep::Other => {}
        }
    }

    /// End of body: close steps the server left open, then report usage and the outcome.
    fn finish(mut self) -> Result<Vec<ProviderEvent>> {
        let Some(status) = self.status.take() else {
            return Err(Error::Provider(
                "stream ended before the interaction completed".into(),
            ));
        };
        let mut out = Vec::new();
        for (_, step) in std::mem::take(&mut self.open) {
            self.close(step, &mut out);
        }
        let reason = match status.as_str() {
            "failed" => {
                return Err(Error::Provider(
                    "Gemini reported the interaction failed".into(),
                ))
            }
            "cancelled" => return Err(Error::Provider("Gemini cancelled the interaction".into())),
            "requires_continuation" => {
                return Err(Error::Provider(
                    "Gemini paused a long response for continuation, which is not supported yet \
                     — retry, or lower the output length"
                        .into(),
                ))
            }
            _ if self.calls > 0 => StopReason::ToolUse,
            "incomplete" => StopReason::MaxTokens,
            _ => StopReason::EndTurn,
        };
        if let Some(u) = self.usage {
            out.push(ProviderEvent::Usage(u));
        }
        out.push(ProviderEvent::Done(reason));
        Ok(out)
    }
}

fn push_text(kind: &str, text: &str, out: &mut Vec<ProviderEvent>) {
    if kind == "text" && !text.is_empty() {
        out.push(ProviderEvent::TextDelta(text.to_owned()));
    }
}

/// Thought summaries are content items; only text items are shown.
fn push_reasoning(item: &Value, out: &mut Vec<ProviderEvent>) {
    if str_at(item, "type") == "text" {
        let text = str_at(item, "text");
        if !text.is_empty() {
            out.push(ProviderEvent::ReasoningDelta(text.to_owned()));
        }
    }
}

/// Thought tokens are billed as output and are reported separately, so they are folded in.
fn parse_usage(u: &Value) -> Usage {
    let n = |k: &str| u.get(k).and_then(Value::as_u64);
    let output = match (n("total_output_tokens"), n("total_thought_tokens")) {
        (None, None) => None,
        (o, t) => Some(o.unwrap_or(0) + t.unwrap_or(0)),
    };
    Usage {
        input_tokens: n("total_input_tokens"),
        output_tokens: output,
        cache_read_tokens: n("total_cached_tokens"),
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

    fn request(messages: Vec<Message>) -> CompletionRequest {
        CompletionRequest {
            model: "gemini-flash-latest".into(),
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
    fn body_shape_and_step_sequence() {
        let thought = json!({ "type": "thought", "signature": "sig-t", "summary": [{ "type": "text", "text": "plan" }] });
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
                    opaque(json!({ "step": thought.clone() })),
                    ContentBlock::Text {
                        text: "Reading.".into(),
                    },
                    opaque(json!({ "call_signature": { "id": "c1", "signature": "sig-c" } })),
                    call("c1", "fs__read", json!({ "path": "a.txt" })),
                    call("c2", "fs__read", Value::String("not json".into())),
                    ContentBlock::ProviderOpaque {
                        provider: "other_family".into(),
                        data: json!({ "step": { "type": "thought" } }),
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
        let body = GeminiProvider::build_body(&request(messages));
        assert_eq!(body["model"], "gemini-flash-latest");
        assert_eq!(body["system_instruction"], "sys");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["generation_config"]["max_output_tokens"], 256);
        assert_eq!(body["generation_config"]["thinking_summaries"], "auto");
        assert_eq!(
            body["tools"][0],
            json!({ "type": "function", "name": "fs__read", "description": "Read a file",
                    "parameters": { "type": "object", "properties": { "path": { "type": "string" } } } })
        );
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
            ],
            "foreign opaque state is dropped; own state replays in place"
        );
        assert_eq!(
            input[0]["content"][1],
            json!({ "type": "image", "data": "aGk=", "mime_type": "image/png" })
        );
        assert_eq!(input[1], thought, "thought step replayed verbatim");
        assert_eq!(input[2]["content"][0]["text"], "Reading.");
        assert_eq!(
            input[3],
            json!({ "type": "function_call", "id": "c1", "name": "fs__read",
                    "arguments": { "path": "a.txt" }, "signature": "sig-c" })
        );
        assert_eq!(
            input[4]["arguments"],
            json!({}),
            "malformed args become {{}}"
        );
        assert!(input[4].get("signature").is_none());
        assert_eq!(input[5]["call_id"], "c1");
        assert_eq!(input[5]["name"], "fs__read");
        assert_eq!(input[5]["result"][0]["text"], "hello");
        assert_eq!(input[5]["is_error"], false);
        assert_eq!(input[6]["is_error"], true);
        assert_eq!(input[6]["result"][0]["text"], "boom");
    }

    #[test]
    fn minimal_body_omits_optional_fields() {
        let mut req = request(vec![Message::text(Role::User, "hi")]);
        req.system = None;
        req.tools.clear();
        req.max_output_tokens = None;
        req.model = "gemma-4-31b-it".into();
        let body = GeminiProvider::build_body(&req);
        assert!(body.get("system_instruction").is_none());
        assert!(body.get("tools").is_none());
        assert!(body.get("generation_config").is_none(), "{body}");
    }

    fn sse(event_type: &str, data: Value) -> SseEvent {
        let mut data = data;
        data["event_type"] = json!(event_type);
        SseEvent {
            event: Some(event_type.into()),
            data: data.to_string(),
        }
    }

    fn run(events: Vec<SseEvent>) -> (Vec<ProviderEvent>, Result<Vec<ProviderEvent>>) {
        let mut state = StreamState::default();
        let mut out = Vec::new();
        for ev in events {
            if let Err(e) = state.handle(ev, &mut out) {
                return (out, Err(e));
            }
        }
        let end = state.finish();
        (out, end)
    }

    fn completed(status: &str) -> SseEvent {
        sse(
            "interaction.completed",
            json!({ "interaction": { "status": status, "usage": {
                "total_input_tokens": 10, "total_output_tokens": 5,
                "total_thought_tokens": 7, "total_cached_tokens": 4 } } }),
        )
    }

    #[test]
    fn thought_text_and_usage_stream_in_order() {
        let (out, end) = run(vec![
            sse(
                "interaction.created",
                json!({ "interaction": { "status": "in_progress" } }),
            ),
            sse(
                "step.start",
                json!({ "index": 0, "step": { "type": "thought", "signature": "",
                    "summary": [{ "type": "text", "text": "Evaluating" }] } }),
            ),
            sse(
                "step.delta",
                json!({ "index": 0, "delta": { "type": "thought_summary",
                    "content": { "type": "text", "text": " the clues" } } }),
            ),
            sse(
                "step.delta",
                json!({ "index": 0, "delta": { "type": "thought_signature", "signature": "SIG" } }),
            ),
            sse("step.stop", json!({ "index": 0 })),
            sse(
                "step.start",
                json!({ "index": 1, "step": { "type": "model_output",
                    "content": [{ "type": "text", "text": "Based on" }] } }),
            ),
            sse(
                "step.delta",
                json!({ "index": 1, "delta": { "type": "text", "text": " the clues." } }),
            ),
            sse("step.stop", json!({ "index": 1 })),
            completed("completed"),
        ]);
        let end = end.unwrap();
        assert_eq!(
            out,
            vec![
                ProviderEvent::ReasoningDelta("Evaluating".into()),
                ProviderEvent::ReasoningDelta(" the clues".into()),
                ProviderEvent::Opaque {
                    provider: FAMILY,
                    data: json!({ "step": { "type": "thought", "signature": "SIG", "summary": [
                        { "type": "text", "text": "Evaluating" },
                        { "type": "text", "text": " the clues" }
                    ] } }),
                },
                ProviderEvent::TextDelta("Based on".into()),
                ProviderEvent::TextDelta(" the clues.".into()),
            ],
            "content carried by step.start is not lost"
        );
        assert_eq!(
            end,
            vec![
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
    fn interleaved_calls_are_emitted_at_step_stop_in_order() {
        let (out, end) = run(vec![
            sse(
                "step.start",
                json!({ "index": 0, "step": { "type": "thought", "signature": "T1" } }),
            ),
            sse("step.stop", json!({ "index": 0 })),
            sse(
                "step.start",
                json!({ "index": 1, "step": { "type": "function_call", "id": "a", "name": "fs__read" } }),
            ),
            sse(
                "step.delta",
                json!({ "index": 1, "delta": { "type": "arguments_delta", "arguments": "{\"path\":" } }),
            ),
            sse(
                "step.delta",
                json!({ "index": 1, "delta": { "type": "arguments_delta", "arguments": "\"a\"}", "signature": "CS" } }),
            ),
            sse("step.stop", json!({ "index": 1 })),
            sse(
                "step.start",
                json!({ "index": 2, "step": { "type": "thought", "signature": "T2" } }),
            ),
            sse("step.stop", json!({ "index": 2 })),
            // Whole-object arguments, repeated id: the id is replaced.
            sse(
                "step.start",
                json!({ "index": 3, "step": { "type": "function_call", "id": "a", "name": "time__now",
                    "arguments": { "tz": "UTC" } } }),
            ),
            sse("step.stop", json!({ "index": 3 })),
            completed("requires_action"),
        ]);
        let kinds: Vec<&str> = out
            .iter()
            .map(|e| match e {
                ProviderEvent::Opaque { data, .. } if data.get("step").is_some() => "thought",
                ProviderEvent::Opaque { .. } => "call_sig",
                ProviderEvent::ToolCall { .. } => "call",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["thought", "call_sig", "call", "thought", "call"]);
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
        assert_eq!(calls[0], ("a", "fs__read", r#"{"path":"a"}"#));
        assert_eq!(calls[1].1, "time__now");
        assert_ne!(calls[1].0, "a", "duplicate id replaced");
        assert_eq!(calls[1].2, r#"{"tz":"UTC"}"#);
        assert!(matches!(
            &out[1],
            ProviderEvent::Opaque { data, .. } if data == &json!({ "call_signature": { "id": "a", "signature": "CS" } })
        ));
        assert_eq!(
            end.unwrap().last(),
            Some(&ProviderEvent::Done(StopReason::ToolUse))
        );
    }

    #[test]
    fn status_mapping() {
        let done = |status: &str| run(vec![completed(status)]).1;
        assert_eq!(
            done("incomplete").unwrap().last(),
            Some(&ProviderEvent::Done(StopReason::MaxTokens))
        );
        assert_eq!(
            done("completed").unwrap().last(),
            Some(&ProviderEvent::Done(StopReason::EndTurn))
        );
        for bad in ["failed", "cancelled", "requires_continuation"] {
            assert!(done(bad).is_err(), "{bad}");
        }
        let (_, end) = run(vec![sse(
            "step.start",
            json!({ "index": 0, "step": { "type": "model_output" } }),
        )]);
        assert!(end
            .unwrap_err()
            .to_string()
            .contains("before the interaction completed"));
    }

    #[test]
    fn error_event_fails_the_stream() {
        let (_, end) = run(vec![sse(
            "error",
            json!({ "error": { "code": "RESOURCE_EXHAUSTED", "message": "quota exceeded" } }),
        )]);
        assert!(end.unwrap_err().to_string().contains("quota exceeded"));
    }

    #[test]
    fn empty_thoughts_are_skipped_and_oversized_summaries_dropped() {
        let big = "x".repeat(super::super::MAX_OPAQUE_BYTES);
        let (out, _) = run(vec![
            sse(
                "step.start",
                json!({ "index": 0, "step": { "type": "thought", "signature": "" } }),
            ),
            sse("step.stop", json!({ "index": 0 })),
            sse(
                "step.start",
                json!({ "index": 1, "step": { "type": "thought", "signature": "KEEP",
                    "summary": [{ "type": "text", "text": big }] } }),
            ),
            sse("step.stop", json!({ "index": 1 })),
            completed("completed"),
        ]);
        let opaque: Vec<&Value> = out
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::Opaque { data, .. } => Some(data),
                _ => None,
            })
            .collect();
        assert_eq!(
            opaque,
            vec![&json!({ "step": { "type": "thought", "signature": "KEEP" } })]
        );
    }

    #[test]
    fn listing_filters_non_chat_models_and_reads_metadata() {
        let doc = json!({
            "models": [
                { "name": "models/gemini-flash-latest", "inputTokenLimit": 1048576,
                  "supportedGenerationMethods": ["generateContent", "countTokens"], "thinking": true },
                { "name": "models/gemma-4-31b-it", "inputTokenLimit": 131072,
                  "supportedGenerationMethods": ["generateContent"] },
                { "name": "models/gemini-embedding-001", "supportedGenerationMethods": ["embedContent"] },
                { "name": "models/gemini-3.8-flash-tts", "supportedGenerationMethods": ["generateContent"] },
                { "name": "models/imagen-4", "supportedGenerationMethods": ["predict"] }
            ],
            "nextPageToken": "p2"
        });
        let (models, next) = parse_models_page(&doc);
        let ids: Vec<&str> = models.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["gemini-flash-latest", "gemma-4-31b-it"]);
        assert_eq!(models[0].1.context_window, Some(1_048_576));
        assert_eq!(models[0].1.reasoning, Some(true));
        assert_eq!(models[0].1.vision, Some(true));
        assert_eq!(models[1].1.reasoning, None);
        assert_eq!(next.as_deref(), Some("p2"));
        assert_eq!(parse_models_page(&json!({ "models": [] })).1, None);
    }
}
