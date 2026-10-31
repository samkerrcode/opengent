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
use super::openai_compat::{normalize_base_url, OpenAiCompatConfig, OpenAiCompatProvider};
use super::{Capabilities, ModelProvider};
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
    Simulated,
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
        tls_mode: TlsMode::System,
        tls_ca_pem: None,
        image_support: ImageSupport::Always,
        vision_models: Vec::new(),
    }
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
        if input.kind == ProviderKind::Simulated
            || input.id.as_deref() == Some(SIMULATED_PROVIDER_ID)
        {
            return Err(Error::InvalidInput(
                "the simulated provider cannot be edited".into(),
            ));
        }
        let name = input.name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(Error::InvalidInput("name must be 1–64 characters".into()));
        }
        let base_url = normalize_base_url(&input.base_url)?;
        let id = input.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

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

        let has_api_key = match input.api_key.as_deref() {
            Some("") => {
                self.secrets.delete(&key_ref)?;
                false
            }
            Some(key) => {
                self.secrets.set(&key_ref, key.trim())?;
                true
            }
            None => existing.as_ref().map(|p| p.has_api_key).unwrap_or(false),
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
            models: existing
                .as_ref()
                .map(|p| p.models.clone())
                .unwrap_or_default(),
            has_api_key,
            include_usage: input.include_usage,
            context_window: input.context_window,
            tls_mode: input.tls_mode,
            tls_ca_pem: match input.tls_ca_pem.as_deref() {
                Some("") => None,
                Some(pem) => Some(pem.trim().to_owned()),
                None => existing.as_ref().and_then(|p| p.tls_ca_pem.clone()),
            },
            image_support: input.image_support,
            vision_models: existing.map(|p| p.vision_models).unwrap_or_default(),
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

    pub fn update_models(&self, id: &str, models: Vec<String>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let p = state
            .providers
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::NotFound(format!("provider `{id}`")))?;
        p.models = models;
        self.persist(&state)
    }

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
        let config = self.get(id)?;
        Ok(match config.kind {
            ProviderKind::Simulated => Arc::new(FakeProvider::default()),
            ProviderKind::OpenAiCompatible => Arc::new(self.build_openai_compat(&config)?),
        })
    }

    pub fn build_openai_compat(&self, config: &ProviderConfig) -> Result<OpenAiCompatProvider> {
        let mut c = OpenAiCompatConfig::new(config.id.clone(), config.base_url.clone());
        c.include_usage = config.include_usage;
        c.context_window = config.context_window;
        if config.has_api_key {
            c.api_key = self.secrets.get(&api_key_ref(&config.id)?)?;
        }
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
        Ok(OpenAiCompatProvider::new(c, http))
    }

    /// Effective capabilities for a provider+model pair: adapter-level flags plus the
    /// model-level `images` resolution (explicit tri-state → server report → heuristic).
    pub fn capabilities_for(&self, provider_id: &str, model: &str) -> Result<Capabilities> {
        let config = self.get(provider_id)?;
        let mut caps = match config.kind {
            ProviderKind::Simulated => FakeProvider::default().capabilities(),
            ProviderKind::OpenAiCompatible => self.build_openai_compat(&config)?.capabilities(),
        };
        caps.images = match config.image_support {
            ImageSupport::Always => true,
            ImageSupport::Never => false,
            ImageSupport::Auto => {
                config.vision_models.iter().any(|m| m == model) || looks_like_vision_model(model)
            }
        };
        Ok(caps)
    }

    /// Discovers models where the protocol supports it and caches them on the config.
    /// Also refreshes `vision_models` when the server exposes capability metadata.
    pub async fn discover_models(&self, id: &str) -> Result<Vec<String>> {
        let config = self.get(id)?;
        let (models, vision) = match config.kind {
            ProviderKind::Simulated => (config.models.clone(), None),
            ProviderKind::OpenAiCompatible => self.build_openai_compat(&config)?.discover().await?,
        };
        self.update_models(id, models.clone())?;
        if let Some(vision) = vision {
            self.update_vision_models(id, vision)?;
        }
        Ok(models)
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
