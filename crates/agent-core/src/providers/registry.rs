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

use super::fake::FakeProvider;
use super::openai_compat::{normalize_base_url, OpenAiCompatConfig, OpenAiCompatProvider};
use super::ModelProvider;
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
            models: existing.map(|p| p.models).unwrap_or_default(),
            has_api_key,
            include_usage: input.include_usage,
            context_window: input.context_window,
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
        Ok(OpenAiCompatProvider::new(c, self.http.clone()))
    }

    /// Discovers models where the protocol supports it and caches them on the config.
    pub async fn discover_models(&self, id: &str) -> Result<Vec<String>> {
        let config = self.get(id)?;
        let models = match config.kind {
            ProviderKind::Simulated => config.models.clone(),
            ProviderKind::OpenAiCompatible => {
                self.build_openai_compat(&config)?.list_models().await?
            }
        };
        self.update_models(id, models.clone())?;
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
