use std::sync::Arc;

use agent_core::agent::tools::ToolSettings;
use agent_core::agents::{Agent, AgentServerAccess};
use agent_core::domain::{AgentId, ApprovalDecision, ChatId, RunId, ToolPolicy};
use agent_core::mcp::permissions::clean_tool_name;
use agent_core::mcp::{
    ChatToolOverrides, McpManager, McpServerInput, McpServerView, PermissionScope, ScopePolicies,
    ScopedServerView,
};
use agent_core::storage::agents as agents_repo;
use agent_core::storage::chats as chats_repo;
use agent_core::Error;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::state::AGENTS_CHANGED_EVENT;
use crate::AppState;

#[tauri::command]
pub fn list_mcp_servers(state: State<'_, AppState>) -> Vec<McpServerView> {
    state.mcp.list_views()
}

/// Validates and persists a server config (secrets to the keychain) and returns its id. An
/// enabled server reconnects when `connect` is set ("Save & test" — awaited, so the result
/// is known on return) or, in the background, when the save changed how it is reached; a
/// plain save that only renames or edits permissions keeps the live session. Disabled
/// servers never connect. Status changes arrive via `mcp-changed`.
/// A new stdio server, or a change to what one launches (command, args, cwd, env), must be
/// confirmed through a native dialog first: it runs arbitrary code, so it can't be driven
/// silently by the webview.
#[tauri::command]
pub async fn save_mcp_server(
    app: AppHandle,
    state: State<'_, AppState>,
    input: McpServerInput,
    connect: bool,
) -> Result<String, Error> {
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
    let changed = state.mcp.store().connection_changed(&input);
    let config = state.mcp.store().save(input)?;
    if config.enabled {
        if connect {
            // "Save & test" waits for the outcome so the UI can report it.
            state.mcp.connect(&config.id).await;
        } else if changed {
            let id = config.id.clone();
            let manager: Arc<McpManager> = state.mcp.clone();
            tauri::async_runtime::spawn(async move {
                manager.connect(&id).await;
            });
        }
    }
    Ok(config.id)
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

/// Clears per-tool overrides so every tool follows the server default.
#[tauri::command]
pub fn reset_mcp_tool_policies(state: State<'_, AppState>, server_id: String) -> Result<(), Error> {
    state.mcp.store().reset_tool_policies(&server_id)
}

#[tauri::command]
pub fn get_tool_settings(state: State<'_, AppState>) -> ToolSettings {
    state.mcp.store().settings()
}

#[tauri::command]
pub fn set_tool_settings(state: State<'_, AppState>, settings: ToolSettings) -> Result<(), Error> {
    state.mcp.store().set_settings(settings)
}

/// Reads a scope's permissions from the database (chat overrides or the agent's access).
pub(crate) async fn load_scope(
    state: &AppState,
    scope: PermissionScope,
) -> Result<ScopePolicies, Error> {
    state
        .db
        .call(move |c| match scope {
            PermissionScope::Chat(id) => {
                chats_repo::get_tool_overrides(c, id).map(ScopePolicies::Chat)
            }
            PermissionScope::Agent(id) => agents_repo::get_agent(c, id).map(|a| a.scope_policies()),
        })
        .await
}

/// Permissions of every server as seen from a chat or agent (⚒ Tools menu, agent editor).
#[tauri::command]
pub async fn list_tool_permissions(
    state: State<'_, AppState>,
    scope: PermissionScope,
) -> Result<Vec<ScopedServerView>, Error> {
    let policies = load_scope(&state, scope).await?;
    state.mcp.set_scope(scope, policies);
    Ok(state.mcp.list_scoped(&scope))
}

fn known_server(state: &AppState, server_id: &str) -> Result<(), Error> {
    state.mcp.store().get(server_id).map(|_| ())
}

/// Read-modify-write of a chat's overrides; the change applies to the chat's next tool
/// call (dispatch re-reads the installed scope), not only its next run.
async fn update_chat_overrides(
    state: &AppState,
    chat_id: ChatId,
    edit: impl FnOnce(&mut ChatToolOverrides) + Send + 'static,
) -> Result<(), Error> {
    let known: Vec<String> = state.mcp.store().list().into_iter().map(|s| s.id).collect();
    let overrides = state
        .db
        .call(move |c| {
            let mut o = chats_repo::get_tool_overrides(c, chat_id)?;
            edit(&mut o);
            let o = o.validated(|s| known.iter().any(|k| k == s))?;
            chats_repo::set_tool_overrides(c, chat_id, &o)?;
            Ok(o)
        })
        .await?;
    state.mcp.set_scope(
        PermissionScope::Chat(chat_id),
        ScopePolicies::Chat(overrides),
    );
    Ok(())
}

/// Turns a server on or off for one chat only; the connection stays up elsewhere.
#[tauri::command]
pub async fn set_chat_server_enabled(
    state: State<'_, AppState>,
    chat_id: ChatId,
    server_id: String,
    enabled: bool,
) -> Result<(), Error> {
    known_server(&state, &server_id)?;
    update_chat_overrides(&state, chat_id, move |o| {
        o.set_server_off(&server_id, !enabled)
    })
    .await
}

/// Sets a tool's policy for one chat; `None` returns it to the connection's policy.
#[tauri::command]
pub async fn set_chat_tool_policy(
    state: State<'_, AppState>,
    chat_id: ChatId,
    server_id: String,
    tool: String,
    policy: Option<ToolPolicy>,
) -> Result<(), Error> {
    known_server(&state, &server_id)?;
    let tool = clean_tool_name(&tool)?;
    update_chat_overrides(&state, chat_id, move |o| {
        o.set_tool(&server_id, &tool, policy)
    })
    .await
}

/// Clears a chat's overrides for one server, or all of them.
#[tauri::command]
pub async fn reset_chat_tool_permissions(
    state: State<'_, AppState>,
    chat_id: ChatId,
    server_id: Option<String>,
) -> Result<(), Error> {
    update_chat_overrides(&state, chat_id, move |o| o.reset(server_id.as_deref())).await
}

/// Replaces (or with `None` removes) one server's access in an agent — edits made from an
/// agent chat's ⚒ Tools menu. Returns the updated agent.
#[tauri::command]
pub async fn set_agent_server_access(
    app: AppHandle,
    state: State<'_, AppState>,
    agent_id: AgentId,
    server_id: String,
    access: Option<AgentServerAccess>,
) -> Result<Agent, Error> {
    known_server(&state, &server_id)?;
    if access.as_ref().is_some_and(|a| a.server_id != server_id) {
        return Err(Error::InvalidInput("server id mismatch".into()));
    }
    let agent = state
        .db
        .call(move |c| {
            let mut servers = agents_repo::get_agent(c, agent_id)?.servers;
            match (
                servers.iter().position(|s| s.server_id == server_id),
                access,
            ) {
                (Some(i), Some(a)) => servers[i] = a,
                (Some(i), None) => {
                    servers.remove(i);
                }
                (None, Some(a)) => servers.push(a),
                (None, None) => {}
            }
            agents_repo::set_agent_servers(c, agent_id, servers)
        })
        .await?;
    state
        .mcp
        .set_scope(PermissionScope::Agent(agent.id), agent.scope_policies());
    let _ = app.emit(AGENTS_CHANGED_EVENT, ());
    Ok(agent)
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
