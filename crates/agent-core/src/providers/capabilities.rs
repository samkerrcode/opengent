//! Model-level capability resolution. `ModelProvider::capabilities()` describes the adapter;
//! whether a *model* accepts image input is a per-model property that lives in provider
//! config: an explicit user tri-state first, then server-reported `vision_models`, then a
//! name heuristic for well-known families.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How the provider config treats image input. `Auto` (default) combines server-reported
/// vision flags with a name heuristic; `Always`/`Never` are explicit user overrides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ImageSupport {
    #[default]
    Auto,
    Always,
    Never,
}

/// Distinctive name fragments for model families known to accept image input. This is a
/// fallback for servers that report no capability metadata — false positives are possible
/// (e.g. a fine-tune named `my-vision-lora` that is text-only), which is why the
/// `ImageSupport` tri-state exists as an override.
const VISION_MARKERS: &[&str] = &[
    "vision",
    "-vl",
    "vl-",
    "llava",
    "pixtral",
    "moondream",
    "minicpm-v",
    "internvl",
    "glm-4v",
    "gpt-4o",
    "gpt-4.1",
    "gpt-4-turbo",
    "gpt-5",
    "chatgpt-4o",
    "gemini",
    "gemma3",
    "phi-3.5-vision",
    "phi-4-multimodal",
    "mistral-small-3",
];

/// Prefixes matched with `starts_with` — too short to be safe as substrings.
const VISION_PREFIXES: &[&str] = &["o3-", "o4-", "qwen-vl"];

/// Best-effort guess: does this model name look like a vision-capable family?
pub fn looks_like_vision_model(model: &str) -> bool {
    let name = model.to_ascii_lowercase();
    // Claude 3+ ids take several shapes (claude-3-opus, claude-sonnet-4, anthropic/
    // claude-haiku-4.5); anything "claude" carrying a major version ≥ 3 accepts images.
    if name.contains("claude") && name.chars().any(|c| ('3'..='9').contains(&c)) {
        return true;
    }
    VISION_PREFIXES.iter().any(|p| name.starts_with(p))
        || VISION_MARKERS.iter().any(|m| name.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_matches_known_families() {
        for name in [
            "gpt-4o",
            "gpt-4o-mini",
            "openai/gpt-4.1",
            "o3-mini",
            "o4-mini",
            "anthropic/claude-sonnet-4",
            "claude-3.5-sonnet",
            "gemini-2.0-flash",
            "gemma3:27b",
            "llava:13b",
            "llama3.2-vision",
            "qwen2.5-vl:7b",
            "qwen-vl-max",
            "pixtral-large",
            "moondream",
            "minicpm-v:8b",
            "internvl2-26b",
            "glm-4v-9b",
            "phi-4-multimodal-instruct",
            "mistral-small-3.1",
        ] {
            assert!(looks_like_vision_model(name), "{name} should match");
        }
    }

    #[test]
    fn heuristic_rejects_text_models() {
        for name in [
            "llama3.1",
            "mistral",
            "qwen2.5",
            "deepseek-r1",
            "phi-4",
            "mock-fast",
            "starcoder2",
            "gemma2",
            "claude-2.1",
            "claude-instant-1.2",
        ] {
            assert!(!looks_like_vision_model(name), "{name} should not match");
        }
    }
}
