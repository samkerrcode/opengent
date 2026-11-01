use std::path::PathBuf;

use agent_core::domain::{AgentId, ChatId};
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

/// Creates an empty chat, optionally bound to a user-configured agent.
#[tauri::command]
pub async fn create_chat(
    state: State<'_, AppState>,
    agent_id: Option<AgentId>,
) -> Result<ChatSummary, Error> {
    state
        .db
        .call(move |c| repo::create_chat_with(c, "New chat", agent_id, None))
        .await
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
/// `include_images` reflects the active model's image capability — image files picked
/// anyway are rejected with a clear error.
#[tauri::command]
pub async fn pick_attachments(
    app: AppHandle,
    state: State<'_, AppState>,
    include_images: bool,
) -> Result<Vec<AttachmentMeta>, Error> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app.dialog().file().set_title("Attach files");
        if include_images {
            dialog = dialog
                .add_filter("Images and text", &["png", "jpg", "jpeg", "gif", "webp"])
                .add_filter("All files", &["*"]);
        }
        dialog.blocking_pick_files().unwrap_or_default()
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
                    Ok(meta) if meta.mime.starts_with("image/") && !include_images => {
                        return Err(Error::InvalidInput(format!(
                            "{}: the selected model does not accept image attachments",
                            path.display()
                        )));
                    }
                    Ok(meta) => out.push(meta),
                    Err(e) => return Err(Error::InvalidInput(format!("{}: {e}", path.display()))),
                }
            }
            Ok(out)
        })
        .await
}

/// Returns a `data:` URL for an image attachment so the webview can render thumbnails
/// without any file access (CSP allows `img-src data:`).
#[tauri::command]
pub async fn attachment_data_url(
    state: State<'_, AppState>,
    attachment_id: String,
) -> Result<String, Error> {
    let store = state.attachments.clone();
    state
        .db
        .call(move |conn| {
            let meta = store
                .get(conn, &attachment_id)?
                .ok_or_else(|| Error::NotFound(format!("attachment {attachment_id}")))?;
            if !meta.mime.starts_with("image/") {
                return Err(Error::InvalidInput("attachment is not an image".into()));
            }
            store.read_data_url(&meta)
        })
        .await
}
