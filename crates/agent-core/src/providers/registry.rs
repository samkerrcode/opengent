//! Provider connection configuration and the factory that turns a config into a live adapter.
//!
//! Non-secret configuration is persisted as JSON under the data root (moves to SQLite in
//! Phase 1 — the shape is already flat and versioned). API keys live only in the `SecretStore`
//! under `provider/<id>/api_key`; `ProviderConfig` only records whether one exists.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::anthropic::{self, AnthropicConfig, AnthropicProvider};
use super::capabilities::{looks_like_vision_model, ImageSupport};
use super::fake::FakeProvider;
use super::gemini::{self, GeminiConfig, GeminiProvider};
use super::openai::{self, OpenAiConfig, OpenAiProvider};
use super::openai_compat::{normalize_base_url, OpenAiCompatConfig, OpenAiCompatProvider};
use super::{Capabilities, DiscoveredModel, ModelProvider};
use crate::config::DataRoot;
use crate::secrets::{SecretRef, SecretStore};
use crate::{Error, Result};

/// Built-in, always-present provider that needs no network or account.
pub const SIMULATED_PROVIDER_ID: &str = "simulated";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ProviderKind {
    /// Any server speaking the OpenAI Chat Completions protocol (Ollama, LM Studio, vLLM, gateways).
    OpenAiCompatible,
    /// OpenAI via the Responses API with an OpenAI API key.
    OpenAi,
    /// Google Gemini via the Interactions API with a Google AI Studio API key.
    GoogleGemini,
    /// Anthropic Claude via the Messages API with an Anthropic API key.
    Anthropic,
    Simulated,
}

/// Adapter-level flags shared by the native HTTP adapters.
const NATIVE_CAPS: Capabilities = Capabilities {
    streaming: true,
    tools: true,
    parallel_tool_calls: true,
    images: false,
    context_window: None,
    max_output_tokens: None,
};

/// Static description of a provider kind. Adding a provider = one `KindMeta` constant, one
/// arm in `ProviderKind::meta`, and one arm in `ProviderStore::instantiate`.
struct KindMeta {
    /// Adapter-level capabilities (model-level ones are resolved in `capabilities_for`).
    /// Static so capability lookups never construct an adapter.
    capabilities: Capabilities,
    label: &'static str,
    /// One-line summary for the provider picker tile; empty = none.
    tagline: &'static str,
    description: &'static str,
    /// Used when the user leaves the base URL empty; `None` = the user must supply one.
    default_base_url: Option<&'static str>,
    base_url_hint: &'static str,
    base_url_placeholder: &'static str,
    /// Example model names for the default-model field.
    model_placeholder: &'static str,
    requires_api_key: bool,
    /// Help text under the API key field (where to get one); empty = none.
    api_key_hint: &'static str,
    /// Where to create an API key (opened in the browser); empty = none.
    api_key_url: &'static str,
    /// Whether the "request token usage" toggle applies (OpenAI-compatible only).
    usage_option: bool,
    user_creatable: bool,
    /// (label, base URL, suggested model)
    presets: &'static [(&'static str, &'static str, &'static str)],
}

const OPENAI_COMPATIBLE: KindMeta = KindMeta {
    capabilities: NATIVE_CAPS,
    label: "OpenAI-compatible",
    tagline: "Local servers and gateways",
    description: "Any server speaking the OpenAI Chat Completions protocol — local (Ollama, \
                  LM Studio, vLLM, llama.cpp) or hosted gateways.",
    default_base_url: None,
    base_url_hint: "OpenAI-compatible, ending in /v1",
    base_url_placeholder: "http://localhost:11434/v1",
    model_placeholder: "e.g. llama3.1, gpt-4o-mini",
    requires_api_key: false,
    api_key_hint: "Only needed for hosted gateways or servers started with an API key.",
    api_key_url: "",
    usage_option: true,
    user_creatable: true,
    presets: &[
        ("Ollama", "http://localhost:11434/v1", "llama3.1"),
        ("LM Studio", "http://localhost:1234/v1", ""),
        ("vLLM / llama.cpp", "http://localhost:8000/v1", ""),
        ("OpenRouter", "https://openrouter.ai/api/v1", ""),
    ],
};

const OPENAI: KindMeta = KindMeta {
    capabilities: NATIVE_CAPS,
    label: "OpenAI",
    tagline: "GPT and o-series models",
    description: "OpenAI models (GPT, o-series) through the Responses API, using an API key from \
                  the OpenAI Platform.",
    default_base_url: Some(openai::DEFAULT_BASE_URL),
    base_url_hint: "OpenAI API root, ending in /v1",
    base_url_placeholder: openai::DEFAULT_BASE_URL,
    model_placeholder: "e.g. gpt-5.2, gpt-5-mini, gpt-4.1",
    requires_api_key: true,
    api_key_hint: "Create a key at platform.openai.com → API keys.",
    api_key_url: "https://platform.openai.com/api-keys",
    usage_option: false,
    user_creatable: true,
    presets: &[],
};

const GOOGLE_GEMINI: KindMeta = KindMeta {
    capabilities: NATIVE_CAPS,
    label: "Google Gemini",
    tagline: "Gemini models via Google AI Studio",
    description: "Gemini models through Google's Gemini API (Interactions API), using an API key \
                  from Google AI Studio.",
    default_base_url: Some(gemini::DEFAULT_BASE_URL),
    base_url_hint: "Gemini API root, ending in /v1",
    base_url_placeholder: gemini::DEFAULT_BASE_URL,
    model_placeholder: "e.g. gemini-flash-latest, gemini-pro-latest",
    requires_api_key: true,
    api_key_hint: "Create a key in Google AI Studio (aistudio.google.com → Get API key).",
    api_key_url: "https://aistudio.google.com/apikey",
    usage_option: false,
    user_creatable: true,
    presets: &[],
};

const ANTHROPIC: KindMeta = KindMeta {
    capabilities: NATIVE_CAPS,
    label: "Anthropic",
    tagline: "Claude models",
    description: "Claude models through Anthropic's Messages API, using an API key from the \
                  Anthropic Console.",
    default_base_url: Some(anthropic::DEFAULT_BASE_URL),
    base_url_hint: "Anthropic API root, ending in /v1",
    base_url_placeholder: anthropic::DEFAULT_BASE_URL,
    model_placeholder: "e.g. claude-opus-5-5, claude-sonnet-5-5, claude-haiku-5-5",
    requires_api_key: true,
    api_key_hint: "Create a key in the Anthropic Console (console.anthropic.com → API keys).",
    api_key_url: "https://console.anthropic.com/settings/keys",
    usage_option: false,
    user_creatable: true,
    presets: &[],
};

const SIMULATED: KindMeta = KindMeta {
    capabilities: Capabilities {
        streaming: true,
        tools: false,
        parallel_tool_calls: false,
        images: true,
        context_window: None,
        max_output_tokens: None,
    },
    label: "Simulated",
    tagline: "",
    description: "Built-in offline provider for trying the app.",
    default_base_url: None,
    base_url_hint: "",
    base_url_placeholder: "",
    model_placeholder: "",
    requires_api_key: false,
    api_key_hint: "",
    api_key_url: "",
    usage_option: false,
    user_creatable: false,
    presets: &[],
};

impl ProviderKind {
    pub const ALL: [ProviderKind; 5] = [
        ProviderKind::OpenAiCompatible,
        ProviderKind::OpenAi,
        ProviderKind::GoogleGemini,
        ProviderKind::Anthropic,
        ProviderKind::Simulated,
    ];

    fn meta(self) -> &'static KindMeta {
        match self {
            ProviderKind::OpenAiCompatible => &OPENAI_COMPATIBLE,
            ProviderKind::OpenAi => &OPENAI,
            ProviderKind::GoogleGemini => &GOOGLE_GEMINI,
            ProviderKind::Anthropic => &ANTHROPIC,
            ProviderKind::Simulated => &SIMULATED,
        }
    }

    /// Adapter-level capabilities of this kind; adapters report the same value.
    pub fn capabilities(self) -> Capabilities {
        self.meta().capabilities.clone()
    }

    pub fn label(self) -> &'static str {
        self.meta().label
    }

    pub fn requires_api_key(self) -> bool {
        self.meta().requires_api_key
    }

    /// UI description of this kind (rendered by the provider settings form).
    pub fn spec(self) -> KindSpec {
        let m = self.meta();
        KindSpec {
            kind: self,
            label: m.label.into(),
            tagline: m.tagline.into(),
            description: m.description.into(),
            default_base_url: m.default_base_url.map(Into::into),
            base_url_hint: m.base_url_hint.into(),
            base_url_placeholder: m.base_url_placeholder.into(),
            model_placeholder: m.model_placeholder.into(),
            requires_api_key: m.requires_api_key,
            api_key_hint: m.api_key_hint.into(),
            api_key_url: m.api_key_url.into(),
            usage_option: m.usage_option,
            presets: m
                .presets
                .iter()
                .map(|(label, base_url, model)| ProviderPreset {
                    label: (*label).into(),
                    base_url: (*base_url).into(),
                    model: (*model).into(),
                })
                .collect(),
        }
    }
}

/// Kinds a user can create, in display order.
pub fn creatable_kinds() -> Vec<KindSpec> {
    ProviderKind::ALL
        .into_iter()
        .filter(|k| k.meta().user_creatable)
        .map(ProviderKind::spec)
        .collect()
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct KindSpec {
    pub kind: ProviderKind,
    pub label: String,
    /// One-line summary for the provider picker tile; empty = none.
    pub tagline: String,
    pub description: String,
    /// Prefilled (and used when the field is left empty); `None` = the user must enter one.
    pub default_base_url: Option<String>,
    pub base_url_hint: String,
    pub base_url_placeholder: String,
    pub model_placeholder: String,
    pub requires_api_key: bool,
    /// Help text under the API key field; empty = none.
    pub api_key_hint: String,
    /// Where to create an API key (opened in the browser); empty = none.
    pub api_key_url: String,
    pub usage_option: bool,
    pub presets: Vec<ProviderPreset>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ProviderPreset {
    pub label: String,
    pub base_url: String,
    pub model: String,
}

/// How strictly to verify the provider's TLS certificate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TlsMode {
    /// OS verifier + trust store (macOS Keychain / Windows store), the safe default.
    #[default]
    System,
    /// Trust a specific PEM-encoded server certificate or private CA in `tls_ca_pem`.
    /// This is the recommended option for self-signed LAN servers (pins to that cert).
    CustomCa,
    /// Skip verification entirely (`curl -k`). Encrypted but unauthenticated —
    /// only acceptable on a trusted network.
    SkipVerification,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub default_model: Option<String>,
    /// Models discovered via the provider's listing endpoint (cache; may be empty).
    #[serde(default)]
    pub models: Vec<String>,
    pub has_api_key: bool,
    #[serde(default = "default_true")]
    pub include_usage: bool,
    pub context_window: Option<u32>,
    /// Per-model context window overrides (tokens); wins over `context_window` when the
    /// named model is selected. Empty = every model uses the provider default.
    #[serde(default)]
    pub model_context_windows: HashMap<String, u32>,
    #[serde(default)]
    pub tls_mode: TlsMode,
    /// PEM certificate to trust when `tls_mode` is `custom_ca`. Not a secret.
    #[serde(default)]
    pub tls_ca_pem: Option<String>,
    /// Whether models on this provider accept image input. `auto` combines server-reported
    /// `vision_models` with a name heuristic; `always`/`never` are user overrides.
    #[serde(default)]
    pub image_support: ImageSupport,
    /// Models the server reported as vision-capable during the last discovery (cache).
    #[serde(default)]
    pub vision_models: Vec<String>,
    /// Per-model metadata from the last discovery (context window, vision, reasoning).
    #[serde(default)]
    pub discovered: HashMap<String, DiscoveredModel>,
    /// Models the user hid from pickers. A listing preference only: runs accept any model
    /// name, and a hidden entry is kept (inert) while the model isn't listed so re-discovery
    /// doesn't resurrect it.
    #[serde(default)]
    pub hidden_models: BTreeSet<String>,
}

fn default_true() -> bool {
    true
}

/// Input from the UI when creating or editing a provider. `api_key: Some("")` clears the key;
/// `None` leaves it unchanged.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct ProviderInput {
    pub id: Option<String>,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub default_model: Option<String>,
    pub api_key: Option<String>,
    #[serde(default = "default_true")]
    pub include_usage: bool,
    pub context_window: Option<u32>,
    #[serde(default)]
    pub model_context_windows: HashMap<String, u32>,
    #[serde(default)]
    pub tls_mode: TlsMode,
    /// `Some("")` clears a stored PEM; `None` leaves it unchanged.
    pub tls_ca_pem: Option<String>,
    #[serde(default)]
    pub image_support: ImageSupport,
    /// Replaces the hidden-model set on save (not additive); ignored when the kind changes.
    #[serde(default)]
    pub hidden_models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActiveSelection {
    pub provider_id: String,
    pub model: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    version: u16,
    #[serde(default)]
    providers: Vec<ProviderConfig>,
    #[serde(default)]
    active: Option<ActiveSelection>,
}

pub struct ProviderStore {
    path: PathBuf,
    state: Mutex<StoreFile>,
    secrets: Arc<dyn SecretStore>,
    http: reqwest::Client,
    /// Per-provider clients for custom/skip TLS modes (see `connection`).
    tls_clients: Mutex<HashMap<String, TlsClient>>,
}

fn api_key_ref(id: &str) -> Result<SecretRef> {
    SecretRef::new(format!("provider/{id}/api_key"))
}

fn simulated_config() -> ProviderConfig {
    ProviderConfig {
        id: SIMULATED_PROVIDER_ID.into(),
        name: "Simulated (no account needed)".into(),
        kind: ProviderKind::Simulated,
        base_url: String::new(),
        default_model: Some("simulated-1".into()),
        models: vec!["simulated-1".into()],
        has_api_key: false,
        include_usage: true,
        context_window: Some(8192),
        model_context_windows: HashMap::new(),
        tls_mode: TlsMode::System,
        tls_ca_pem: None,
        image_support: ImageSupport::Always,
        vision_models: Vec::new(),
        discovered: HashMap::new(),
        hidden_models: BTreeSet::new(),
    }
}

/// A client built for a provider's non-default TLS settings, reused while they are unchanged.
struct TlsClient {
    mode: TlsMode,
    ca_pem: Option<String>,
    client: reqwest::Client,
}

impl ProviderStore {
    pub fn load(
        data_root: &DataRoot,
        secrets: Arc<dyn SecretStore>,
        http: reqwest::Client,
    ) -> Result<Self> {
        let path = data_root.path().join("providers.json");
        let mut state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<StoreFile>(&bytes)
                .map_err(|e| Error::Io(format!("providers.json is corrupt: {e}")))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreFile {
                version: 1,
                ..Default::default()
            },
            Err(e) => return Err(e.into()),
        };
        state.providers.retain(|p| p.id != SIMULATED_PROVIDER_ID);
        state.providers.insert(0, simulated_config());
        Ok(Self {
            path,
            state: Mutex::new(state),
            secrets,
            http,
            tls_clients: Mutex::new(HashMap::new()),
        })
    }

    fn persist(&self, state: &StoreFile) -> Result<()> {
        let persisted = StoreFile {
            version: 1,
            providers: state
                .providers
                .iter()
                .filter(|p| p.id != SIMULATED_PROVIDER_ID)
                .cloned()
                .collect(),
            active: state.active.clone(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(&persisted).map_err(|e| Error::Internal(e.to_string()))?,
        )?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<ProviderConfig> {
        self.state.lock().unwrap().providers.clone()
    }

    pub fn get(&self, id: &str) -> Result<ProviderConfig> {
        self.state
            .lock()
            .unwrap()
            .providers
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("provider `{id}`")))
    }

    pub fn active(&self) -> Option<ActiveSelection> {
        self.state.lock().unwrap().active.clone()
    }

    pub fn set_active(&self, selection: ActiveSelection) -> Result<()> {
        self.get(&selection.provider_id)?;
        if selection.model.trim().is_empty() {
            return Err(Error::InvalidInput("model is required".into()));
        }
        let mut state = self.state.lock().unwrap();
        state.active = Some(selection);
        self.persist(&state)
    }

    pub fn save(&self, input: ProviderInput) -> Result<ProviderConfig> {
        let meta = input.kind.meta();
        if !meta.user_creatable || input.id.as_deref() == Some(SIMULATED_PROVIDER_ID) {
            return Err(Error::InvalidInput(
                "the simulated provider cannot be edited".into(),
            ));
        }
        let name = input.name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(Error::InvalidInput("name must be 1–64 characters".into()));
        }
        let base_url = match (input.base_url.trim(), meta.default_base_url) {
            ("", Some(default)) => default.to_owned(),
            (raw, _) => normalize_base_url(raw)?,
        };
        let id = input.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        // A whitespace-only key is a clear, not an empty secret.
        let api_key = input.api_key.as_deref().map(str::trim);

        if input.tls_mode == TlsMode::CustomCa
            && input
                .tls_ca_pem
                .as_deref()
                .is_some_and(|p| !p.trim().is_empty())
        {
            // Validate eagerly so a bad PEM fails at save time, not mid-run.
            reqwest::tls::Certificate::from_pem(input.tls_ca_pem.as_deref().unwrap().as_bytes())
                .map_err(|e| Error::InvalidInput(format!("certificate is not valid PEM: {e}")))?;
        }
        let key_ref = api_key_ref(&id)?;

        let mut state = self.state.lock().unwrap();
        let existing = state.providers.iter().find(|p| p.id == id).cloned();
        if existing
            .as_ref()
            .is_some_and(|p| !p.kind.meta().user_creatable)
        {
            return Err(Error::InvalidInput("this provider cannot be edited".into()));
        }
        // Validate before touching the keychain so a rejected save has no side effects.
        let will_have_key = match api_key {
            Some(key) => !key.is_empty(),
            None => existing.as_ref().is_some_and(|p| p.has_api_key),
        };
        if meta.requires_api_key && !will_have_key {
            return Err(Error::InvalidInput(format!(
                "an API key is required for {}",
                meta.label
            )));
        }
        match api_key {
            Some("") => self.secrets.delete(&key_ref)?,
            Some(key) => self.secrets.set(&key_ref, key)?,
            None => {}
        }
        let has_api_key = will_have_key;
        let existing_pem = existing.as_ref().and_then(|p| p.tls_ca_pem.clone());
        // Cached discovery belongs to the previous protocol when the kind changes.
        let is_new = existing.is_none();
        let cache = existing.filter(|p| p.kind == input.kind);
        // Hidden names belong to the previous protocol's listing when the kind changes.
        let hidden_models: BTreeSet<String> = if is_new || cache.is_some() {
            input
                .hidden_models
                .into_iter()
                .map(|m| m.trim().to_owned())
                .filter(|m| !m.is_empty())
                .collect()
        } else {
            BTreeSet::new()
        };

        let config = ProviderConfig {
            id: id.clone(),
            name: name.to_owned(),
            kind: input.kind,
            base_url,
            default_model: input
                .default_model
                .map(|m| m.trim().to_owned())
                .filter(|m| !m.is_empty()),
            models: cache.as_ref().map(|p| p.models.clone()).unwrap_or_default(),
            has_api_key,
            include_usage: input.include_usage,
            context_window: input.context_window,
            model_context_windows: input
                .model_context_windows
                .into_iter()
                .map(|(m, w)| (m.trim().to_owned(), w))
                .filter(|(m, w)| !m.is_empty() && *w > 0)
                .collect(),
            tls_mode: input.tls_mode,
            tls_ca_pem: match input.tls_ca_pem.as_deref() {
                Some("") => None,
                Some(pem) => Some(pem.trim().to_owned()),
                None => existing_pem,
            },
            image_support: input.image_support,
            vision_models: cache
                .as_ref()
                .map(|p| p.vision_models.clone())
                .unwrap_or_default(),
            discovered: cache.map(|p| p.discovered).unwrap_or_default(),
            hidden_models,
        };
        match state.providers.iter_mut().find(|p| p.id == id) {
            Some(slot) => *slot = config.clone(),
            None => state.providers.push(config.clone()),
        }
        self.persist(&state)?;
        Ok(config)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        if id == SIMULATED_PROVIDER_ID {
            return Err(Error::InvalidInput(
                "the simulated provider cannot be removed".into(),
            ));
        }
        let mut state = self.state.lock().unwrap();
        let before = state.providers.len();
        state.providers.retain(|p| p.id != id);
        if state.providers.len() == before {
            return Err(Error::NotFound(format!("provider `{id}`")));
        }
        if state.active.as_ref().is_some_and(|a| a.provider_id == id) {
            state.active = None;
        }
        self.tls_clients.lock().unwrap().remove(id);
        self.secrets.delete(&api_key_ref(id)?)?;
        self.persist(&state)
    }

    /// Overwrites the cached vision flags (tests and manual overrides).
    pub fn update_vision_models(&self, id: &str, vision_models: Vec<String>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let p = state
            .providers
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::NotFound(format!("provider `{id}`")))?;
        p.vision_models = vision_models;
        self.persist(&state)
    }

    /// Instantiates a live adapter, resolving the API key from the secret store.
    pub fn build(&self, id: &str) -> Result<Arc<dyn ModelProvider>> {
        self.instantiate(&self.get(id)?)
    }

    /// The single per-kind dispatch point.
    fn instantiate(&self, config: &ProviderConfig) -> Result<Arc<dyn ModelProvider>> {
        if config.kind == ProviderKind::Simulated {
            return Ok(Arc::new(FakeProvider::default()));
        }
        let (key, http) = self.connection(config)?;
        let (id, base_url) = (config.id.clone(), config.base_url.clone());
        Ok(match config.kind {
            ProviderKind::Simulated => unreachable!("handled above"),
            ProviderKind::OpenAiCompatible => {
                let mut c = OpenAiCompatConfig::new(id, base_url);
                c.include_usage = config.include_usage;
                c.context_window = config.context_window;
                c.api_key = key;
                Arc::new(OpenAiCompatProvider::new(c, http))
            }
            ProviderKind::OpenAi => {
                let mut c = OpenAiConfig::new(id, base_url);
                c.api_key = key;
                Arc::new(OpenAiProvider::new(c, http))
            }
            ProviderKind::GoogleGemini => {
                let mut c = GeminiConfig::new(id, base_url);
                c.api_key = key;
                Arc::new(GeminiProvider::new(c, http))
            }
            ProviderKind::Anthropic => {
                let mut c = AnthropicConfig::new(id, base_url);
                c.api_key = key;
                Arc::new(AnthropicProvider::new(c, http))
            }
        })
    }

    /// API key and HTTP client for a connection. Clients for non-default TLS modes are built
    /// once per provider and reused until its TLS settings change: building one loads a
    /// trust store, which is too slow to repeat for every run.
    fn connection(&self, config: &ProviderConfig) -> Result<(Option<String>, reqwest::Client)> {
        let key = if config.has_api_key {
            self.secrets.get(&api_key_ref(&config.id)?)?
        } else {
            None
        };
        if config.tls_mode == TlsMode::System {
            return Ok((key, self.http.clone()));
        }
        let mut cache = self.tls_clients.lock().unwrap();
        if let Some(c) = cache.get(&config.id).filter(|c| {
            c.mode == config.tls_mode && c.ca_pem.as_deref() == config.tls_ca_pem.as_deref()
        }) {
            return Ok((key, c.client.clone()));
        }
        let client = match config.tls_mode {
            TlsMode::CustomCa => crate::http::client_with_extra_ca(
                config.tls_ca_pem.as_deref().ok_or_else(|| {
                    Error::InvalidInput(
                        "TLS mode requires a certificate — paste the server's PEM certificate"
                            .into(),
                    )
                })?,
            )?,
            _ => crate::http::client_insecure()?,
        };
        cache.insert(
            config.id.clone(),
            TlsClient {
                mode: config.tls_mode,
                ca_pem: config.tls_ca_pem.clone(),
                client: client.clone(),
            },
        );
        Ok((key, client))
    }

    /// Effective capabilities for a provider+model pair: adapter-level flags plus model-level
    /// resolution. Context window: per-model override → provider default (a deliberate user
    /// cap) → discovered → adapter-declared. Images: explicit tri-state, else server report
    /// (discovered or cached) or the name heuristic. Runs on every model change and run
    /// start, so it reads the config in place: no clone, keychain read or adapter.
    pub fn capabilities_for(&self, provider_id: &str, model: &str) -> Result<Capabilities> {
        let state = self.state.lock().unwrap();
        let config = state
            .providers
            .iter()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| Error::NotFound(format!("provider `{provider_id}`")))?;
        let mut caps = config.kind.capabilities();
        let discovered = config.discovered.get(model);
        caps.context_window = config
            .model_context_windows
            .get(model)
            .copied()
            .or(config.context_window)
            .or(discovered.and_then(|d| d.context_window))
            .or(caps.context_window);
        caps.max_output_tokens = discovered
            .and_then(|d| d.max_output_tokens)
            .or(caps.max_output_tokens);
        caps.images = match config.image_support {
            ImageSupport::Always => true,
            ImageSupport::Never => false,
            ImageSupport::Auto => {
                discovered.and_then(|d| d.vision) == Some(true)
                    || config.vision_models.iter().any(|m| m == model)
                    || looks_like_vision_model(model)
            }
        };
        Ok(caps)
    }

    /// Discovers models where the protocol supports it and caches them (with any reported
    /// metadata) on the config in one write. `vision_models` is refreshed only when the
    /// provider reported vision flags, so a failed probe keeps the previous cache.
    pub async fn discover_models(&self, id: &str) -> Result<Vec<String>> {
        let config = self.get(id)?;
        let discovery = self.instantiate(&config)?.discover().await?;
        if config.kind == ProviderKind::Simulated {
            return Ok(config.models);
        }
        let mut state = self.state.lock().unwrap();
        let p = state
            .providers
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::NotFound(format!("provider `{id}`")))?;
        // The config may have been edited while the request was in flight; results for a
        // different endpoint or protocol must not be cached against it.
        if p.kind != config.kind || p.base_url != config.base_url {
            return Ok(discovery.models);
        }
        if discovery.details.values().any(|d| d.vision.is_some()) {
            let mut vision: Vec<String> = discovery
                .details
                .iter()
                .filter(|(_, d)| d.vision == Some(true))
                .map(|(m, _)| m.clone())
                .collect();
            vision.sort();
            p.vision_models = vision;
        }
        p.models = discovery.models.clone();
        p.discovered = discovery.details;
        self.persist(&state)?;
        Ok(discovery.models)
    }

    pub fn secret_refs(&self) -> HashMap<String, SecretRef> {
        self.list()
            .into_iter()
            .filter(|p| p.has_api_key)
            .filter_map(|p| api_key_ref(&p.id).ok().map(|r| (p.id, r)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolve_data_root;
    use crate::secrets::MemorySecretStore;

    fn store() -> (ProviderStore, PathBuf) {
        let tmp = std::env::temp_dir().join(format!("opengent-prov-{}", uuid::Uuid::new_v4()));
        let root = resolve_data_root(Some(&tmp)).unwrap();
        let s = ProviderStore::load(
            &root,
            Arc::new(MemorySecretStore::default()),
            crate::http::client().unwrap(),
        )
        .unwrap();
        (s, tmp)
    }

    fn input(name: &str) -> ProviderInput {
        ProviderInput {
            id: None,
            name: name.into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "http://localhost:11434/v1/".into(),
            default_model: Some("llama3".into()),
            api_key: Some("sk-test".into()),
            include_usage: true,
            context_window: None,
            model_context_windows: HashMap::new(),
            tls_mode: TlsMode::System,
            tls_ca_pem: None,
            image_support: ImageSupport::Auto,
            hidden_models: Vec::new(),
        }
    }

    #[test]
    fn save_persists_without_secret_and_reloads() {
        let (s, tmp) = store();
        let saved = s.save(input("Ollama")).unwrap();
        assert!(saved.has_api_key);
        assert_eq!(saved.base_url, "http://localhost:11434/v1");
        let raw = std::fs::read_to_string(tmp.join("providers.json")).unwrap();
        assert!(
            !raw.contains("sk-test"),
            "secret leaked into providers.json"
        );
        assert!(!raw.contains(SIMULATED_PROVIDER_ID));

        let root = resolve_data_root(Some(&tmp)).unwrap();
        let s2 = ProviderStore::load(
            &root,
            Arc::new(MemorySecretStore::default()),
            crate::http::client().unwrap(),
        )
        .unwrap();
        let list = s2.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, SIMULATED_PROVIDER_ID);
        assert_eq!(list[1].name, "Ollama");
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn edit_keeps_key_when_none_and_clears_on_empty() {
        let (s, tmp) = store();
        let p = s.save(input("X")).unwrap();
        let mut edit = input("X2");
        edit.id = Some(p.id.clone());
        edit.api_key = None;
        assert!(s.save(edit.clone()).unwrap().has_api_key);
        edit.api_key = Some(String::new());
        assert!(!s.save(edit).unwrap().has_api_key);
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn capabilities_for_respects_tri_state_detection_and_heuristic() {
        let (s, tmp) = store();
        assert!(
            s.capabilities_for(SIMULATED_PROVIDER_ID, "simulated-1")
                .unwrap()
                .images
        );
        let p = s.save(input("X")).unwrap();
        let images = |model: &str| s.capabilities_for(&p.id, model).unwrap().images;
        assert!(images("llava:7b"), "heuristic should match");
        assert!(!images("llama3.1"));
        s.update_vision_models(&p.id, vec!["custom-model".into()])
            .unwrap();
        assert!(images("custom-model"), "server-reported should match");

        let mut e = input("X");
        e.id = Some(p.id.clone());
        e.image_support = ImageSupport::Always;
        s.save(e).unwrap();
        assert!(images("llama3.1"));
        let mut e = input("X");
        e.id = Some(p.id.clone());
        e.image_support = ImageSupport::Never;
        s.save(e).unwrap();
        assert!(!images("llava:7b"));
        assert!(!images("custom-model"));
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn hidden_models_round_trip_and_clear_on_kind_change() {
        let (s, tmp) = store();
        let mut i = input("X");
        i.hidden_models = vec![" old-model ".into(), "old-model".into(), "".into()];
        let p = s.save(i).unwrap();
        assert_eq!(
            p.hidden_models.iter().collect::<Vec<_>>(),
            vec!["old-model"]
        );

        // Same-kind edit replaces the set wholesale.
        let mut e = input("X");
        e.id = Some(p.id.clone());
        e.hidden_models = vec!["other-model".into()];
        assert_eq!(
            s.save(e).unwrap().hidden_models.iter().collect::<Vec<_>>(),
            vec!["other-model"]
        );

        // A kind change drops hidden names from the previous protocol's listing.
        let mut e = input("X");
        e.id = Some(p.id.clone());
        e.kind = ProviderKind::OpenAi;
        e.base_url = String::new();
        e.hidden_models = vec!["gpt-4o".into()];
        let saved = s.save(e).unwrap();
        assert!(saved.hidden_models.is_empty());
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn old_providers_json_loads_with_defaults() {
        let tmp = std::env::temp_dir().join(format!("opengent-prov-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("providers.json"),
            r#"{"version":1,"providers":[{"id":"p1","name":"P","kind":"open_ai_compatible","base_url":"http://localhost:11434/v1","has_api_key":false}],"active":null}"#,
        )
        .unwrap();
        let root = resolve_data_root(Some(&tmp)).unwrap();
        let s = ProviderStore::load(
            &root,
            Arc::new(MemorySecretStore::default()),
            crate::http::client().unwrap(),
        )
        .unwrap();
        let p = s.get("p1").unwrap();
        assert_eq!(p.image_support, ImageSupport::Auto);
        assert!(p.vision_models.is_empty());
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn context_window_prefers_user_values_then_discovered() {
        let (s, tmp) = store();
        let p = s.save(input("X")).unwrap();
        let window = |model: &str| s.capabilities_for(&p.id, model).unwrap().context_window;
        assert_eq!(window("m"), None);
        s.state
            .lock()
            .unwrap()
            .providers
            .iter_mut()
            .find(|c| c.id == p.id)
            .unwrap()
            .discovered
            .insert(
                "m".into(),
                DiscoveredModel {
                    context_window: Some(1_000_000),
                    max_output_tokens: None,
                    vision: Some(true),
                    reasoning: None,
                },
            );
        assert_eq!(window("m"), Some(1_000_000), "discovered");
        assert!(s.capabilities_for(&p.id, "m").unwrap().images);
        let mut e = input("X");
        e.id = Some(p.id.clone());
        e.context_window = Some(32_768);
        s.save(e.clone()).unwrap();
        assert_eq!(window("m"), Some(32_768), "provider default caps");
        assert!(
            s.get(&p.id).unwrap().discovered.contains_key("m"),
            "edits keep the discovery cache"
        );
        e.model_context_windows.insert("m".into(), 8192);
        s.save(e.clone()).unwrap();
        assert_eq!(window("m"), Some(8192), "per-model override wins");
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn whitespace_key_clears_and_simulated_kind_is_rejected() {
        let (s, tmp) = store();
        let p = s.save(input("X")).unwrap();
        let mut e = input("X");
        e.id = Some(p.id.clone());
        e.api_key = Some("   ".into());
        assert!(!s.save(e).unwrap().has_api_key);
        let mut sim = input("S");
        sim.kind = ProviderKind::Simulated;
        assert!(s.save(sim).is_err());
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn kind_specs_list_only_creatable_kinds() {
        let kinds = creatable_kinds();
        assert!(kinds.iter().all(|k| k.kind != ProviderKind::Simulated));
        let oc = kinds
            .iter()
            .find(|k| k.kind == ProviderKind::OpenAiCompatible)
            .unwrap();
        assert!(oc.default_base_url.is_none() && !oc.requires_api_key && oc.usage_option);
        assert!(!oc.presets.is_empty());
        let g = kinds
            .iter()
            .find(|k| k.kind == ProviderKind::GoogleGemini)
            .unwrap();
        assert_eq!(
            g.default_base_url.as_deref(),
            Some(gemini::DEFAULT_BASE_URL)
        );
        assert!(g.requires_api_key && !g.usage_option);
        assert!(!g.model_placeholder.is_empty() && !g.api_key_hint.is_empty());
        assert!(kinds
            .iter()
            .all(|k| !k.tagline.is_empty() && !k.api_key_url.starts_with("http:")));
    }

    #[test]
    fn gemini_requires_key_defaults_url_and_reports_family() {
        let (s, tmp) = store();
        let mut g = input("Gemini");
        g.kind = ProviderKind::GoogleGemini;
        g.base_url = String::new();
        g.api_key = None;
        assert!(s.save(g.clone()).is_err(), "key required");
        g.api_key = Some("AIza-test".into());
        let saved = s.save(g).unwrap();
        assert_eq!(saved.base_url, gemini::DEFAULT_BASE_URL);
        assert_eq!(s.build(&saved.id).unwrap().family(), Some(gemini::FAMILY));
        let caps = s
            .capabilities_for(&saved.id, "gemini-flash-latest")
            .unwrap();
        assert!(caps.tools && caps.images, "name heuristic covers gemini");
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn openai_requires_key_defaults_url_and_reports_family() {
        let (s, tmp) = store();
        let mut o = input("OpenAI");
        o.kind = ProviderKind::OpenAi;
        o.base_url = String::new();
        o.api_key = None;
        assert!(s.save(o.clone()).is_err(), "key required");
        o.api_key = Some("sk-test".into());
        let saved = s.save(o).unwrap();
        assert_eq!(saved.base_url, openai::DEFAULT_BASE_URL);
        assert_eq!(s.build(&saved.id).unwrap().family(), Some(openai::FAMILY));
        let caps = s.capabilities_for(&saved.id, "gpt-5").unwrap();
        assert!(caps.tools && caps.images, "name heuristic covers gpt-5");
        let spec = ProviderKind::OpenAi.spec();
        assert!(spec.requires_api_key && !spec.usage_option);
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn anthropic_requires_key_defaults_url_and_reports_family() {
        let (s, tmp) = store();
        let mut a = input("Anthropic");
        a.kind = ProviderKind::Anthropic;
        a.base_url = String::new();
        a.api_key = None;
        assert!(s.save(a.clone()).is_err(), "key required");
        a.api_key = Some("sk-ant-test".into());
        let saved = s.save(a).unwrap();
        assert_eq!(saved.base_url, anthropic::DEFAULT_BASE_URL);
        assert_eq!(
            s.build(&saved.id).unwrap().family(),
            Some(anthropic::FAMILY)
        );
        let caps = s.capabilities_for(&saved.id, "claude-sonnet-5-5").unwrap();
        assert!(caps.tools && caps.images, "name heuristic covers claude 5");
        let spec = ProviderKind::Anthropic.spec();
        assert!(spec.requires_api_key && !spec.usage_option);
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn adapters_report_their_kind_capabilities() {
        let (s, tmp) = store();
        for kind in ProviderKind::ALL {
            let id = if kind == ProviderKind::Simulated {
                SIMULATED_PROVIDER_ID.to_owned()
            } else {
                let mut i = input("X");
                i.kind = kind;
                s.save(i).unwrap().id
            };
            let adapter = s.build(&id).unwrap().capabilities();
            let meta = kind.capabilities();
            assert_eq!(
                (
                    adapter.streaming,
                    adapter.tools,
                    adapter.parallel_tool_calls
                ),
                (meta.streaming, meta.tools, meta.parallel_tool_calls),
                "{kind:?}: capabilities_for must agree with the adapter"
            );
        }
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn custom_tls_clients_are_reused_until_settings_change() {
        let (s, tmp) = store();
        let mut i = input("LAN");
        i.base_url = "https://lan.example/v1".into();
        i.tls_mode = TlsMode::SkipVerification;
        let p = s.save(i.clone()).unwrap();
        s.build(&p.id).unwrap();
        s.build(&p.id).unwrap();
        assert_eq!(s.tls_clients.lock().unwrap().len(), 1);
        i.id = Some(p.id.clone());
        i.tls_mode = TlsMode::System;
        s.save(i).unwrap();
        s.build(&p.id).unwrap();
        s.delete(&p.id).unwrap();
        assert!(s.tls_clients.lock().unwrap().is_empty());
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[tokio::test]
    async fn simulated_discovery_returns_its_models() {
        let (s, tmp) = store();
        assert_eq!(
            s.discover_models(SIMULATED_PROVIDER_ID).await.unwrap(),
            vec!["simulated-1".to_string()]
        );
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn simulated_is_protected_and_active_validates() {
        let (s, tmp) = store();
        assert!(s.delete(SIMULATED_PROVIDER_ID).is_err());
        assert!(s
            .set_active(ActiveSelection {
                provider_id: "nope".into(),
                model: "m".into()
            })
            .is_err());
        s.set_active(ActiveSelection {
            provider_id: SIMULATED_PROVIDER_ID.into(),
            model: "simulated-1".into(),
        })
        .unwrap();
        assert_eq!(s.active().unwrap().model, "simulated-1");
        let p = s.save(input("Y")).unwrap();
        s.set_active(ActiveSelection {
            provider_id: p.id.clone(),
            model: "llama3".into(),
        })
        .unwrap();
        s.delete(&p.id).unwrap();
        assert!(
            s.active().is_none(),
            "deleting the active provider clears selection"
        );
        std::fs::remove_dir_all(tmp).unwrap();
    }
}
