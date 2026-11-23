use std::collections::HashSet;
use std::sync::Arc;

use agent_core::agent::launcher::RunServices;
use agent_core::agent::RunCoordinator;
use agent_core::config::{resolve_data_root, DataRoot};
use agent_core::mcp::{McpManager, McpStore, PermissionScope, ScopePolicies};
use agent_core::providers::registry::ProviderStore;
use agent_core::secrets::{MemorySecretStore, SecretStore};
use agent_core::settings::SettingsStore;
use agent_core::storage::agents as agents_repo;
use agent_core::storage::chats as chats_repo;
use agent_core::storage::{AttachmentStore, Db, ProcessLock};
use tauri::{AppHandle, Emitter};

/// Upper bound on simultaneously generating chats.
const MAX_CONCURRENT_RUNS: usize = 4;

/// App event emitted whenever MCP connection status, tool catalogs or policies change;
/// the UI refetches `list_mcp_servers` on it.
pub const MCP_CHANGED_EVENT: &str = "mcp-changed";
/// Emitted when the scheduler updates schedules (next due time, last outcome).
pub const AGENTS_CHANGED_EVENT: &str = "agents-changed";
/// A scheduled run started (payload: `RunStarted`); its events follow as `BACKGROUND_RUN_EVENT`.
pub const BACKGROUND_RUN_STARTED_EVENT: &str = "background-run-started";
/// One `RunEvent` of a scheduled run (interactive runs use their own channel instead).
pub const BACKGROUND_RUN_EVENT: &str = "background-run-event";
/// Files dropped on the window were imported (payload: `PickedAttachments`); the chat
/// store merges them into the visible chat's pending list.
pub const ATTACHMENTS_DROPPED_EVENT: &str = "attachments-dropped";
/// A picked or dropped PDF is being rendered (payload: `ImportProgress`), so the composer
/// can show what it is waiting for.
pub const ATTACHMENTS_IMPORTING_EVENT: &str = "attachments-importing";
/// The macOS Settings… menu item (⌘,) was invoked. WKWebView never reports ⌘, as a
/// `keydown` — AppKit claims key equivalents before the view sees them — so the shortcut
/// has to live on the native menu. `Ctrl+,` elsewhere goes through the webview key handler.
pub const OPEN_SETTINGS_EVENT: &str = "open-settings";

/// Persists approval-card "Always allow" grants to the chat or agent they were made in.
/// The run already uses the grant in memory; if the write fails it is logged and the UI is
/// told to re-read, so the next run sees the persisted state (where the tool still asks).
fn install_grant_hook(mcp: &McpManager, db: &Db, app: &AppHandle) {
    let (db, app) = (db.clone(), app.clone());
    mcp.set_grant_hook(move |scope, policies| {
        let app = app.clone();
        db.spawn_job(move |c| {
            let result = match (scope, policies) {
                (PermissionScope::Chat(id), ScopePolicies::Chat(o)) => {
                    chats_repo::set_tool_overrides(c, id, &o)
                }
                (PermissionScope::Agent(id), ScopePolicies::Agent(servers)) => {
                    agents_repo::set_agent_servers(c, id, servers).map(|_| ())
                }
                _ => Ok(()),
            };
            let _ = app.emit(MCP_CHANGED_EVENT, ());
            if matches!(scope, PermissionScope::Agent(_)) {
                let _ = app.emit(AGENTS_CHANGED_EVENT, ());
            }
            result
        });
    });
}

/// Drops scoped signature pins whose chat or agent no longer exists.
fn prune_orphaned_scopes(mcp: &McpManager, db: &Db) {
    let live = db.call_blocking(|c| {
        let mut keys: HashSet<String> = chats_repo::list_chat_ids(c, None)?
            .into_iter()
            .map(|id| PermissionScope::Chat(id).key())
            .collect();
        keys.extend(
            agents_repo::list_agents(c)?
                .into_iter()
                .map(|a| PermissionScope::Agent(a.id).key()),
        );
        Ok(keys)
    });
    match live {
        Ok(keys) => mcp.store().prune_scopes(|k| keys.contains(k)),
        Err(e) => tracing::warn!(error = %e, "skipped pruning tool permission scopes"),
    }
}

/// Host-only handles plus the `RunServices` shared with the run launcher; commands read
/// stores through `services`, so there is a single copy of each.
pub struct AppState {
    pub data_root: DataRoot,
    pub secrets: Arc<dyn SecretStore>,
    pub services: RunServices,
    /// Held for the app's lifetime so a CLI process sharing the data root knows it isn't
    /// alone (and leaves this app's live replies alone).
    _process_lock: ProcessLock,
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
        // Separate pools for providers and remote MCP servers; both are redirect-free so
        // custom credential headers can't leak to a redirect target.
        let http = agent_core::http::client()?;
        let providers = Arc::new(ProviderStore::load(&data_root, secrets.clone(), http)?);
        let settings = Arc::new(SettingsStore::load(&data_root)?);
        let (db, process_lock) = Db::open_root(&data_root)?;
        if !process_lock.was_alone() {
            tracing::info!("another process is using the data folder; skipped startup recovery");
        }
        let attachments = Arc::new(AttachmentStore::new(data_root.attachments_dir()));
        let handle = app.clone();
        let mcp = McpManager::new(
            McpStore::load(&data_root, secrets.clone())?,
            agent_core::http::client()?,
            tauri::async_runtime::handle().inner().clone(),
            move || {
                let _ = handle.emit(MCP_CHANGED_EVENT, ());
            },
        );
        install_grant_hook(&mcp, &db, app);
        prune_orphaned_scopes(&mcp, &db);
        Ok(Self {
            data_root,
            secrets,
            services: RunServices {
                db,
                providers,
                attachments,
                mcp,
                coordinator: RunCoordinator::new(
                    MAX_CONCURRENT_RUNS,
                    tauri::async_runtime::handle().inner().clone(),
                ),
                settings,
            },
            _process_lock: process_lock,
        })
    }
}
