use std::sync::Arc;

use agent_core::agent::RunCoordinator;
use agent_core::config::{resolve_data_root, DataRoot};
use agent_core::mcp::{McpManager, McpStore};
use agent_core::providers::registry::ProviderStore;
use agent_core::secrets::{MemorySecretStore, SecretStore};
use agent_core::storage::{AttachmentStore, Db};
use tauri::{AppHandle, Emitter};

/// Upper bound on simultaneously generating chats.
const MAX_CONCURRENT_RUNS: usize = 4;

/// App event emitted whenever MCP connection status, tool catalogs or policies change;
/// the UI refetches `list_mcp_servers` on it.
pub const MCP_CHANGED_EVENT: &str = "mcp-changed";

pub struct AppState {
    pub data_root: DataRoot,
    pub coordinator: Arc<RunCoordinator>,
    pub secrets: Arc<dyn SecretStore>,
    pub providers: ProviderStore,
    pub mcp: Arc<McpManager>,
    pub db: Db,
    pub attachments: Arc<AttachmentStore>,
}

impl AppState {
    pub fn initialize(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let data_root = resolve_data_root(None)?;
        let secrets: Arc<dyn SecretStore> = match agent_core::secrets::NativeSecretStore::new() {
            Ok(store) => Arc::new(store),
            Err(e) => {
                tracing::warn!(error = %e, "native secret store unavailable; using non-persistent memory store");
                Arc::new(MemorySecretStore::default())
            }
        };
        // Pooled client for providers; remote MCP servers get a redirect-free client so
        // custom credential headers can't leak to a redirect target.
        let http = agent_core::http::client()?;
        let providers = ProviderStore::load(&data_root, secrets.clone(), http)?;
        let db = Db::open(&data_root.database_path())?;
        let attachments = Arc::new(AttachmentStore::new(data_root.attachments_dir()));
        let handle = app.clone();
        let mcp = McpManager::new(
            McpStore::load(&data_root, secrets.clone())?,
            agent_core::http::client_no_redirect()?,
            tauri::async_runtime::handle().inner().clone(),
            move || {
                let _ = handle.emit(MCP_CHANGED_EVENT, ());
            },
        );
        Ok(Self {
            data_root,
            coordinator: RunCoordinator::new(
                MAX_CONCURRENT_RUNS,
                tauri::async_runtime::handle().inner().clone(),
            ),
            secrets,
            providers,
            mcp,
            db,
            attachments,
        })
    }
}
