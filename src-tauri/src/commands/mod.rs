//! IPC boundary. Every command validates its input, touches state only through `AppState`,
//! and returns serializable DTOs. Secrets are never returned to the webview.

mod agents;
mod chats;
mod mcp;
mod providers;

pub use agents::*;
pub use chats::*;
pub use mcp::*;
pub use providers::*;

use agent_core::agent::launcher::{self, RunParams, RunStarted};
use agent_core::agent::RunSink;
use agent_core::agents::ModelRef;
use agent_core::config::{product, ProductInfo};
use agent_core::domain::{ChatId, RunEvent, RunId};
use agent_core::secrets::SecretRef;
use agent_core::settings::Settings;
use agent_core::Error;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;
use ts_rs::TS;

use crate::AppState;

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct AppInfo {
    pub product: ProductInfo,
    pub data_root: String,
    pub secret_backend: String,
    pub platform: &'static str,
    pub debug_build: bool,
}

#[tauri::command]
pub fn get_app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        product: *product(),
        data_root: state.data_root.path().display().to_string(),
        secret_backend: state.secrets.backend().to_owned(),
        platform: std::env::consts::OS,
        debug_build: cfg!(debug_assertions),
    }
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.services.settings.get()
}

/// Partial update (`{ section: { field: value } }`), validated in Rust; returns the
/// settings as stored. On error nothing changes, so the UI reverts the control.
#[tauri::command]
pub fn update_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    patch: serde_json::Value,
) -> Result<Settings, Error> {
    let settings = state.services.settings.update(&patch)?;
    crate::window::apply_theme(&app, settings.appearance.theme);
    Ok(settings)
}

/// Adapts a Tauri channel to the core's `RunSink`. Channels deliver in order; a send error
/// means the webview side is gone, which the coordinator treats as cancellation.
struct ChannelSink(Channel<RunEvent>);

impl RunSink for ChannelSink {
    fn send(&self, event: RunEvent) -> bool {
        self.0.send(event).is_ok()
    }
}

/// Starts a run through `launcher::launch`. Agent chats ignore `provider_id`/`model`: their
/// models, tools and prompt come from the stored agent.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn start_run(
    state: State<'_, AppState>,
    chat_id: ChatId,
    provider_id: Option<String>,
    model: Option<String>,
    prompt: String,
    attachment_ids: Vec<String>,
    replace_from: Option<String>,
    on_event: Channel<RunEvent>,
) -> Result<RunStarted, Error> {
    launcher::launch(
        &state.services,
        RunParams {
            chat_id,
            model: provider_id
                .zip(model)
                .map(|(provider_id, model)| ModelRef { provider_id, model }),
            prompt,
            attachment_ids,
            replace_from,
            unattended: false,
        },
        ChannelSink(on_event),
        |_| {},
    )
    .await
}

/// Runs file- or CPU-bound work off the async runtime and off the database worker.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
}

#[tauri::command]
pub fn cancel_run(state: State<'_, AppState>, run_id: RunId) -> bool {
    state.services.coordinator.cancel(run_id)
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct SecretProbe {
    pub backend: String,
    pub round_trip_ok: bool,
}

/// Debug-only: writes and deletes a throwaway entry to verify the OS credential store works on
/// this machine. Never stores or returns real secrets. Rejected in release builds.
#[tauri::command]
pub fn probe_secret_store(state: State<'_, AppState>) -> Result<SecretProbe, Error> {
    if !cfg!(debug_assertions) {
        return Err(Error::InvalidInput(
            "secret store probe is only available in debug builds".into(),
        ));
    }
    let key = SecretRef::new(format!("probe/{}", RunId::new()))?;
    let value = RunId::new().to_string();
    state.secrets.set(&key, &value)?;
    let read_back = state.secrets.get(&key)?;
    state.secrets.delete(&key)?;
    Ok(SecretProbe {
        backend: state.secrets.backend().to_owned(),
        round_trip_ok: read_back.as_deref() == Some(value.as_str()),
    })
}
