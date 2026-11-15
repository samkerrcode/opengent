use std::path::PathBuf;

use agent_core::domain::{AgentId, ChatId};
use agent_core::mcp::PermissionScope;
use agent_core::storage::attachments::data_url;
use agent_core::storage::chats as repo;
use agent_core::storage::{AttachmentKind, AttachmentMeta, ChatDetail, ChatSummary};
use agent_core::Error;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use ts_rs::TS;

use crate::state::ATTACHMENTS_DROPPED_EVENT;
use crate::AppState;

const LIST_LIMIT: usize = 500;
const MAX_ATTACHMENTS_PER_PICK: usize = 20;

/// A file the user picked that could not be attached, with a user-facing reason.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct RejectedAttachment {
    pub name: String,
    pub reason: String,
}

/// Picker result: imported attachments plus per-file rejections, so one bad file in a
/// multi-select doesn't discard the rest.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct PickedAttachments {
    pub attached: Vec<AttachmentMeta>,
    pub rejected: Vec<RejectedAttachment>,
}

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
    state
        .db
        .call(move |c| repo::get_chat(c, id).map(ChatDetail::for_display))
        .await
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
    state.db.call(move |c| repo::delete_chat(c, id)).await?;
    state.mcp.remove_scope(PermissionScope::Chat(id));
    Ok(())
}

/// Opens the native file picker and imports the chosen files as immutable attachment
/// snapshots. Paths never come from the webview; the dialog result stays in Rust.
/// The picker is intentionally unfiltered — the store decides by content (any UTF-8 text
/// file, a `%PDF-` file, or a magic-byte-sniffed PNG/JPEG/GIF/WebP), which no extension
/// list can describe without hiding attachable files. Everything importable is imported:
/// whether the chat's model can be sent it is decided by the chat store for display
/// (`AttachmentMeta::unusable_reason`, mirrored in `src/lib/attachments.ts`) and enforced
/// by `launch_run`.
#[tauri::command]
pub async fn pick_attachments(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PickedAttachments, Error> {
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
    Ok(import_paths(&state, paths).await)
}

/// Imports each path as an immutable attachment snapshot, collecting a per-file reason for
/// the ones that fail (too large, unsupported, password-protected or damaged PDF) so one
/// bad file doesn't discard the rest. Shared by the picker above and the native drop
/// handler. Files go one at a time, which bounds memory to a single file and its pages.
async fn import_paths(state: &AppState, paths: Vec<PathBuf>) -> PickedAttachments {
    let mut attached = Vec::with_capacity(paths.len());
    let mut rejected = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_owned();
        match import_path(state, path).await {
            Ok(meta) => attached.push(meta),
            Err(e) => rejected.push(RejectedAttachment {
                name,
                reason: e.to_string(),
            }),
        }
    }
    PickedAttachments { attached, rejected }
}

/// Reading and PDF rendering run on blocking threads; the database worker only looks up
/// the content hash and records the result. A file that was attached before is found by
/// its hash and never rendered again.
async fn import_path(state: &AppState, path: PathBuf) -> Result<AttachmentMeta, Error> {
    let store = state.attachments.clone();
    let file = super::blocking(move || store.load(&path)).await?;
    let store = state.attachments.clone();
    let (existing, file) = state
        .db
        .call(move |conn| Ok((store.find(conn, &file)?, file)))
        .await?;
    if let Some(meta) = existing {
        return Ok(meta);
    }
    let store = state.attachments.clone();
    let prepared = super::blocking(move || store.prepare(file)).await?;
    let store = state.attachments.clone();
    state
        .db
        .call(move |conn| store.commit(conn, prepared))
        .await
}

/// Native drag-and-drop entry point (wired in `lib.rs`'s `on_window_event`): files dropped
/// on the window are imported exactly like picked ones — paths come from the OS drop,
/// never from the webview. The result goes out on `ATTACHMENTS_DROPPED_EVENT`; the frontend
/// merges it into the visible chat's pending list and moves the files its model can't
/// accept to the rejection notice.
pub(crate) fn handle_dropped_paths(app: &AppHandle, paths: &[PathBuf]) {
    let paths: Vec<PathBuf> = paths
        .iter()
        .take(MAX_ATTACHMENTS_PER_PICK)
        .cloned()
        .collect();
    if paths.is_empty() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let picked = import_paths(&state, paths).await;
        if let Err(e) = app.emit(ATTACHMENTS_DROPPED_EVENT, picked) {
            tracing::warn!(error = %e, "could not announce dropped attachments");
        }
    });
}

/// Returns a `data:` URL for an image attachment — or page 1 of a PDF — so the webview
/// can render thumbnails without any file access (CSP allows `img-src data:`).
#[tauri::command]
pub async fn attachment_data_url(
    state: State<'_, AppState>,
    attachment_id: String,
) -> Result<String, Error> {
    let store = state.attachments.clone();
    let (meta, page) = state
        .db
        .call(move |conn| {
            let meta = store
                .get(conn, &attachment_id)?
                .ok_or_else(|| Error::NotFound(format!("attachment {attachment_id}")))?;
            let page = match meta.kind() {
                AttachmentKind::Image => None,
                AttachmentKind::Pdf => Some(store.first_page(conn, &meta)?.ok_or_else(|| {
                    Error::NotFound(format!("pages of attachment {attachment_id}"))
                })?),
                AttachmentKind::Text => {
                    return Err(Error::InvalidInput("attachment has no preview".into()))
                }
            };
            Ok((meta, page))
        })
        .await?;
    let store = state.attachments.clone();
    super::blocking(move || {
        Ok(match page {
            Some(page) => data_url(&page.mime, &store.read_page(&page)?),
            None => data_url(&meta.mime, &store.read_bytes(&meta)?),
        })
    })
    .await
}
