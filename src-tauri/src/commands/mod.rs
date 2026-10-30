//! IPC boundary. Every command validates its input, touches state only through `AppState`,
//! and returns serializable DTOs. Secrets are never returned to the webview.

mod providers;

pub use providers::*;

use agent_core::agent::{context, RunRequest, RunSink};
use agent_core::config::{product, ProductInfo};
use agent_core::domain::{ChatId, ContentBlock, Message, RunEvent, RunId};
use agent_core::secrets::SecretRef;
use agent_core::Error;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;
use ts_rs::TS;

use crate::AppState;

/// Hard cap on a single user prompt; larger inputs go through attachments (Phase 1).
const MAX_PROMPT_CHARS: usize = 64 * 1024;
/// Hard cap on history accepted from the webview per run (Phase 1 moves history into Rust).
const MAX_HISTORY_MESSAGES: usize = 500;

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

/// Adapts a Tauri channel to the core's `RunSink`. Channels deliver in order; a send error
/// means the webview side is gone, which the coordinator treats as cancellation.
struct ChannelSink(Channel<RunEvent>);

impl RunSink for ChannelSink {
    fn send(&self, event: RunEvent) -> bool {
        self.0.send(event).is_ok()
    }
}

/// Starts a run for `chat_id` using the given provider/model. `history` is the full
/// conversation including the new user turn as its last message.
///
/// `async` so Tauri executes it off the main thread.
#[tauri::command]
pub async fn start_run(
    state: State<'_, AppState>,
    chat_id: ChatId,
    provider_id: String,
    model: String,
    history: Vec<Message>,
    on_event: Channel<RunEvent>,
) -> Result<RunId, Error> {
    if model.trim().is_empty() {
        return Err(Error::InvalidInput("choose a model first".into()));
    }
    if history.is_empty() || history.len() > MAX_HISTORY_MESSAGES {
        return Err(Error::InvalidInput("invalid conversation history".into()));
    }
    let last_user_len: usize = history
        .last()
        .map(|m| {
            m.blocks
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => text.chars().count(),
                    _ => 0,
                })
                .sum()
        })
        .unwrap_or(0);
    if last_user_len == 0 {
        return Err(Error::InvalidInput("prompt is empty".into()));
    }
    if last_user_len > MAX_PROMPT_CHARS {
        return Err(Error::InvalidInput(format!(
            "prompt exceeds {MAX_PROMPT_CHARS} characters"
        )));
    }

    let config = state.providers.get(&provider_id)?;
    let provider = state.providers.build(&provider_id)?;
    let compiled = context::compile(
        model.trim(),
        &history,
        config
            .context_window
            .or(provider.capabilities().context_window),
    );
    let mut notices = Vec::new();
    if compiled.trimmed_messages > 0 {
        notices.push(format!(
            "Older messages were left out to fit the model's context window ({} omitted).",
            compiled.trimmed_messages
        ));
    }
    state.coordinator.start(
        RunRequest {
            chat_id,
            provider,
            completion: compiled.request,
            notices,
        },
        ChannelSink(on_event),
    )
}

#[tauri::command]
pub fn cancel_run(state: State<'_, AppState>, run_id: RunId) -> bool {
    state.coordinator.cancel(run_id)
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
