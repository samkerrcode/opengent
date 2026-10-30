use std::path::PathBuf;

use agent_core::domain::ChatId;
use agent_core::storage::chats as repo;
use agent_core::storage::{AttachmentMeta, ChatDetail, ChatSummary};
use agent_core::Error;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::AppState;

const LIST_LIMIT: usize = 500;
const MAX_ATTACHMENTS_PER_PICK: usize = 20;

#[tauri::command]
pub async fn list_chats(state: State<'_, AppState>) -> Result<Vec<ChatSummary>, Error> {
    state.db.call(|c| repo::list_chats(c, LIST_LIMIT)).await
}

#[tauri::command]
pub async fn search_chats(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<ChatSummary>, Error> {
    let q = query.trim().to_owned();
    if q.is_empty() {
        return state.db.call(|c| repo::list_chats(c, LIST_LIMIT)).await;
    }
    state
        .db
        .call(move |c| repo::search_chats(c, &q, LIST_LIMIT))
        .await
}

#[tauri::command]
pub async fn create_chat(state: State<'_, AppState>) -> Result<ChatSummary, Error> {
    state.db.call(|c| repo::create_chat(c, "New chat")).await
}

#[tauri::command]
pub async fn get_chat(state: State<'_, AppState>, id: ChatId) -> Result<ChatDetail, Error> {
    state.db.call(move |c| repo::get_chat(c, id)).await
}

#[tauri::command]
pub async fn rename_chat(
    state: State<'_, AppState>,
    id: ChatId,
    title: String,
) -> Result<(), Error> {
    state
        .db
        .call(move |c| repo::rename_chat(c, id, &title))
        .await
}

#[tauri::command]
pub async fn delete_chat(state: State<'_, AppState>, id: ChatId) -> Result<(), Error> {
    if let Some(run) = state.coordinator.active_run(id) {
        state.coordinator.cancel(run);
    }
    state.db.call(move |c| repo::delete_chat(c, id)).await
}

/// Opens the native file picker and imports the chosen files as immutable attachment
/// snapshots. Paths never come from the webview; the dialog result stays in Rust.
#[tauri::command]
pub async fn pick_attachments(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<AttachmentMeta>, Error> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Attach files")
            .blocking_pick_files()
            .unwrap_or_default()
    })
    .await
    .map_err(|e| Error::Internal(e.to_string()))?;
    let paths: Vec<PathBuf> = picked
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .take(MAX_ATTACHMENTS_PER_PICK)
        .collect();
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let store = state.attachments.clone();
    state
        .db
        .call(move |conn| {
            let mut out = Vec::with_capacity(paths.len());
            for path in &paths {
                match store.import_file(conn, path) {
                    Ok(meta) => out.push(meta),
                    Err(e) => return Err(Error::InvalidInput(format!("{}: {e}", path.display()))),
                }
            }
            Ok(out)
        })
        .await
}
