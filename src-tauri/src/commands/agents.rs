use agent_core::agents::{Agent, AgentInput, AgentSchedule, ScheduleInput, ScheduleOutcome};
use agent_core::domain::{AgentId, ScheduleId};
use agent_core::mcp::PermissionScope;
use agent_core::storage::agents as repo;
use agent_core::storage::chats as chats_repo;
use agent_core::storage::ChatSummary;
use agent_core::Error;
use chrono::Local;
use tauri::{AppHandle, State};

use crate::scheduler;
use crate::AppState;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[tauri::command]
pub async fn list_agents(state: State<'_, AppState>) -> Result<Vec<Agent>, Error> {
    state.db.call(|c| repo::list_agents(c)).await
}

/// Validates and stores an agent. Providers must exist when saved; a provider removed later
/// is reported when the agent next runs.
#[tauri::command]
pub async fn save_agent(state: State<'_, AppState>, input: AgentInput) -> Result<Agent, Error> {
    let check = |provider_id: &str, what: &str| {
        state
            .providers
            .get(provider_id.trim())
            .map_err(|_| Error::InvalidInput(format!("the {what}'s provider no longer exists")))
    };
    check(&input.default_model.provider_id, "default model")?;
    if let Some(fb) = input
        .fallback_model
        .as_ref()
        .filter(|f| !f.provider_id.trim().is_empty())
    {
        check(&fb.provider_id, "fallback model")?;
    }
    let agent = state.db.call(move |c| repo::save_agent(c, input)).await?;
    // Refresh any running agent chats and reconcile the agent's signature pins.
    state
        .mcp
        .set_scope(PermissionScope::Agent(agent.id), agent.scope_policies());
    Ok(agent)
}

/// Deletes the agent; its running chats are cancelled and its permissions forgotten (a run
/// still holding the scope fails closed — every tool asks).
#[tauri::command]
pub async fn delete_agent(state: State<'_, AppState>, id: AgentId) -> Result<(), Error> {
    let chats = state
        .db
        .call(move |c| chats_repo::list_chat_ids(c, Some(id)))
        .await?;
    for chat in chats {
        if let Some(run) = state.coordinator.active_run(chat) {
            state.coordinator.cancel(run);
        }
    }
    state.db.call(move |c| repo::delete_agent(c, id)).await?;
    state.mcp.remove_scope(PermissionScope::Agent(id));
    Ok(())
}

/// All schedules, or one agent's.
#[tauri::command]
pub async fn list_schedules(
    state: State<'_, AppState>,
    agent_id: Option<AgentId>,
) -> Result<Vec<AgentSchedule>, Error> {
    state
        .db
        .call(move |c| repo::list_schedules(c, agent_id))
        .await
}

#[tauri::command]
pub async fn save_schedule(
    state: State<'_, AppState>,
    input: ScheduleInput,
) -> Result<AgentSchedule, Error> {
    state
        .db
        .call(move |c| repo::save_schedule(c, input, now_ms(), &Local))
        .await
}

#[tauri::command]
pub async fn set_schedule_enabled(
    state: State<'_, AppState>,
    id: ScheduleId,
    enabled: bool,
) -> Result<AgentSchedule, Error> {
    state
        .db
        .call(move |c| repo::set_schedule_enabled(c, id, enabled, now_ms(), &Local))
        .await
}

#[tauri::command]
pub async fn delete_schedule(state: State<'_, AppState>, id: ScheduleId) -> Result<(), Error> {
    state.db.call(move |c| repo::delete_schedule(c, id)).await
}

/// Runs a schedule's prompt immediately in a new chat without changing its next due time.
/// Returns the new chat; the run streams through the background-run events.
#[tauri::command]
pub async fn run_schedule_now(
    app: AppHandle,
    state: State<'_, AppState>,
    id: ScheduleId,
) -> Result<ChatSummary, Error> {
    let schedule = state.db.call(move |c| repo::get_schedule(c, id)).await?;
    scheduler::fire(&app, &state, &schedule, ScheduleOutcome::Manual).await
}
