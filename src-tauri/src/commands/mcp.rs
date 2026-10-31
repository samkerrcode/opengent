use std::sync::Arc;

use agent_core::agent::tools::ToolSettings;
use agent_core::domain::{ApprovalDecision, RunId, ToolPolicy};
use agent_core::mcp::{McpManager, McpServerInput, McpServerView};
use agent_core::Error;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::AppState;

#[tauri::command]
pub fn list_mcp_servers(state: State<'_, AppState>) -> Vec<McpServerView> {
    state.mcp.list_views()
}

/// Validates and persists a server config (secrets to the keychain). If the server is
/// enabled it (re)connects in the background — status changes arrive via `mcp-changed`.
/// A new stdio server, or a change to what one launches (command, args, cwd, env), must be
/// confirmed through a native dialog first: it runs arbitrary code, so it can't be driven
/// silently by the webview.
#[tauri::command]
pub async fn save_mcp_server(
    app: AppHandle,
    state: State<'_, AppState>,
    input: McpServerInput,
) -> Result<(), Error> {
    // An empty command is rejected by `save` validation; don't prompt for it.
    if !input.command.trim().is_empty() && state.mcp.store().launch_changed(&input) {
        let env_names: Vec<&str> = input.env.iter().map(|e| e.name.trim()).collect();
        let mut detail = input.command.trim().to_owned();
        if let Some(cwd) = input
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            detail.push_str(&format!("\n\nWorking directory: {cwd}"));
        }
        if !env_names.is_empty() {
            detail.push_str(&format!("\n\nEnvironment: {}", env_names.join(", ")));
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.dialog()
            .message(format!(
                "This MCP server will run a local program:\n\n{detail}\n\nOnly allow it if you trust its source."
            ))
            .title("Allow MCP server?")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Allow".into(),
                "Cancel".into(),
            ))
            .show(move |ok| {
                let _ = tx.send(ok);
            });
        if !rx.await.unwrap_or(false) {
            return Err(Error::InvalidInput("server command not confirmed".into()));
        }
    }
    let config = state.mcp.store().save(input)?;
    let id = config.id.clone();
    let manager: Arc<McpManager> = state.mcp.clone();
    tauri::async_runtime::spawn(async move {
        manager.connect(&id).await;
    });
    Ok(())
}

#[tauri::command]
pub fn delete_mcp_server(state: State<'_, AppState>, id: String) -> Result<(), Error> {
    // Delete from the store first so `disconnect` cleans up status rather than
    // resurrecting an `Idle` entry for a server that no longer exists.
    state.mcp.store().delete(&id)?;
    state.mcp.disconnect(&id);
    Ok(())
}

/// Toggles a server for future runs; connects or disconnects it now.
#[tauri::command]
pub async fn set_mcp_server_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<(), Error> {
    state.mcp.store().set_enabled(&id, enabled)?;
    if enabled {
        let manager = state.mcp.clone();
        tauri::async_runtime::spawn(async move {
            manager.connect(&id).await;
        });
    } else {
        state.mcp.disconnect(&id);
    }
    Ok(())
}

#[tauri::command]
pub async fn reconnect_mcp_server(state: State<'_, AppState>, id: String) -> Result<(), Error> {
    let manager = state.mcp.clone();
    tauri::async_runtime::spawn(async move {
        manager.connect(&id).await;
    });
    Ok(())
}

#[tauri::command]
pub fn set_mcp_tool_policy(
    state: State<'_, AppState>,
    server_id: String,
    tool: String,
    policy: ToolPolicy,
) -> Result<(), Error> {
    // Route through the manager so the tool's definition signature is recorded with
    // `always_allow` grants (verified at dispatch, shown as "changed" when it drifts).
    state.mcp.set_policy(&server_id, &tool, policy);
    Ok(())
}

#[tauri::command]
pub fn set_mcp_default_policy(
    state: State<'_, AppState>,
    server_id: String,
    policy: ToolPolicy,
) -> Result<(), Error> {
    state.mcp.store().set_default_policy(&server_id, policy)?;
    Ok(())
}

#[tauri::command]
pub fn get_tool_settings(state: State<'_, AppState>) -> ToolSettings {
    state.mcp.store().settings()
}

#[tauri::command]
pub fn set_tool_settings(state: State<'_, AppState>, settings: ToolSettings) -> Result<(), Error> {
    state.mcp.store().set_settings(settings)
}

/// The user's answer to a tool approval prompt. Returns false if the call is no longer
/// waiting (e.g. the run was cancelled first).
#[tauri::command]
pub fn resolve_tool_approval(
    state: State<'_, AppState>,
    run_id: RunId,
    call_id: String,
    decision: ApprovalDecision,
) -> bool {
    state
        .coordinator
        .resolve_approval(run_id, &call_id, decision)
}
