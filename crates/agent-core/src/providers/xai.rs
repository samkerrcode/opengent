//! xAI adapter for Grok models, authenticated with an xAI API key. xAI exposes the OpenAI
//! Responses protocol (`POST {base}/responses`), so this adapter reuses `providers::openai`'s
//! request/stream machinery with its own `Dialect`: a separate opaque-state family (Grok's
//! encrypted reasoning is never replayed to another vendor), the `/language-models` listing
//! and Grok model rules.
//!
//! Requests are stateless (`store: false`): the full local history is sent as input items
//! each turn. Reasoning models are asked for `include: ["reasoning.encrypted_content"]` and
//! `reasoning.summary` (accepted as a compatibility field; Grok's thinking is otherwise
//! opaque). Returned reasoning items are replayed verbatim through `ProviderOpaque` blocks,
//! which keeps the model's earlier reasoning across turns and tool calls — for `grok-4.7`
//! and newer they arrive whether or not `include` is sent.

use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::capabilities::looks_like_vision_model;
use super::openai::{Dialect, OpenAiConfig, OpenAiProvider};
use super::registry::ProviderKind;
use super::{
    Capabilities, CompletionRequest, DiscoveredModel, ModelDiscovery, ModelProvider, ProviderEvent,
    ProviderFuture,
};
use crate::Result;

/// Opaque-state family tag. Stable: stored history references it.
pub const FAMILY: &str = "xai_responses";
pub const DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";

#[derive(Debug, Clone)]
pub struct XAiConfig {
    pub id: String,
    /// e.g. `https://api.x.ai/v1` — `/responses` and `/language-models` are appended.
    pub base_url: String,
    pub api_key: Option<String>,
    pub connect_timeout: Duration,
    /// Maximum silence between stream events. Same rationale as OpenAI's: reasoning models
    /// can think for a long time before the first event. The whole-request deadline is 4×
    /// this (`common::request_timeout`), so 225 s gives the 900 s xAI recommends for
    /// high-effort reasoning.
    pub idle_timeout: Duration,
}

impl XAiConfig {
    pub fn new(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            api_key: None,
            connect_timeout: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(225),
        }
    }
}

/// xAI's dialect of the Responses protocol.
static DIALECT: Dialect = Dialect {
    kind: ProviderKind::XAi,
    vendor: "xAI",
    family: FAMILY,
    image_mimes: Some(&["image/jpeg", "image/jpg", "image/png"]),
    models_path: "/language-models",
    parse_models,
    is_reasoning_model,
    known_context_window,
    known_max_output,
};

/// Bare model name: aggregator-style `xai/` prefixes stripped, lowercased.
fn model_name(model: &str) -> String {
    let lower = model.to_ascii_lowercase();
    lower
        .strip_prefix("xai/")
        .map(str::to_owned)
        .unwrap_or(lower)
}

/// The leading number in a `grok-N…` id (`grok-4.7` → 4, `grok-4-0709` → 4,
/// `grok-420-reasoning` → 420).
fn grok_major(m: &str) -> Option<u32> {
    let digits: String = m
        .strip_prefix("grok-")?
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// A flagship release id: `grok-N` or `grok-N.M`, plus optional `-latest` or `-<digits>`
/// date suffixes (`grok-4-0709`). Specialist families (`-fast`, `-mini`, `grok-420-*`,
/// `*-reasoning` variants, multi-agent) don't match.
fn is_flagship(m: &str) -> bool {
    let Some(rest) = m.strip_prefix("grok-") else {
        return false;
    };
    let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let mut parts = rest.split('-');
    let version_ok = parts.next().is_some_and(|v| v.split('.').all(numeric));
    version_ok && parts.all(|suf| suf == "latest" || numeric(suf))
}

/// Current Grok models reason (effort is configurable, not disableable), so unknown and new
/// names (`grok-build-0.1`, aliases) default to reasoning: xAI accepts the reasoning
/// options as compatibility fields, while omitting `include` would lose the model's
/// reasoning across tool calls. Only `*-non-reasoning` variants and the legacy Grok ≤ 3 /
/// `*-beta` models are excluded (`grok-3-mini` reasons).
pub fn is_reasoning_model(model: &str) -> bool {
    let m = model_name(model);
    if m.contains("non-reasoning")
        || m.starts_with("grok-beta")
        || m.starts_with("grok-vision-beta")
    {
        return false;
    }
    m.starts_with("grok-3-mini") || !grok_major(&m).is_some_and(|major| major <= 3)
}

/// Vision-capable families: flagship Grok releases are multimodal; specialist variants
/// (`grok-420-*`, `-fast`, `-mini`) are text-only. Older vision models are caught by the
/// generic name heuristic (`grok-2-vision`).
fn accepts_images(model: &str) -> bool {
    let m = model_name(model);
    looks_like_vision_model(&m) || is_flagship(&m)
}

/// Best-known input budget per family — `/language-models` reports no context length, so
/// without this every Grok model would fall back to the 8K default. Values are conservative;
/// the user's per-model setting always wins.
pub fn known_context_window(model: &str) -> Option<u32> {
    let m = model_name(model);
    if m.starts_with("grok-build") {
        return Some(500_000);
    }
    if m.contains("-fast") && grok_major(&m) == Some(4) {
        return Some(2_000_000); // grok-4-fast / grok-4-1-fast
    }
    Some(match grok_major(&m)? {
        4 if m.starts_with("grok-4.5")
            || m.starts_with("grok-4.6")
            || m.starts_with("grok-4.7") =>
        {
            500_000
        }
        // grok-4.2* and grok-4.3 lines; undotted `grok-4NN-*` ids are the same era.
        4 if m.starts_with("grok-4.2") || m.starts_with("grok-4.3") => 1_000_000,
        4 => 256_000,
        3 | 2 => 131_072,
        1 => 128_000,
        major if major >= 400 => 1_000_000,
        _ => return None,
    })
}

/// xAI documents no per-model output cap — `max_output_tokens` defaults to 128K and the API
/// accepts larger values — so the compiler's budget is never clamped.
fn known_max_output(_: &str) -> Option<u32> {
    None
}

/// Parses a `GET /language-models` document into chat models with their metadata. The
/// listing already only covers chat and image-understanding models; `input_modalities`
/// reports vision and `capabilities.reasoning_effort` marks reasoning models, with the name
/// rules as fallback for entries that omit them.
fn parse_models(doc: &Value) -> Vec<(String, DiscoveredModel)> {
    let mut models: Vec<(String, DiscoveredModel)> = doc
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?;
            let has = |key: &str, want: &str| {
                m.get(key)
                    .and_then(Value::as_array)
                    .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(want)))
            };
            if m.get("output_modalities")
                .and_then(Value::as_array)
                .is_some()
                && !has("output_modalities", "text")
            {
                return None; // anything without text output isn't a chat model
            }
            let details = DiscoveredModel {
                context_window: m
                    .get("context_length")
                    .and_then(Value::as_u64)
                    .and_then(|w| u32::try_from(w).ok())
                    .or_else(|| known_context_window(id)),
                max_output_tokens: None,
                vision: Some(if m.get("input_modalities").is_some() {
                    has("input_modalities", "image")
                } else {
                    accepts_images(id)
                }),
                reasoning: Some(
                    m.pointer("/capabilities/reasoning_effort")
                        .and_then(Value::as_array)
                        .map(|e| !e.is_empty())
                        .unwrap_or_else(|| is_reasoning_model(id)),
                ),
            };
            Some((id.to_owned(), details))
        })
        .collect();
    models.sort_by(|a, b| a.0.cmp(&b.0));
    models.dedup_by(|a, b| a.0 == b.0);
    models
}

pub struct XAiProvider {
    inner: OpenAiProvider,
}

impl XAiProvider {
    pub fn new(config: XAiConfig, http: reqwest::Client) -> Self {
        let mut c = OpenAiConfig::new(config.id, config.base_url);
        c.api_key = config.api_key;
        c.connect_timeout = config.connect_timeout;
        c.idle_timeout = config.idle_timeout;
        c.dialect = &DIALECT;
        Self {
            inner: OpenAiProvider::new(c, http),
        }
    }

    /// Builds the `responses.create` body for this dialect (unit-tested).
    pub fn build_body(request: &CompletionRequest) -> Value {
        OpenAiProvider::build_body(&DIALECT, request)
    }
}

impl ModelProvider for XAiProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn family(&self) -> Option<&'static str> {
        Some(FAMILY)
    }

    fn stream(
        &self,
        request: &CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ProviderEvent>> {
        self.inner.stream(request, cancel)
    }

    fn discover(&self) -> ProviderFuture<'_, ModelDiscovery> {
        self.inner.discover()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ContentBlock, Message, Role};
    use crate::providers::ToolSpec;
    use serde_json::json;

    fn request(model: &str, messages: Vec<Message>) -> CompletionRequest {
        CompletionRequest {
            model: model.into(),
            system: Some("sys".into()),
            messages,
            max_output_tokens: Some(256),
            tools: vec![ToolSpec {
                name: "fs__read".into(),
                description: "Read a file".into(),
                parameters: json!({ "type": "object", "properties": {} }),
            }],
        }
    }

    #[test]
    fn body_is_stateless_and_asks_for_encrypted_reasoning() {
        let body =
            XAiProvider::build_body(&request("grok-4.7", vec![Message::text(Role::User, "hi")]));
        assert_eq!(body["model"], "grok-4.7");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["instructions"], "sys");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["max_output_tokens"], 256, "no output cap is clamped");

        let body = XAiProvider::build_body(&request(
            "grok-4.20-0309-non-reasoning",
            vec![Message::text(Role::User, "hi")],
        ));
        assert!(
            body.get("include").is_none() && body.get("reasoning").is_none(),
            "non-reasoning models get no reasoning options: {body}"
        );
    }

    #[test]
    fn replays_own_opaque_state_and_drops_other_vendors() {
        let messages = vec![
            Message::text(Role::User, "hi"),
            Message {
                role: Role::Assistant,
                blocks: vec![
                    ContentBlock::ProviderOpaque {
                        provider: FAMILY.into(),
                        data: json!({ "item": { "type": "reasoning", "summary": [],
                            "encrypted_content": "XENC" } }),
                    },
                    ContentBlock::Text { text: "ok".into() },
                    ContentBlock::ProviderOpaque {
                        provider: "openai_responses".into(),
                        data: json!({ "item": { "type": "reasoning", "summary": [],
                            "encrypted_content": "OENC" } }),
                    },
                ],
            },
        ];
        let body = XAiProvider::build_body(&request("grok-4.7", messages));
        let input = body["input"].as_array().unwrap();
        let reasoning: Vec<&Value> = input.iter().filter(|i| i["type"] == "reasoning").collect();
        assert_eq!(reasoning.len(), 1, "foreign state dropped: {input:?}");
        assert_eq!(reasoning[0]["encrypted_content"], "XENC");
        assert!(
            input.iter().all(|i| i.get("id").is_none()),
            "no item ids under store:false"
        );
    }

    #[test]
    fn model_rules() {
        for m in [
            "grok-4.7",
            "grok-4.3",
            "grok-4-0709",
            "grok-4-1-fast-reasoning",
            "grok-420-reasoning",
            "grok-3-mini-latest",
            "grok-code-fast-1",
            "grok-build-0.1",
            "xai/grok-4.20-beta",
            "latest",
        ] {
            assert!(is_reasoning_model(m), "{m}");
        }
        for m in [
            "grok-4.20-0309-non-reasoning",
            "grok-4-fast-non-reasoning",
            "grok-3",
            "grok-2-vision-1212",
            "grok-beta",
            "grok-vision-beta",
        ] {
            assert!(!is_reasoning_model(m), "{m}");
        }
        for m in [
            "grok-4.7",
            "grok-4-0709",
            "grok-4.1-latest",
            "grok-2-vision",
        ] {
            assert!(accepts_images(m), "{m}");
        }
        for m in ["grok-420-reasoning", "grok-3-mini", "grok-4-fast-reasoning"] {
            assert!(!accepts_images(m), "{m}");
        }
        assert_eq!(known_context_window("grok-4.7"), Some(500_000));
        assert_eq!(known_context_window("grok-build-0.1"), Some(500_000));
        assert_eq!(known_context_window("grok-4.3"), Some(1_000_000));
        assert_eq!(
            known_context_window("grok-4.20-multi-agent-0309"),
            Some(1_000_000)
        );
        assert_eq!(known_context_window("grok-420-reasoning"), Some(1_000_000));
        assert_eq!(
            known_context_window("grok-4-1-fast-reasoning"),
            Some(2_000_000)
        );
        assert_eq!(known_context_window("grok-4"), Some(256_000));
        assert_eq!(known_context_window("grok-3"), Some(131_072));
        assert_eq!(known_context_window("mystery"), None);
    }

    #[test]
    fn listing_uses_server_modalities_and_effort() {
        let doc = json!({ "models": [
            { "id": "grok-4.7", "input_modalities": ["text", "image"],
              "output_modalities": ["text"], "capabilities": {
                "reasoning_effort": ["low", "medium", "high"], "default_reasoning_effort": "high" } },
            { "id": "grok-420-reasoning", "input_modalities": ["text"],
              "output_modalities": ["text"], "capabilities": {
                "reasoning_effort": ["low", "medium", "high", "xhigh"] } },
            { "id": "grok-imagine-image", "input_modalities": ["text"],
              "output_modalities": ["image"], "image_price": 200000000 },
            { "id": "grok-2", "capabilities": {} },
            { "id": "grok-build-0.1", "input_modalities": ["text"], "output_modalities": ["text"] },
            { "id": "grok-4.20-0309-non-reasoning", "output_modalities": ["text"],
              "capabilities": { "reasoning_effort": ["low"] } }
        ]});
        let models = parse_models(&doc);
        let ids: Vec<&str> = models.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "grok-2",
                "grok-4.20-0309-non-reasoning",
                "grok-4.7",
                "grok-420-reasoning",
                "grok-build-0.1"
            ]
        );
        let d = |id: &str| &models.iter().find(|(m, _)| m == id).unwrap().1;
        assert_eq!(d("grok-4.7").vision, Some(true));
        assert_eq!(d("grok-4.7").reasoning, Some(true));
        assert_eq!(d("grok-4.7").context_window, Some(500_000));
        assert_eq!(
            d("grok-420-reasoning").vision,
            Some(false),
            "server-reported text-only beats the name rules"
        );
        assert_eq!(d("grok-2").reasoning, Some(false));
        assert_eq!(
            d("grok-2").context_window,
            Some(131_072),
            "name-table fallback"
        );
        assert_eq!(
            d("grok-build-0.1").reasoning,
            Some(true),
            "unreported reasoning defaults on for current models"
        );
        assert_eq!(d("grok-build-0.1").context_window, Some(500_000));
        assert_eq!(
            d("grok-4.20-0309-non-reasoning").reasoning,
            Some(true),
            "the server report beats the name rules"
        );
    }

    #[test]
    fn unsupported_image_formats_become_a_note() {
        let messages = vec![Message {
            role: Role::User,
            blocks: vec![
                ContentBlock::Image {
                    mime: "image/png".into(),
                    data: "aGk=".into(),
                },
                ContentBlock::Image {
                    mime: "image/webp".into(),
                    data: "aGk=".into(),
                },
            ],
        }];
        let body = XAiProvider::build_body(&request("grok-4.7", messages));
        let content = &body["input"][0]["content"];
        assert_eq!(content[0]["type"], "input_image");
        assert_eq!(content[1]["type"], "input_text");
        let note = content[1]["text"].as_str().unwrap();
        assert!(
            note.contains("image/webp") && note.contains("xAI"),
            "{note}"
        );
    }
}
