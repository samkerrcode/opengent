//! Background driver for agent schedules. The decisions (is it due, was it missed, what's
//! next) live in `agent_core::agents::schedule`; this module ticks, persists the decision,
//! and starts runs whose events go to the UI as app events instead of a per-call channel.
//!
//! Sleep and app downtime are detected with wall-clock time: timers may pause while the
//! machine sleeps, but each tick compares due times against the real clock, so anything
//! that came due while away is handled per the schedule's missed-run policy.

use std::time::{Duration, Instant};

use agent_core::agent::RunSink;
use agent_core::agents::{plan_due, AgentSchedule, ScheduleOutcome};
use agent_core::domain::RunEvent;
use agent_core::storage::agents as repo;
use agent_core::storage::chats as chats_repo;
use agent_core::storage::ChatSummary;
use agent_core::Error;
use chrono::Local;
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::{launch_run, RunParams};
use crate::state::{AGENTS_CHANGED_EVENT, BACKGROUND_RUN_EVENT, BACKGROUND_RUN_STARTED_EVENT};
use crate::AppState;

/// Schedules are checked this often; the missed-run grace window (2 min) is well above it.
const TICK: Duration = Duration::from_secs(20);
/// At startup, wait up to this long for MCP servers to connect so catch-up runs get tools.
const MCP_SETTLE_MAX: Duration = Duration::from_secs(30);

/// Forwards a background run's events to every webview. Always reports success: nobody may
/// be watching, and that must not cancel the run (approvals still time out on their own).
struct AppEventSink(AppHandle);

impl RunSink for AppEventSink {
    fn send(&self, event: RunEvent) -> bool {
        let _ = self.0.emit(BACKGROUND_RUN_EVENT, event);
        true
    }
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let settle = Instant::now();
        while settle.elapsed() < MCP_SETTLE_MAX
            && app.state::<AppState>().mcp.pending_connections() > 0
        {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        loop {
            if let Err(e) = tick(&app).await {
                tracing::warn!(error = %e, "schedule tick failed");
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

async fn tick(app: &AppHandle) -> Result<(), Error> {
    let state = app.state::<AppState>();
    let now = chrono::Utc::now().timestamp_millis();
    let due = state.db.call(move |c| repo::due_schedules(c, now)).await?;
    if due.is_empty() {
        return Ok(());
    }
    for schedule in due {
        let Some(plan) = plan_due(&schedule, now, &Local) else {
            continue;
        };
        let outcome = plan.outcome();
        // Persist the decision before running so a crash mid-run can't fire it twice.
        let (id, next, o, run) = (schedule.id, plan.next_run_at, outcome.clone(), plan.run);
        state
            .db
            .call(move |c| repo::advance_schedule(c, id, next, &o, run))
            .await?;
        if plan.run {
            tracing::info!(schedule = %schedule.id, missed = plan.missed, "starting scheduled agent run");
            if let Err(e) = fire(app, &state, &schedule, outcome).await {
                tracing::warn!(schedule = %schedule.id, error = %e, "scheduled run failed to start");
            }
        } else {
            tracing::info!(schedule = %schedule.id, "skipped a missed scheduled run");
        }
    }
    let _ = app.emit(AGENTS_CHANGED_EVENT, ());
    Ok(())
}

/// Starts one run of `schedule` in a new agent chat and records the result on the schedule.
pub async fn fire(
    app: &AppHandle,
    state: &AppState,
    schedule: &AgentSchedule,
    outcome: ScheduleOutcome,
) -> Result<ChatSummary, Error> {
    let (agent_id, schedule_id) = (schedule.agent_id, schedule.id);
    let chat = state
        .db
        .call(move |c| {
            chats_repo::create_chat_with(c, "Scheduled run", Some(agent_id), Some(schedule_id))
        })
        .await?;
    let announce = app.clone();
    let result = launch_run(
        state,
        RunParams {
            chat_id: chat.id,
            selection: None,
            prompt: schedule.prompt.clone(),
            attachment_ids: Vec::new(),
            replace_from: None,
            unattended: true,
        },
        AppEventSink(app.clone()),
        move |started| {
            let _ = announce.emit(BACKGROUND_RUN_STARTED_EVENT, started.clone());
        },
    )
    .await;
    let chat_id = chat.id;
    let (recorded, chat_ref) = match &result {
        Ok(started) => (outcome, Some(started.chat.id)),
        Err(e) => {
            // Nothing ran: drop the empty chat rather than leave a blank entry behind.
            state
                .db
                .spawn_job(move |c| chats_repo::delete_chat(c, chat_id));
            (
                ScheduleOutcome::Failed {
                    message: e.to_string(),
                },
                None,
            )
        }
    };
    state
        .db
        .call(move |c| repo::set_schedule_result(c, schedule_id, &recorded, chat_ref))
        .await?;
    let _ = app.emit(AGENTS_CHANGED_EVENT, ());
    result.map(|started| started.chat)
}
