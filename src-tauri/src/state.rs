use std::sync::Arc;
use std::time::Duration;

use agent_core::agent::RunCoordinator;
use agent_core::config::{product, resolve_data_root, DataRoot};
use agent_core::providers::registry::ProviderStore;
use agent_core::secrets::{MemorySecretStore, SecretStore};

/// Upper bound on simultaneously generating chats.
const MAX_CONCURRENT_RUNS: usize = 4;

pub struct AppState {
    pub data_root: DataRoot,
    pub coordinator: Arc<RunCoordinator>,
    pub secrets: Arc<dyn SecretStore>,
    pub providers: ProviderStore,
}

impl AppState {
    pub fn initialize() -> Result<Self, Box<dyn std::error::Error>> {
        let data_root = resolve_data_root(None)?;
        let secrets: Arc<dyn SecretStore> = match agent_core::secrets::NativeSecretStore::new() {
            Ok(store) => Arc::new(store),
            Err(e) => {
                tracing::warn!(error = %e, "native secret store unavailable; using non-persistent memory store");
                Arc::new(MemorySecretStore::default())
            }
        };
        // One pooled client for all providers. Per-request timeouts are set by the adapters.
        let http = reqwest::Client::builder()
            .user_agent(format!("{}/{}", product().slug, product().version))
            .connect_timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()?;
        let providers = ProviderStore::load(&data_root, secrets.clone(), http)?;
        Ok(Self {
            data_root,
            coordinator: RunCoordinator::new(
                MAX_CONCURRENT_RUNS,
                tauri::async_runtime::handle().inner().clone(),
            ),
            secrets,
            providers,
        })
    }
}
