//! IPC boundary. Every command validates its input, touches state only through `AppState`,
//! and returns serializable DTOs. Secrets are never returned to the webview.

mod chats;
mod mcp;
mod providers;

pub use chats::*;
pub use mcp::*;
pub use providers::*;

use std::collections::HashMap;

use agent_core::agent::context::{self, AttachmentContent, ToolContext};
use agent_core::agent::{RunRecorder, RunRequest, RunSink};
use agent_core::config::{product, ProductInfo};
use agent_core::domain::{ChatId, ContentBlock, Role, RunEvent, RunId};
use agent_core::secrets::SecretRef;
use agent_core::storage::chats as repo;
use agent_core::storage::StoredMessage;
use agent_core::Error;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;
use ts_rs::TS;

use crate::AppState;

/// Hard cap on a single user prompt; larger inputs go through attachments.
const MAX_PROMPT_CHARS: usize = 64 * 1024;
const MAX_ATTACHMENTS_PER_MESSAGE: usize = 20;

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

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct RunStarted {
    pub run_id: RunId,
    pub user_message: StoredMessage,
    pub assistant_message: StoredMessage,
}

/// Persists the user turn, creates the assistant placeholder, compiles context from stored
/// history and starts streaming. If `replace_from` is set (retry), that message and everything
/// after it is removed first.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn start_run(
    state: State<'_, AppState>,
    chat_id: ChatId,
    provider_id: String,
    model: String,
    prompt: String,
    attachment_ids: Vec<String>,
    replace_from: Option<String>,
    on_event: Channel<RunEvent>,
) -> Result<RunStarted, Error> {
    let model = model.trim().to_owned();
    let prompt = prompt.trim().to_owned();
    if model.is_empty() {
        return Err(Error::InvalidInput("choose a model first".into()));
    }
    if prompt.is_empty() && attachment_ids.is_empty() {
        return Err(Error::InvalidInput("prompt is empty".into()));
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(Error::InvalidInput(format!(
            "prompt exceeds {MAX_PROMPT_CHARS} characters"
        )));
    }
    if attachment_ids.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(Error::InvalidInput("too many attachments".into()));
    }
    if state.coordinator.active_run(chat_id).is_some() {
        return Err(Error::RunBusy);
    }

    let provider = state.providers.build(&provider_id)?;
    // capabilities_for resolves the context window per provider+model:
    // per-model override → provider default → adapter-declared.
    let caps = state.providers.capabilities_for(&provider_id, &model)?;
    let context_window = caps.context_window;
    let run_id = RunId::new();

    // One database round trip: truncate (retry), persist the user turn, create the placeholder,
    // and load everything needed to compile the request.
    let store = state.attachments.clone();
    let (pid, mdl) = (provider_id.clone(), model.clone());
    let allow_images = caps.images;
    let prepared = state
        .db
        .call(move |conn| {
            if let Some(from) = replace_from {
                repo::truncate_from(conn, chat_id, &from)?;
            }
            let mut blocks = Vec::new();
            let mut contents = HashMap::new();
            for id in &attachment_ids {
                let meta = store
                    .get(conn, id)?
                    .ok_or_else(|| Error::NotFound(format!("attachment {id}")))?;
                let is_image = meta.mime.starts_with("image/");
                if is_image && !allow_images {
                    return Err(Error::InvalidInput(format!(
                        "{}: the selected model does not accept image attachments",
                        meta.name
                    )));
                }
                contents.insert(
                    meta.id.clone(),
                    if is_image {
                        AttachmentContent::image(meta.mime.clone(), &store.read_bytes(&meta)?)
                    } else {
                        AttachmentContent::Text(store.read_text(&meta)?)
                    },
                );
                blocks.push(ContentBlock::Attachment {
                    id: meta.id,
                    name: meta.name,
                    mime: meta.mime,
                    char_count: meta.char_count,
                });
            }
            if !prompt.is_empty() {
                blocks.push(ContentBlock::Text { text: prompt });
            }
            let user_id =
                repo::append_message(conn, chat_id, Role::User, &blocks, &attachment_ids)?;
            let assistant_id = repo::begin_assistant_message(conn, chat_id, run_id, &pid, &mdl)?;
            let messages = repo::list_messages(conn, chat_id)?;
            // Attachment payloads for earlier turns too, so history stays coherent.
            for m in &messages {
                for b in &m.blocks {
                    if let ContentBlock::Attachment { id, .. } = b {
                        if !contents.contains_key(id) {
                            if let Some(meta) = store.get(conn, id)? {
                                let content = if meta.mime.starts_with("image/") {
                                    store
                                        .read_bytes(&meta)
                                        .ok()
                                        .map(|b| AttachmentContent::image(meta.mime.clone(), &b))
                                } else {
                                    store.read_text(&meta).ok().map(AttachmentContent::Text)
                                };
                                if let Some(c) = content {
                                    contents.insert(id.clone(), c);
                                }
                            }
                        }
                    }
                }
            }
            Ok((user_id, assistant_id, messages, contents))
        })
        .await?;
    let (user_id, assistant_id, messages, contents) = prepared;

    let user_message = messages
        .iter()
        .find(|m| m.id == user_id)
        .cloned()
        .ok_or_else(|| Error::Internal("user message vanished".into()))?;
    let assistant_message = messages
        .iter()
        .find(|m| m.id == assistant_id)
        .cloned()
        .ok_or_else(|| Error::Internal("assistant message vanished".into()))?;

    // History = everything complete/interrupted before the placeholder, as domain messages.
    let history: Vec<_> = messages
        .iter()
        .filter(|m| m.id != assistant_id && !m.blocks.is_empty())
        .map(StoredMessage::to_message)
        .collect();

    // Tool snapshot: catalog from connected servers (skipped when the model can't call
    // tools), the system-prompt section, and a token reservation for the definitions.
    let mut tool_ctx = ToolContext::default();
    let mut run_tools = None;
    if caps.tools {
        if let Some((tools, prompt, def_tokens)) = state.mcp.prepare_tools(
            &history,
            context_window.unwrap_or(context::DEFAULT_CONTEXT_TOKENS),
        ) {
            tool_ctx.definition_tokens = def_tokens;
            tool_ctx.prompt = prompt;
            run_tools = Some(tools);
        }
    }

    let compiled = context::compile(
        &model,
        &history,
        context_window,
        &contents,
        caps.images,
        &tool_ctx,
    );
    let mut notices = Vec::new();
    if compiled.trimmed_messages > 0 {
        notices.push(format!(
            "Older messages were left out to fit the model's context window ({} omitted).",
            compiled.trimmed_messages
        ));
    }

    let sink = RunRecorder::new(ChannelSink(on_event), state.db.clone(), assistant_id);
    let started = state.coordinator.start_with_id(
        run_id,
        RunRequest {
            chat_id,
            provider,
            completion: compiled.request,
            tools: run_tools,
            notices,
        },
        sink,
    );
    if let Err(e) = started {
        let id = assistant_message.id.clone();
        let msg = e.to_string();
        state.db.spawn_job(move |c| {
            repo::finalize_assistant_message(
                c,
                &id,
                &[],
                agent_core::storage::MessageStatus::Failed,
                None,
                Some(&msg),
            )
        });
        return Err(e);
    }
    Ok(RunStarted {
        run_id,
        user_message,
        assistant_message,
    })
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
