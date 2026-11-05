//! Provider connection configuration and the factory that turns a config into a live adapter.
//!
//! Non-secret configuration is persisted as JSON under the data root (moves to SQLite in
//! Phase 1 — the shape is already flat and versioned). API keys live only in the `SecretStore`
//! under `provider/<id>/api_key`; `ProviderConfig` only records whether one exists.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::capabilities::{looks_like_vision_model, ImageSupport};
use super::fake::FakeProvider;
use super::gemini::{self, GeminiConfig, GeminiProvider};
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
    /// Google Gemini via the Interactions API with a Google AI Studio API key.
    GoogleGemini,
    Simulated,
}

/// Static description of a provider kind. Adding a provider = one `KindMeta` constant, one
/// arm in `ProviderKind::meta`, and one arm in `ProviderStore::instantiate`.
struct KindMeta {
    label: &'static str,
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
    /// Whether the "request token usage" toggle applies (OpenAI-compatible only).
    usage_option: bool,
    user_creatable: bool,
    /// (label, base URL, suggested model)
    presets: &'static [(&'static str, &'static str, &'static str)],
}

const OPENAI_COMPATIBLE: KindMeta = KindMeta {
    label: "OpenAI-compatible",
    description: "Any server speaking the OpenAI Chat Completions protocol — local (Ollama, \
                  LM Studio, vLLM, llama.cpp) or hosted gateways.",
    default_base_url: None,
    base_url_hint: "OpenAI-compatible, ending in /v1",
    base_url_placeholder: "http://localhost:11434/v1",
    model_placeholder: "e.g. llama3.1, gpt-4o-mini",
    requires_api_key: false,
    api_key_hint: "Only needed for hosted gateways or servers started with an API key.",
    usage_option: true,
    user_creatable: true,
    presets: &[
        ("Ollama", "http://localhost:11434/v1", "llama3.1"),
        ("LM Studio", "http://localhost:1234/v1", ""),
        ("vLLM / llama.cpp", "http://localhost:8000/v1", ""),
        ("OpenRouter", "https://openrouter.ai/api/v1", ""),
    ],
};

const GOOGLE_GEMINI: KindMeta = KindMeta {
    label: "Google Gemini",
    description: "Gemini models through Google's Gemini API (Interactions API), using an API key \
                  from Google AI Studio.",
    default_base_url: Some(gemini::DEFAULT_BASE_URL),
    base_url_hint: "Gemini API root, ending in /v1",
    base_url_placeholder: gemini::DEFAULT_BASE_URL,
    model_placeholder: "e.g. gemini-flash-latest, gemini-pro-latest",
    requires_api_key: true,
    api_key_hint: "Create a key in Google AI Studio (aistudio.google.com → Get API key).",
    usage_option: false,
    user_creatable: true,
    presets: &[],
};

const SIMULATED: KindMeta = KindMeta {
    label: "Simulated",
    description: "Built-in offline provider for trying the app.",
    default_base_url: None,
    base_url_hint: "",
    base_url_placeholder: "",
    model_placeholder: "",
    requires_api_key: false,
    api_key_hint: "",
    usage_option: false,
    user_creatable: false,
    presets: &[],
};

impl ProviderKind {
    pub const ALL: [ProviderKind; 3] = [
        ProviderKind::OpenAiCompatible,
        ProviderKind::GoogleGemini,
        ProviderKind::Simulated,
    ];

    fn meta(self) -> &'static KindMeta {
        match self {
            ProviderKind::OpenAiCompatible => &OPENAI_COMPATIBLE,
            ProviderKind::GoogleGemini => &GOOGLE_GEMINI,
            ProviderKind::Simulated => &SIMULATED,
        }
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
            description: m.description.into(),
            default_base_url: m.default_base_url.map(Into::into),
            base_url_hint: m.base_url_hint.into(),
            base_url_placeholder: m.base_url_placeholder.into(),
            model_placeholder: m.model_placeholder.into(),
            requires_api_key: m.requires_api_key,
            api_key_hint: m.api_key_hint.into(),
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
    pub description: String,
    /// Prefilled (and used when the field is left empty); `None` = the user must enter one.
    pub default_base_url: Option<String>,
    pub base_url_hint: String,
    pub base_url_placeholder: String,
    pub model_placeholder: String,
    pub requires_api_key: bool,
    /// Help text under the API key field; empty = none.
    pub api_key_hint: String,
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
    }
}

/// Whether to resolve credentials and the TLS-configured client. Inspection (capabilities)
/// needs neither, which keeps keychain reads and client construction off that path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Connect,
    Inspect,
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
        let cache = existing.filter(|p| p.kind == input.kind);

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
        self.instantiate(&self.get(id)?, Purpose::Connect)
    }

    /// The single per-kind dispatch point. `Inspect` skips the keychain read and TLS client
    /// construction: capability lookups run on every model change and never hit the network.
    fn instantiate(
        &self,
        config: &ProviderConfig,
        purpose: Purpose,
    ) -> Result<Arc<dyn ModelProvider>> {
        Ok(match config.kind {
            ProviderKind::Simulated => Arc::new(FakeProvider::default()),
            ProviderKind::OpenAiCompatible => {
                let mut c = OpenAiCompatConfig::new(config.id.clone(), config.base_url.clone());
                c.include_usage = config.include_usage;
                c.context_window = config.context_window;
                let (key, http) = self.connection(config, purpose)?;
                c.api_key = key;
                Arc::new(OpenAiCompatProvider::new(c, http))
            }
            ProviderKind::GoogleGemini => {
                let mut c = GeminiConfig::new(config.id.clone(), config.base_url.clone());
                let (key, http) = self.connection(config, purpose)?;
                c.api_key = key;
                Arc::new(GeminiProvider::new(c, http))
            }
        })
    }

    /// API key and HTTP client for a connection (`Inspect`: neither is resolved).
    fn connection(
        &self,
        config: &ProviderConfig,
        purpose: Purpose,
    ) -> Result<(Option<String>, reqwest::Client)> {
        if purpose == Purpose::Inspect {
            return Ok((None, self.http.clone()));
        }
        let key = if config.has_api_key {
            self.secrets.get(&api_key_ref(&config.id)?)?
        } else {
            None
        };
        let http = match config.tls_mode {
            TlsMode::System => self.http.clone(),
            TlsMode::SkipVerification => crate::http::client_insecure()?,
            TlsMode::CustomCa => crate::http::client_with_extra_ca(
                config.tls_ca_pem.as_deref().ok_or_else(|| {
                    Error::InvalidInput(
                        "TLS mode requires a certificate — paste the server's PEM certificate"
                            .into(),
                    )
                })?,
            )?,
        };
        Ok((key, http))
    }

    /// Effective capabilities for a provider+model pair: adapter-level flags plus model-level
    /// resolution. Context window: per-model override → provider default (a deliberate user
    /// cap) → discovered → adapter-declared. Images: explicit tri-state, else server report
    /// (discovered or cached) or the name heuristic.
    pub fn capabilities_for(&self, provider_id: &str, model: &str) -> Result<Capabilities> {
        let config = self.get(provider_id)?;
        let mut caps = self.instantiate(&config, Purpose::Inspect)?.capabilities();
        let discovered = config.discovered.get(model);
        caps.context_window = config
            .model_context_windows
            .get(model)
            .copied()
            .or(config.context_window)
            .or(discovered.and_then(|d| d.context_window))
            .or(caps.context_window);
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
        let discovery = self
            .instantiate(&config, Purpose::Connect)?
            .discover()
            .await?;
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
