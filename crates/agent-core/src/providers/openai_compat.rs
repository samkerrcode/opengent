//! OpenAI-compatible Chat Completions adapter. This is the protocol spoken by most local
//! servers (Ollama, LM Studio, vLLM, llama.cpp) and many hosted gateways. Function tools are
//! sent as `tools`; streamed `tool_calls` fragments are assembled into whole calls.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::common::{
    arguments_string, extract_error_message, map_transport, normalize_call_ids, pump_sse,
    read_json, request_timeout, send_checked, spawn_stream, status_error, PartialCall, StreamEnd,
    MAX_TOOL_ARG_CHARS,
};
use super::{
    Capabilities, CompletionRequest, DiscoveredModel, ModelDiscovery, ModelProvider, ProviderEvent,
    ProviderFuture, PROVIDER_EVENT_BUFFER,
};
use crate::domain::{tool_result_text, ContentBlock, Role, StopReason, Usage};
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct OpenAiCompatConfig {
    pub id: String,
    /// e.g. `http://localhost:11434/v1` — `/chat/completions` and `/models` are appended.
    pub base_url: String,
    pub api_key: Option<String>,
    /// Ask the server to append a final usage chunk. Most modern servers support it.
    pub include_usage: bool,
    pub context_window: Option<u32>,
    pub connect_timeout: Duration,
    /// Maximum silence between stream chunks before the run fails.
    pub idle_timeout: Duration,
}

impl OpenAiCompatConfig {
    pub fn new(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            api_key: None,
            include_usage: true,
            context_window: None,
            connect_timeout: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(90),
        }
    }
}

pub struct OpenAiCompatProvider {
    config: OpenAiCompatConfig,
    http: reqwest::Client,
}

/// `scheme://host[:port]` of a base URL — where Ollama's `/api/*` and LM Studio's
/// `/api/v0/*` endpoints live relative to the `/v1` API root.
fn server_origin(base_url: &str) -> Option<String> {
    reqwest::Url::parse(base_url)
        .ok()
        .map(|u| u.origin().ascii_serialization())
}

/// Extracts the ids of models a `/models` document flags as accepting image input.
/// Returns `None` when the document carries no capability metadata at all. Covers
/// OpenRouter-style `architecture.modality` (`"text+image->text"`), LM Studio-style
/// `type: "vlm"`, and generic `input_modalities`/`modalities` string arrays — llama.cpp
/// nests these under `architecture` (`architecture.input_modalities`), most servers put
/// them at the model's top level.
pub fn vision_flags_from_models(doc: &Value) -> Option<Vec<String>> {
    let data = doc.get("data")?.as_array()?;
    let mut saw_metadata = false;
    let mut vision = Vec::new();
    for m in data {
        let arch = m.get("architecture");
        let accepts_image = arch
            .and_then(|a| a.get("modality"))
            .and_then(Value::as_str)
            .map(|modality| modality.split("->").next().unwrap_or("").contains("image"))
            .or_else(|| {
                m.get("type")
                    .and_then(Value::as_str)
                    .map(|t| t == "vlm" || t == "vision")
            })
            .or_else(|| {
                ["input_modalities", "modalities"].iter().find_map(|key| {
                    [m.get(*key), arch.and_then(|a| a.get(*key))]
                        .into_iter()
                        .flatten()
                        .find_map(|v| {
                            v.as_array().map(|modes| {
                                modes
                                    .iter()
                                    .any(|x| x.as_str().is_some_and(|s| s.contains("image")))
                            })
                        })
                })
            });
        if let Some(true) = accepts_image {
            saw_metadata = true;
            if let Some(id) = m.get("id").and_then(Value::as_str) {
                vision.push(id.to_owned());
            }
        } else if accepts_image.is_some() {
            saw_metadata = true;
        }
    }
    saw_metadata.then_some(vision)
}

/// Validates and normalizes a base URL: http(s) only, plain HTTP only for loopback hosts.
pub fn normalize_base_url(raw: &str) -> Result<String> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|e| Error::InvalidInput(format!("invalid URL: {e}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::InvalidInput("URL has no host".into()))?;
    let loopback =
        matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") || host.ends_with(".localhost");
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        "http" => {
            return Err(Error::InvalidInput(
                "plain http is only allowed for localhost endpoints".into(),
            ))
        }
        s => return Err(Error::InvalidInput(format!("unsupported URL scheme `{s}`"))),
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

impl OpenAiCompatProvider {
    pub fn new(config: OpenAiCompatConfig, http: reqwest::Client) -> Self {
        Self { config, http }
    }

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

    /// Request against an absolute URL outside the `/v1` API root (server-flavor probes).
    /// Same host as the configured endpoint, so bearer auth still applies.
    fn probe_request(
        &self,
        method: reqwest::Method,
        url: String,
        timeout: Duration,
    ) -> reqwest::RequestBuilder {
        let mut req = self.http.request(method, url).timeout(timeout);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }
        req
    }

    /// `GET /models`, returning the raw document so capability hints can be inspected.
    /// Best effort: many compatible servers implement it, some do not.
    async fn fetch_models_doc(&self) -> Result<Value> {
        let resp = self
            .request(reqwest::Method::GET, "/models")
            .send()
            .await
            .map_err(|e| map_transport(&self.config.base_url, e))?;
        if !resp.status().is_success() {
            return Err(status_error(resp).await);
        }
        read_json(resp, "/models").await
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        Ok(self.discover_models().await?.0)
    }

    /// `GET /models` plus a best-effort vision probe. Returns the model ids and, when the
    /// server exposes capability metadata, the subset that accepts image input (`None` =
    /// undetectable, callers keep any previously cached result).
    pub async fn discover_models(&self) -> Result<(Vec<String>, Option<Vec<String>>)> {
        let doc = self.fetch_models_doc().await?;
        let mut ids: Vec<String> = doc
            .get("data")
            .and_then(Value::as_array)
            .map(|d| {
                d.iter()
                    .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        let vision = self.detect_vision(&doc, &ids).await;
        Ok((ids, vision))
    }

    /// Detection order: hints inside the `/models` document (OpenRouter `architecture.
    /// modality`, LM Studio `type`, generic `*_modalities` fields — top-level or nested
    /// under `architecture` as llama.cpp does), then an Ollama probe (`/api/show` per
    /// model), then LM Studio's `/api/v0/models`. Probes hit the server root, so they
    /// only run against servers that actually answer them.
    async fn detect_vision(&self, doc: &Value, listed: &[String]) -> Option<Vec<String>> {
        if let Some(ids) = vision_flags_from_models(doc) {
            return Some(ids);
        }
        let origin = server_origin(&self.config.base_url)?;
        if let Some(ids) = self.probe_ollama(&origin, listed).await {
            return Some(ids);
        }
        self.probe_lm_studio(&origin).await
    }

    /// Ollama exposes `GET /api/version`; per-model `POST /api/show` reports a
    /// `capabilities` array that includes `"vision"`. Returns `None` if the server is not
    /// Ollama; per-model failures are ignored (that model simply isn't flagged).
    async fn probe_ollama(&self, origin: &str, listed: &[String]) -> Option<Vec<String>> {
        let resp = self
            .probe_request(
                reqwest::Method::GET,
                format!("{origin}/api/version"),
                Duration::from_secs(5),
            )
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        // Ollama answers with {"version": "…"}; a bare 200 from something else is not enough
        // to justify a /api/show request per model.
        let looks_ollama = read_json(resp, "/api/version")
            .await
            .ok()
            .and_then(|v| v.get("version").and_then(Value::as_str).map(str::to_owned))
            .is_some();
        if !looks_ollama {
            return None;
        }
        let mut vision = Vec::new();
        for model in listed {
            let Ok(resp) = self
                .probe_request(
                    reqwest::Method::POST,
                    format!("{origin}/api/show"),
                    Duration::from_secs(10),
                )
                .json(&json!({ "model": model }))
                .send()
                .await
            else {
                continue;
            };
            let Ok(doc) = read_json(resp, "/api/show").await else {
                continue;
            };
            let is_vision = doc
                .get("capabilities")
                .and_then(Value::as_array)
                .is_some_and(|caps| caps.iter().any(|c| c.as_str() == Some("vision")));
            if is_vision {
                vision.push(model.clone());
            }
        }
        Some(vision)
    }

    /// LM Studio's REST API lists `type` per model (`llm` / `vlm` / `embeddings`).
    async fn probe_lm_studio(&self, origin: &str) -> Option<Vec<String>> {
        let resp = self
            .probe_request(
                reqwest::Method::GET,
                format!("{origin}/api/v0/models"),
                Duration::from_secs(5),
            )
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let doc = read_json(resp, "/api/v0/models").await.ok()?;
        let vision: Vec<String> = doc
            .get("data")
            .and_then(Value::as_array)?
            .iter()
            .filter(|m| m.get("type").and_then(Value::as_str) == Some("vlm"))
            .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect();
        Some(vision)
    }

    fn build_body(request: &CompletionRequest, include_usage: bool) -> Value {
        let mut messages = Vec::with_capacity(request.messages.len() + 1);
        if let Some(system) = &request.system {
            messages.push(json!({ "role": "system", "content": system }));
        }
        for m in &request.messages {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => {
                    // One `tool` message per result, linked to its call by id.
                    for b in &m.blocks {
                        if let ContentBlock::ToolResult {
                            call_id, content, ..
                        } = b
                        {
                            messages.push(json!({
                                "role": "tool",
                                "tool_call_id": call_id,
                                "content": tool_result_text(content),
                            }));
                        }
                    }
                    continue;
                }
            };
            let tool_calls: Vec<Value> = m
                .blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                        ..
                    } => Some(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": arguments_string(arguments) },
                    })),
                    _ => None,
                })
                .collect();
            if !tool_calls.is_empty() {
                let text: String = m
                    .blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                // `content: null` (not "") is the canonical form for a pure tool-call turn.
                let content = if text.is_empty() {
                    Value::Null
                } else {
                    Value::String(text)
                };
                messages
                    .push(json!({ "role": role, "content": content, "tool_calls": tool_calls }));
            } else if m
                .blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::Image { .. }))
            {
                // Multimodal form: content is a parts array with text + data-URL images.
                let parts: Vec<Value> = m
                    .blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => {
                            Some(json!({ "type": "text", "text": text }))
                        }
                        ContentBlock::Image { mime, data } => Some(json!({
                            "type": "image_url",
                            "image_url": { "url": format!("data:{mime};base64,{data}") }
                        })),
                        _ => None,
                    })
                    .collect();
                messages.push(json!({ "role": role, "content": parts }));
            } else {
                let text: String = m
                    .blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                messages.push(json!({ "role": role, "content": text }));
            }
        }
        let mut body = json!({ "model": request.model, "messages": messages, "stream": true });
        if include_usage {
            body["stream_options"] = json!({ "include_usage": true });
        }
        if let Some(max) = request.max_output_tokens {
            body["max_tokens"] = json!(max);
        }
        if !request.tools.is_empty() {
            body["tools"] = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect();
        }
        body
    }
}

/// A hostile or buggy server could stream a huge `index`; bound it (arguments are bounded by
/// `MAX_TOOL_ARG_CHARS`).
const MAX_STREAMED_CALLS: usize = 64;

/// Folds one streamed `tool_calls` fragment into `calls`. Fragments are keyed by `index`;
/// servers that omit it are handled by treating a new `id` as a new call.
fn merge_tool_call_delta(calls: &mut Vec<PartialCall>, d: ToolCallDelta) {
    let idx = d.index.unwrap_or_else(|| {
        let new_id = d.id.as_deref().filter(|s| !s.is_empty());
        match (new_id, calls.last()) {
            (Some(id), Some(last)) if last.id != id => calls.len(),
            (_, None) => 0,
            _ => calls.len() - 1,
        }
    });
    if idx >= MAX_STREAMED_CALLS {
        return;
    }
    if calls.len() <= idx {
        calls.resize_with(idx + 1, PartialCall::default);
    }
    let call = &mut calls[idx];
    if let Some(id) = d.id.filter(|s| !s.is_empty()) {
        call.id = id;
    }
    if let Some(f) = d.function {
        if let Some(name) = f.name.filter(|s| !s.is_empty()) {
            if call.name.is_empty() {
                call.name = name;
            } else if call.name != name {
                call.name.push_str(&name);
            }
        }
        match f.arguments {
            Some(Value::String(s)) => {
                let room = MAX_TOOL_ARG_CHARS.saturating_sub(call.arguments.len());
                call.arguments.extend(s.chars().take(room));
            }
            // Some servers send the arguments as an object rather than a JSON string.
            Some(v @ Value::Object(_)) if call.arguments.len() < MAX_TOOL_ARG_CHARS => {
                call.arguments = v.to_string();
            }
            _ => {}
        }
    }
}

#[derive(Deserialize, Default)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<ChunkUsage>,
    #[serde(default)]
    error: Option<Value>,
}
#[derive(Deserialize, Default)]
struct Choice {
    #[serde(default)]
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}
#[derive(Deserialize, Default)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    /// Reasoning models (Qwen3, DeepSeek-R1, …) stream their chain of thought here.
    #[serde(default)]
    reasoning_content: Option<String>,
    /// mlx-lm and OpenRouter carry the same stream under `reasoning` instead.
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}
#[derive(Deserialize)]
struct ToolCallDelta {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}
#[derive(Deserialize)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<Value>,
}
#[derive(Deserialize)]
struct ChunkUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptDetails>,
}
#[derive(Deserialize)]
struct PromptDetails {
    cached_tokens: Option<u64>,
}

impl ModelProvider for OpenAiCompatProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            context_window: self.config.context_window,
            ..super::registry::ProviderKind::OpenAiCompatible.capabilities()
        }
    }

    fn stream(
        &self,
        request: &CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ProviderEvent>> {
        let (tx, rx) = mpsc::channel(PROVIDER_EVENT_BUFFER);
        let body = Self::build_body(request, self.config.include_usage);
        let req = self
            .request(reqwest::Method::POST, "/chat/completions")
            .json(&body);
        let config = self.config.clone();
        spawn_stream(tx.clone(), cancel.clone(), async move {
            run_stream(config, req, &tx, cancel).await
        });
        Ok(rx)
    }

    fn discover(&self) -> ProviderFuture<'_, ModelDiscovery> {
        Box::pin(async move {
            let (models, vision) = self.discover_models().await?;
            // When the server reports capability metadata, every listed model gets a
            // definite flag; otherwise details stay empty and cached flags are kept.
            let details = match vision {
                Some(v) => models
                    .iter()
                    .map(|m| {
                        let d = DiscoveredModel {
                            vision: Some(v.contains(m)),
                            ..Default::default()
                        };
                        (m.clone(), d)
                    })
                    .collect(),
                None => Default::default(),
            };
            Ok(ModelDiscovery { models, details })
        })
    }
}

async fn run_stream(
    config: OpenAiCompatConfig,
    req: reqwest::RequestBuilder,
    tx: &mpsc::Sender<ProviderEvent>,
    cancel: CancellationToken,
) -> Result<()> {
    let resp = send_checked(req, &config.base_url, config.connect_timeout).await?;

    let mut usage: Option<Usage> = None;
    let mut finish: Option<StopReason> = None;
    let mut saw_done = false;
    let mut calls: Vec<PartialCall> = Vec::new();

    let end = pump_sse(resp, config.idle_timeout, &cancel, tx, |ev, out| {
        if ev.data.trim() == "[DONE]" {
            saw_done = true;
            return Ok(());
        }
        let Ok(parsed) = serde_json::from_str::<Chunk>(&ev.data) else {
            return Ok(()); // tolerate keep-alive / vendor extensions we don't model
        };
        if let Some(err) = parsed.error {
            return Err(Error::Provider(
                extract_error_message(&err).unwrap_or_else(|| err.to_string()),
            ));
        }
        for choice in parsed.choices {
            let reasoning = choice.delta.reasoning_content.or(choice.delta.reasoning);
            if let Some(text) = reasoning.filter(|t| !t.is_empty()) {
                out.push(ProviderEvent::ReasoningDelta(text));
            }
            if let Some(text) = choice.delta.content.filter(|t| !t.is_empty()) {
                out.push(ProviderEvent::TextDelta(text));
            }
            for d in choice.delta.tool_calls.into_iter().flatten() {
                merge_tool_call_delta(&mut calls, d);
            }
            if let Some(reason) = choice.finish_reason {
                finish = Some(match reason.as_str() {
                    "length" => StopReason::MaxTokens,
                    "tool_calls" | "function_call" => StopReason::ToolUse,
                    _ => StopReason::EndTurn,
                });
            }
        }
        if let Some(u) = parsed.usage {
            usage = Some(Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                cache_read_tokens: u.prompt_tokens_details.and_then(|d| d.cached_tokens),
                cache_write_tokens: None,
            });
        }
        Ok(())
    })
    .await?;
    if end == StreamEnd::Abandoned {
        return Ok(());
    }

    // Some servers (older Ollama, some vLLM templates) finish with "stop" even when they
    // emitted tool calls, so the presence of calls decides the stop reason. Calls cut off by
    // the output limit are dropped: their arguments are truncated JSON.
    let mut calls: Vec<PartialCall> = calls.into_iter().filter(|c| !c.name.is_empty()).collect();
    if finish == Some(StopReason::MaxTokens) {
        calls.clear();
    } else if !calls.is_empty() && (finish.is_some() || saw_done) {
        finish = Some(StopReason::ToolUse);
    }
    // The model owns call ids; dedupe them because approvals and result pairing key on id.
    normalize_call_ids(&mut calls);
    for c in calls.into_iter() {
        let ev = ProviderEvent::ToolCall {
            id: c.id,
            name: c.name,
            arguments: c.arguments,
        };
        if tx.send(ev).await.is_err() {
            return Ok(());
        }
    }
    if let Some(u) = usage {
        let _ = tx.send(ProviderEvent::Usage(u)).await;
    }
    match finish {
        Some(reason) => {
            let _ = tx.send(ProviderEvent::Done(reason)).await;
            Ok(())
        }
        None if saw_done => {
            let _ = tx.send(ProviderEvent::Done(StopReason::EndTurn)).await;
            Ok(())
        }
        None => Err(Error::Provider(
            "stream ended without a completion marker".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Message;

    #[test]
    fn base_url_rules() {
        assert_eq!(
            normalize_base_url("http://localhost:11434/v1/").unwrap(),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.example.com/v1").unwrap(),
            "https://api.example.com/v1"
        );
        assert!(normalize_base_url("http://api.example.com/v1").is_err());
        assert!(normalize_base_url("ftp://localhost/v1").is_err());
        assert!(normalize_base_url("not a url").is_err());
    }

    #[test]
    fn body_shape() {
        let req = CompletionRequest {
            model: "m".into(),
            system: Some("sys".into()),
            messages: vec![Message::text(Role::User, "hi")],
            max_output_tokens: Some(100),
            tools: Vec::new(),
        };
        let body = OpenAiCompatProvider::build_body(&req, true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hi");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["max_tokens"], 100);
    }

    #[test]
    fn body_with_image_uses_parts_array() {
        let req = CompletionRequest {
            model: "m".into(),
            system: None,
            messages: vec![Message {
                role: Role::User,
                blocks: vec![
                    ContentBlock::Text {
                        text: "what is this?".into(),
                    },
                    ContentBlock::Image {
                        mime: "image/png".into(),
                        data: "aGk=".into(),
                    },
                ],
            }],
            max_output_tokens: None,
            tools: Vec::new(),
        };
        let body = OpenAiCompatProvider::build_body(&req, false);
        let parts = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0], json!({ "type": "text", "text": "what is this?" }));
        assert_eq!(
            parts[1],
            json!({ "type": "image_url", "image_url": { "url": "data:image/png;base64,aGk=" } })
        );
        // Text-only messages keep the plain string form for maximum compatibility.
        let text_only = OpenAiCompatProvider::build_body(
            &CompletionRequest {
                model: "m".into(),
                system: None,
                messages: vec![Message::text(Role::User, "hi")],
                max_output_tokens: None,
                tools: Vec::new(),
            },
            false,
        );
        assert_eq!(text_only["messages"][0]["content"], "hi");
    }

    #[test]
    fn body_with_tools_tool_calls_and_tool_results() {
        use crate::providers::ToolSpec;
        let req = CompletionRequest {
            model: "m".into(),
            system: None,
            messages: vec![
                Message::text(Role::User, "list files"),
                Message {
                    role: Role::Assistant,
                    blocks: vec![ContentBlock::ToolCall {
                        id: "c1".into(),
                        name: "fs__list".into(),
                        arguments: json!({ "path": "/tmp" }),
                        server: Some("fs".into()),
                        tool: Some("list".into()),
                    }],
                },
                Message {
                    role: Role::Tool,
                    blocks: vec![ContentBlock::ToolResult {
                        call_id: "c1".into(),
                        content: json!([{ "type": "text", "text": "a.txt" }]),
                        is_error: false,
                        status: None,
                    }],
                },
            ],
            max_output_tokens: None,
            tools: vec![ToolSpec {
                name: "fs__list".into(),
                description: "List a directory".into(),
                parameters: json!({ "type": "object" }),
            }],
        };
        let body = OpenAiCompatProvider::build_body(&req, false);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "fs__list");
        let asst = &body["messages"][1];
        assert_eq!(asst["role"], "assistant");
        assert!(asst["content"].is_null());
        assert_eq!(asst["tool_calls"][0]["id"], "c1");
        assert_eq!(
            asst["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"/tmp"}"#
        );
        let tool = &body["messages"][2];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_call_id"], "c1");
        assert_eq!(tool["content"], "a.txt");
        // No tools → no `tools` key (some servers reject an empty array).
        let plain = OpenAiCompatProvider::build_body(
            &CompletionRequest {
                tools: Vec::new(),
                ..req
            },
            false,
        );
        assert!(plain.get("tools").is_none());
    }

    #[test]
    fn tool_call_fragments_merge_by_index_and_by_id() {
        let d = |v: Value| serde_json::from_value::<ToolCallDelta>(v).unwrap();
        let mut calls = Vec::new();
        merge_tool_call_delta(
            &mut calls,
            d(json!({"index":0,"id":"a","function":{"name":"x","arguments":"{\"p\":"}})),
        );
        merge_tool_call_delta(
            &mut calls,
            d(json!({"index":1,"id":"b","function":{"name":"y","arguments":"{}"}})),
        );
        merge_tool_call_delta(
            &mut calls,
            d(json!({"index":0,"function":{"arguments":"1}"}})),
        );
        assert_eq!(calls.len(), 2);
        assert_eq!((calls[0].id.as_str(), calls[0].name.as_str()), ("a", "x"));
        assert_eq!(calls[0].arguments, r#"{"p":1}"#);
        assert_eq!(calls[1].name, "y");

        // No index: a new id starts a new call; fragments without id extend the last one.
        let mut calls = Vec::new();
        merge_tool_call_delta(
            &mut calls,
            d(json!({"id":"a","function":{"name":"x","arguments":{"k":1}}})),
        );
        merge_tool_call_delta(&mut calls, d(json!({"id":"b","function":{"name":"y"}})));
        merge_tool_call_delta(&mut calls, d(json!({"function":{"arguments":"{}"}})));
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments, r#"{"k":1}"#);
        assert_eq!(calls[1].arguments, "{}");
    }

    #[test]
    fn streamed_call_index_and_arguments_are_bounded() {
        let d = |v: Value| serde_json::from_value::<ToolCallDelta>(v).unwrap();
        let mut calls = Vec::new();
        // A huge index must not allocate.
        merge_tool_call_delta(
            &mut calls,
            d(json!({"index": 4_000_000_000usize, "id": "x"})),
        );
        assert!(calls.is_empty());
        merge_tool_call_delta(
            &mut calls,
            d(json!({"index": 0, "function": {"arguments": "y".repeat(MAX_TOOL_ARG_CHARS + 100)}})),
        );
        assert_eq!(calls[0].arguments.len(), MAX_TOOL_ARG_CHARS);
    }

    #[test]
    fn opaque_provider_state_is_never_sent() {
        let opaque = || ContentBlock::ProviderOpaque {
            provider: "google_gemini".into(),
            data: json!({ "sig": "c2VjcmV0LXNpZw==" }),
        };
        let req = CompletionRequest {
            model: "m".into(),
            system: None,
            messages: vec![
                Message::text(Role::User, "hi"),
                Message {
                    role: Role::Assistant,
                    blocks: vec![
                        ContentBlock::Text { text: "ok".into() },
                        opaque(),
                        ContentBlock::ToolCall {
                            id: "c1".into(),
                            name: "t".into(),
                            arguments: json!({}),
                            server: None,
                            tool: None,
                        },
                    ],
                },
                Message {
                    role: Role::Assistant,
                    blocks: vec![
                        ContentBlock::Text {
                            text: "done".into(),
                        },
                        opaque(),
                    ],
                },
            ],
            max_output_tokens: None,
            tools: Vec::new(),
        };
        let body = OpenAiCompatProvider::build_body(&req, false).to_string();
        assert!(!body.contains("c2VjcmV0LXNpZw=="), "{body}");
        assert!(!body.contains("google_gemini"), "{body}");
    }

    #[test]
    fn vision_flags_parse_openrouter_modality() {
        let doc = json!({
            "data": [
                { "id": "openai/gpt-4o", "architecture": { "modality": "text+image->text" } },
                { "id": "meta/llama-3.1", "architecture": { "modality": "text->text" } },
                { "id": "no-meta" }
            ]
        });
        assert_eq!(
            vision_flags_from_models(&doc),
            Some(vec!["openai/gpt-4o".to_string()])
        );
        assert_eq!(
            vision_flags_from_models(&json!({"data":[{"id":"m"}]})),
            None
        );
        assert_eq!(vision_flags_from_models(&json!({})), None);
    }

    #[test]
    fn vision_flags_parse_llamacpp_architecture_modalities() {
        // llama.cpp's /v1/models nests modalities inside `architecture`, with no
        // `modality` string or top-level `type` field.
        let doc = json!({
            "data": [
                {
                    "id": "ggml-org/gemma-4-26B-A4B-it-GGUF:Q4_0",
                    "object": "model",
                    "owned_by": "llamacpp",
                    "architecture": {
                        "input_modalities": ["text", "image", "video"],
                        "output_modalities": ["text"]
                    }
                },
                {
                    "id": "unsloth/GLM-4.7-Flash-GGUF:Q4_K_M",
                    "object": "model",
                    "owned_by": "llamacpp",
                    "architecture": {
                        "input_modalities": ["text"],
                        "output_modalities": ["text"]
                    }
                }
            ],
            "object": "list"
        });
        assert_eq!(
            vision_flags_from_models(&doc),
            Some(vec!["ggml-org/gemma-4-26B-A4B-it-GGUF:Q4_0".to_string()])
        );
    }
}
