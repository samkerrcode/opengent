//! OpenAI-compatible Chat Completions adapter. This is the protocol spoken by most local
//! servers (Ollama, LM Studio, vLLM, llama.cpp) and many hosted gateways. Tool calls are
//! parsed in Phase 3; today unknown delta fields are ignored.

use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::sse::SseParser;
use super::{Capabilities, CompletionRequest, ModelProvider, ProviderEvent, PROVIDER_EVENT_BUFFER};
use crate::domain::{ContentBlock, Role, StopReason, Usage};
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
            .timeout(self.config.idle_timeout.max(self.config.connect_timeout) * 4);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }
        req
    }

    /// `GET /models`. Best effort: many compatible servers implement it, some do not.
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let resp = self
            .request(reqwest::Method::GET, "/models")
            .send()
            .await
            .map_err(|e| map_transport(&self.config, e))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(map_status(status, &body));
        }
        #[derive(Deserialize)]
        struct ModelsResponse {
            data: Vec<ModelEntry>,
        }
        #[derive(Deserialize)]
        struct ModelEntry {
            id: String,
        }
        let parsed: ModelsResponse = resp
            .json()
            .await
            .map_err(|e| Error::Provider(format!("unexpected /models response: {e}")))?;
        let mut ids: Vec<String> = parsed.data.into_iter().map(|m| m.id).collect();
        ids.sort();
        Ok(ids)
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
                Role::Tool => "tool",
            };
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
        let mut body = json!({ "model": request.model, "messages": messages, "stream": true });
        if include_usage {
            body["stream_options"] = json!({ "include_usage": true });
        }
        if let Some(max) = request.max_output_tokens {
            body["max_tokens"] = json!(max);
        }
        body
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
            streaming: true,
            context_window: self.config.context_window,
            ..Default::default()
        }
    }

    fn stream(
        &self,
        request: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ProviderEvent>> {
        let (tx, rx) = mpsc::channel(PROVIDER_EVENT_BUFFER);
        let body = Self::build_body(&request, self.config.include_usage);
        let req = self
            .request(reqwest::Method::POST, "/chat/completions")
            .json(&body);
        let config = self.config.clone();

        tokio::spawn(async move {
            let result = tokio::select! {
                _ = cancel.cancelled() => return,
                r = run_stream(config, req, &tx, cancel.clone()) => r,
            };
            if let Err(e) = result {
                let _ = tx.send(ProviderEvent::Error(e.to_string())).await;
            }
        });
        Ok(rx)
    }
}

async fn run_stream(
    config: OpenAiCompatConfig,
    req: reqwest::RequestBuilder,
    tx: &mpsc::Sender<ProviderEvent>,
    cancel: CancellationToken,
) -> Result<()> {
    let resp = tokio::time::timeout(config.connect_timeout, req.send())
        .await
        .map_err(|_| Error::Provider(format!("timed out connecting to {}", config.base_url)))?
        .map_err(|e| map_transport(&config, e))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(map_status(status, &body));
    }

    let mut resp = resp;
    let mut parser = SseParser::new();
    let mut usage: Option<Usage> = None;
    let mut finish: Option<StopReason> = None;
    let mut saw_done = false;

    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            c = tokio::time::timeout(config.idle_timeout, resp.chunk()) => match c {
                Err(_) => return Err(Error::Provider(format!("no data from provider for {:?}", config.idle_timeout))),
                Ok(Err(e)) => return Err(Error::Provider(format!("stream error: {e}"))),
                Ok(Ok(None)) => break,
                Ok(Ok(Some(bytes))) => bytes,
            },
        };
        for ev in parser
            .feed(&chunk)
            .map_err(|e| Error::Provider(e.to_string()))?
        {
            if ev.data.trim() == "[DONE]" {
                saw_done = true;
                continue;
            }
            let parsed: Chunk = match serde_json::from_str(&ev.data) {
                Ok(c) => c,
                Err(_) => continue, // tolerate keep-alive / vendor extensions we don't model
            };
            if let Some(err) = parsed.error {
                return Err(Error::Provider(
                    extract_error_message(&err).unwrap_or_else(|| err.to_string()),
                ));
            }
            for choice in parsed.choices {
                if let Some(text) = choice.delta.reasoning_content.filter(|t| !t.is_empty()) {
                    if tx.send(ProviderEvent::ReasoningDelta(text)).await.is_err() {
                        return Ok(());
                    }
                }
                if let Some(text) = choice.delta.content.filter(|t| !t.is_empty()) {
                    if tx.send(ProviderEvent::TextDelta(text)).await.is_err() {
                        return Ok(());
                    }
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

/// Walks the error source chain looking for a TLS certificate failure. These surface as
/// connect-phase errors in reqwest, so they must be distinguished from "server unreachable".
fn tls_cert_error(e: &reqwest::Error) -> bool {
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        if s.downcast_ref::<rustls::Error>().is_some() {
            return true;
        }
        // The platform verifier may wrap OS-specific errors (e.g. Security.framework codes)
        // rather than rustls::Error; fall back to the message.
        let msg = s.to_string().to_lowercase();
        if msg.contains("certificate") || msg.contains("self signed") || msg.contains("self-signed")
        {
            return true;
        }
        src = s.source();
    }
    false
}

fn map_transport(config: &OpenAiCompatConfig, e: reqwest::Error) -> Error {
    if tls_cert_error(&e) {
        Error::Provider(format!(
            "TLS certificate verification failed for {} — the server presents a self-signed or \
             untrusted certificate. Trust its certificate in the OS, paste it as a custom CA in \
             provider settings, or set TLS verification to 'skip verification'.",
            config.base_url
        ))
    } else if e.is_connect() {
        Error::Provider(format!(
            "could not connect to {} — is the server running?",
            config.base_url
        ))
    } else if e.is_timeout() {
        Error::Provider(format!("request to {} timed out", config.base_url))
    } else {
        Error::Provider(format!("request failed: {e}"))
    }
}

fn map_status(status: StatusCode, body: &str) -> Error {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| extract_error_message(&v))
        .unwrap_or_else(|| body.chars().take(300).collect::<String>().trim().to_owned());
    let hint = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            "authentication failed — check the API key"
        }
        StatusCode::NOT_FOUND => "endpoint or model not found — check the base URL and model name",
        StatusCode::TOO_MANY_REQUESTS => "rate limited by the provider",
        s if s.is_server_error() => "provider server error",
        _ => "provider rejected the request",
    };
    if detail.is_empty() {
        Error::Provider(format!("{hint} (HTTP {})", status.as_u16()))
    } else {
        Error::Provider(format!("{hint} (HTTP {}): {detail}", status.as_u16()))
    }
}

fn extract_error_message(v: &Value) -> Option<String> {
    v.get("error")
        .and_then(|e| e.get("message").or(Some(e)))
        .or_else(|| v.get("message"))
        .and_then(|m| m.as_str().map(str::to_owned))
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
        };
        let body = OpenAiCompatProvider::build_body(&req, true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hi");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["max_tokens"], 100);
    }

    #[test]
    fn status_mapping_extracts_message() {
        let e = map_status(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"bad key","type":"auth"}}"#,
        );
        assert!(e.to_string().contains("bad key"));
        assert!(e.to_string().contains("401"));
    }
}
