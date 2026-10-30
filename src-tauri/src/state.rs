use std::sync::Arc;

use agent_core::agent::RunCoordinator;
use agent_core::config::{resolve_data_root, DataRoot};
use agent_core::providers::fake::FakeProvider;
use agent_core::providers::ModelProvider;
use agent_core::secrets::{MemorySecretStore, SecretStore};

/// Upper bound on simultaneously generating chats.
const MAX_CONCURRENT_RUNS: usize = 4;

pub struct AppState {
    pub data_root: DataRoot,
    pub coordinator: Arc<RunCoordinator>,
    pub secrets: Arc<dyn SecretStore>,
    /// Phase 0 ships only the in-process fake provider; the registry replaces this in Phase 2.
    pub provider: Arc<dyn ModelProvider>,
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
        Ok(Self {
            data_root,
            coordinator: RunCoordinator::new(
                MAX_CONCURRENT_RUNS,
                tauri::async_runtime::handle().inner().clone(),
            ),
            secrets,
            provider: Arc::new(FakeProvider::default()),
        })
    }
}
