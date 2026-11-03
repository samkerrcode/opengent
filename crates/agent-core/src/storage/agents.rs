//! Persistence for user-configured agents and their schedules.

use chrono::TimeZone;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::de::DeserializeOwned;

use super::chats::{json, parse_id};
use super::db::{now_ms, sql_err};
use crate::agents::schedule::next_after;
use crate::agents::{
    Agent, AgentConfigJson, AgentInput, AgentSchedule, ScheduleInput, ScheduleOutcome,
};
use crate::domain::{AgentId, ChatId, ScheduleId};
use crate::{Error, Result};

fn from_json<T: DeserializeOwned>(s: &str, what: &str) -> Result<T> {
    serde_json::from_str(s).map_err(|e| Error::Internal(format!("corrupt {what}: {e}")))
}

type AgentRow = (String, String, String, i64, i64);

fn agent_row(r: &Row<'_>) -> rusqlite::Result<AgentRow> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn to_agent((id, name, config, created_at, updated_at): AgentRow) -> Result<Agent> {
    Ok(Agent::from_parts(
        parse_id(id, "agent")?,
        name,
        from_json::<AgentConfigJson>(&config, "agent config")?,
        created_at,
        updated_at,
    ))
}

const AGENT_SELECT: &str = "SELECT id, name, config, created_at, updated_at FROM agents";

pub fn list_agents(conn: &Connection) -> Result<Vec<Agent>> {
    let mut stmt = conn
        .prepare_cached(&format!("{AGENT_SELECT} ORDER BY name COLLATE NOCASE"))
        .map_err(sql_err)?;
    let rows = stmt.query_map([], agent_row).map_err(sql_err)?;
    rows.map(|r| r.map_err(sql_err).and_then(to_agent))
        .collect()
}

pub fn get_agent(conn: &Connection, id: AgentId) -> Result<Agent> {
    conn.query_row(
        &format!("{AGENT_SELECT} WHERE id = ?1"),
        params![id.to_string()],
        agent_row,
    )
    .optional()
    .map_err(sql_err)?
    .ok_or_else(|| Error::NotFound(format!("agent {id}")))
    .and_then(to_agent)
}

/// Validated upsert. An `id` that doesn't exist is an error (the agent was deleted).
pub fn save_agent(conn: &Connection, input: AgentInput) -> Result<Agent> {
    let (id, name, config) = input.validate()?;
    let now = now_ms();
    let config_json = json(&config)?;
    let id = match id {
        Some(id) => {
            let n = conn
                .execute(
                    "UPDATE agents SET name = ?2, config = ?3, updated_at = ?4 WHERE id = ?1",
                    params![id.to_string(), name, config_json, now],
                )
                .map_err(sql_err)?;
            if n == 0 {
                return Err(Error::NotFound(format!("agent {id}")));
            }
            id
        }
        None => {
            let id = AgentId::new();
            conn.execute(
                "INSERT INTO agents (id, name, config, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![id.to_string(), name, config_json, now],
            )
            .map_err(sql_err)?;
            id
        }
    };
    get_agent(conn, id)
}

/// Replaces only the agent's server access / tool permissions (Tools menu edits and
/// approval-card grants), keeping everything else.
pub fn set_agent_servers(
    conn: &Connection,
    id: AgentId,
    servers: Vec<crate::agents::AgentServerAccess>,
) -> Result<Agent> {
    let a = get_agent(conn, id)?;
    save_agent(
        conn,
        AgentInput {
            id: Some(id),
            name: a.name,
            description: a.description,
            default_model: a.default_model,
            fallback_model: a.fallback_model,
            system_prompt: a.system_prompt,
            servers,
        },
    )
}

/// Deletes the agent and its schedules; its chats remain as plain chats.
pub fn delete_agent(conn: &Connection, id: AgentId) -> Result<()> {
    let n = conn
        .execute("DELETE FROM agents WHERE id = ?1", params![id.to_string()])
        .map_err(sql_err)?;
    if n == 0 {
        return Err(Error::NotFound(format!("agent {id}")));
    }
    Ok(())
}

type ScheduleRow = (
    String,
    String,
    String,
    String,
    String,
    bool,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
    i64,
    i64,
);

fn schedule_row(r: &Row<'_>) -> rusqlite::Result<ScheduleRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
    ))
}

fn to_schedule(t: ScheduleRow) -> Result<AgentSchedule> {
    Ok(AgentSchedule {
        id: parse_id(t.0, "schedule")?,
        agent_id: parse_id(t.1, "agent")?,
        prompt: t.2,
        recurrence: from_json(&t.3, "schedule recurrence")?,
        missed_policy: from_json(&t.4, "schedule policy")?,
        enabled: t.5,
        next_run_at: t.6,
        last_run_at: t.7,
        // An unreadable outcome is display-only metadata; don't fail the whole list.
        last_outcome: t.8.and_then(|s| serde_json::from_str(&s).ok()),
        last_chat_id: t.9.and_then(|s| s.parse().ok()),
        created_at: t.10,
        updated_at: t.11,
    })
}

const SCHEDULE_SELECT: &str = "SELECT id, agent_id, prompt, recurrence, missed_policy, enabled,
    next_run_at, last_run_at, last_outcome, last_chat_id, created_at, updated_at
    FROM agent_schedules";

/// All schedules, or one agent's, oldest first.
pub fn list_schedules(conn: &Connection, agent: Option<AgentId>) -> Result<Vec<AgentSchedule>> {
    let mut stmt = conn
        .prepare_cached(&format!(
            "{SCHEDULE_SELECT} WHERE ?1 IS NULL OR agent_id = ?1 ORDER BY created_at"
        ))
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![agent.map(|a| a.to_string())], schedule_row)
        .map_err(sql_err)?;
    rows.map(|r| r.map_err(sql_err).and_then(to_schedule))
        .collect()
}

pub fn get_schedule(conn: &Connection, id: ScheduleId) -> Result<AgentSchedule> {
    conn.query_row(
        &format!("{SCHEDULE_SELECT} WHERE id = ?1"),
        params![id.to_string()],
        schedule_row,
    )
    .optional()
    .map_err(sql_err)?
    .ok_or_else(|| Error::NotFound(format!("schedule {id}")))
    .and_then(to_schedule)
}

/// Enabled schedules whose next due time is at or before `now`.
pub fn due_schedules(conn: &Connection, now: i64) -> Result<Vec<AgentSchedule>> {
    let mut stmt = conn
        .prepare_cached(&format!(
            "{SCHEDULE_SELECT} WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1
             ORDER BY next_run_at"
        ))
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![now], schedule_row)
        .map_err(sql_err)?;
    rows.map(|r| r.map_err(sql_err).and_then(to_schedule))
        .collect()
}

/// First due time for a newly enabled schedule. A one-off time in the past is an error
/// rather than an instant "missed" run.
fn first_due<Tz: TimeZone>(input: &ScheduleInput, now: i64, tz: &Tz) -> Result<Option<i64>> {
    if !input.enabled {
        return Ok(None);
    }
    match next_after(&input.recurrence, now, tz) {
        Some(t) => Ok(Some(t)),
        None => Err(Error::InvalidInput(
            "the scheduled time is in the past".into(),
        )),
    }
}

/// Validated upsert. Editing only the prompt or missed-run policy keeps the pending due
/// time; changing the recurrence or re-enabling recomputes it from `now` (time spent
/// disabled never counts as missed).
pub fn save_schedule<Tz: TimeZone>(
    conn: &Connection,
    input: ScheduleInput,
    now: i64,
    tz: &Tz,
) -> Result<AgentSchedule> {
    let prompt = input.validate()?;
    get_agent(conn, input.agent_id)?;
    let existing = input.id.map(|id| get_schedule(conn, id)).transpose()?;
    if existing
        .as_ref()
        .is_some_and(|e| e.agent_id != input.agent_id)
    {
        return Err(Error::InvalidInput(
            "a schedule cannot move to another agent".into(),
        ));
    }
    let next_run_at = match &existing {
        Some(e)
            if e.enabled
                && input.enabled
                && e.recurrence == input.recurrence
                && e.next_run_at.is_some() =>
        {
            e.next_run_at
        }
        _ => first_due(&input, now, tz)?,
    };
    let (recurrence, policy) = (json(&input.recurrence)?, json(&input.missed_policy)?);
    let id = match existing {
        Some(e) => {
            conn.execute(
                "UPDATE agent_schedules SET prompt = ?2, recurrence = ?3, missed_policy = ?4,
                 enabled = ?5, next_run_at = ?6, updated_at = ?7 WHERE id = ?1",
                params![
                    e.id.to_string(),
                    prompt,
                    recurrence,
                    policy,
                    input.enabled,
                    next_run_at,
                    now
                ],
            )
            .map_err(sql_err)?;
            e.id
        }
        None => {
            let id = ScheduleId::new();
            conn.execute(
                "INSERT INTO agent_schedules (id, agent_id, prompt, recurrence, missed_policy,
                 enabled, next_run_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    id.to_string(),
                    input.agent_id.to_string(),
                    prompt,
                    recurrence,
                    policy,
                    input.enabled,
                    next_run_at,
                    now
                ],
            )
            .map_err(sql_err)?;
            id
        }
    };
    get_schedule(conn, id)
}

pub fn set_schedule_enabled<Tz: TimeZone>(
    conn: &Connection,
    id: ScheduleId,
    enabled: bool,
    now: i64,
    tz: &Tz,
) -> Result<AgentSchedule> {
    let s = get_schedule(conn, id)?;
    save_schedule(
        conn,
        ScheduleInput {
            id: Some(id),
            agent_id: s.agent_id,
            prompt: s.prompt,
            recurrence: s.recurrence,
            missed_policy: s.missed_policy,
            enabled,
        },
        now,
        tz,
    )
}

pub fn delete_schedule(conn: &Connection, id: ScheduleId) -> Result<()> {
    let n = conn
        .execute(
            "DELETE FROM agent_schedules WHERE id = ?1",
            params![id.to_string()],
        )
        .map_err(sql_err)?;
    if n == 0 {
        return Err(Error::NotFound(format!("schedule {id}")));
    }
    Ok(())
}

/// Moves a due schedule forward *before* its run starts, so a crash mid-run can't fire it
/// twice. `next_run_at = None` finishes (and disables) the schedule.
pub fn advance_schedule(
    conn: &Connection,
    id: ScheduleId,
    next_run_at: Option<i64>,
    outcome: &ScheduleOutcome,
    ran: bool,
) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "UPDATE agent_schedules SET next_run_at = ?2, enabled = enabled AND ?3,
         last_outcome = ?4, last_run_at = CASE WHEN ?5 THEN ?6 ELSE last_run_at END,
         updated_at = ?6 WHERE id = ?1",
        params![
            id.to_string(),
            next_run_at,
            next_run_at.is_some(),
            json(outcome)?,
            ran,
            now
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Records the result of starting a run (its chat, or why it failed to start).
pub fn set_schedule_result(
    conn: &Connection,
    id: ScheduleId,
    outcome: &ScheduleOutcome,
    chat: Option<ChatId>,
) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "UPDATE agent_schedules SET last_outcome = ?2, last_run_at = ?3,
         last_chat_id = COALESCE(?4, last_chat_id), updated_at = ?3 WHERE id = ?1",
        params![
            id.to_string(),
            json(outcome)?,
            now,
            chat.map(|c| c.to_string())
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentServerAccess, MissedRunPolicy, ModelRef, Recurrence};
    use crate::storage::chats::{create_chat_with, get_summary};
    use chrono::Utc;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        super::super::migrations::apply(&mut c).unwrap();
        c
    }

    fn agent_input(name: &str) -> AgentInput {
        AgentInput {
            id: None,
            name: name.into(),
            description: "d".into(),
            default_model: ModelRef {
                provider_id: "simulated".into(),
                model: "simulated-1".into(),
            },
            fallback_model: None,
            system_prompt: "Be brief.".into(),
            servers: vec![AgentServerAccess {
                server_id: "fs".into(),
                tools: None,
                default_policy: None,
                policies: Default::default(),
            }],
        }
    }

    fn schedule_input(agent_id: AgentId, rec: Recurrence) -> ScheduleInput {
        ScheduleInput {
            id: None,
            agent_id,
            prompt: " Daily digest ".into(),
            recurrence: rec,
            missed_policy: MissedRunPolicy::Skip,
            enabled: true,
        }
    }

    #[test]
    fn agent_crud_and_chat_link() {
        let c = conn();
        let a = save_agent(&c, agent_input("Writer")).unwrap();
        assert_eq!(a.system_prompt, "Be brief.");
        let mut edit = agent_input("Editor");
        edit.id = Some(a.id);
        let a2 = save_agent(&c, edit).unwrap();
        assert_eq!(a2.id, a.id);
        assert_eq!(list_agents(&c).unwrap().len(), 1);

        let chat = create_chat_with(&c, "New chat", Some(a.id), None).unwrap();
        assert_eq!(chat.agent_name.as_deref(), Some("Editor"));
        assert!(create_chat_with(&c, "x", Some(AgentId::new()), None).is_err());

        delete_agent(&c, a.id).unwrap();
        let chat = get_summary(&c, chat.id).unwrap();
        assert_eq!(chat.agent_id, None, "chats outlive their agent");
        let mut ghost = agent_input("Ghost");
        ghost.id = Some(a.id);
        assert!(matches!(save_agent(&c, ghost), Err(Error::NotFound(_))));
    }

    #[test]
    fn schedule_lifecycle() {
        let c = conn();
        let a = save_agent(&c, agent_input("Digest")).unwrap();
        let now = 1_000_000;
        let s = save_schedule(
            &c,
            schedule_input(a.id, Recurrence::Interval { minutes: 10 }),
            now,
            &Utc,
        )
        .unwrap();
        assert_eq!(s.prompt, "Daily digest");
        assert_eq!(s.next_run_at, Some(now + 600_000));

        // Editing the prompt keeps the pending due time.
        let mut edit = schedule_input(a.id, Recurrence::Interval { minutes: 10 });
        edit.id = Some(s.id);
        edit.prompt = "Changed".into();
        let s2 = save_schedule(&c, edit, now + 5_000, &Utc).unwrap();
        assert_eq!(s2.next_run_at, s.next_run_at);

        // Disabling clears it; re-enabling recomputes from now.
        let off = set_schedule_enabled(&c, s.id, false, now, &Utc).unwrap();
        assert_eq!(off.next_run_at, None);
        let on = set_schedule_enabled(&c, s.id, true, now + 60_000, &Utc).unwrap();
        assert_eq!(on.next_run_at, Some(now + 60_000 + 600_000));

        assert!(due_schedules(&c, now).unwrap().is_empty());
        assert_eq!(due_schedules(&c, now + 700_000).unwrap().len(), 1);

        advance_schedule(&c, s.id, Some(now + 2_000_000), &ScheduleOutcome::Ran, true).unwrap();
        let chat = create_chat_with(&c, "run", Some(a.id), Some(s.id)).unwrap();
        set_schedule_result(&c, s.id, &ScheduleOutcome::Ran, Some(chat.id)).unwrap();
        let s3 = get_schedule(&c, s.id).unwrap();
        assert_eq!(s3.last_outcome, Some(ScheduleOutcome::Ran));
        assert_eq!(s3.last_chat_id, Some(chat.id));
        assert!(s3.last_run_at.is_some());
        assert_eq!(chat.schedule_id, Some(s.id));

        // Finishing (no next run) disables.
        advance_schedule(&c, s.id, None, &ScheduleOutcome::Ran, true).unwrap();
        assert!(!get_schedule(&c, s.id).unwrap().enabled);

        delete_agent(&c, a.id).unwrap();
        assert!(list_schedules(&c, None).unwrap().is_empty(), "cascade");
    }

    #[test]
    fn one_off_in_the_past_is_rejected() {
        let c = conn();
        let a = save_agent(&c, agent_input("Once")).unwrap();
        let err = save_schedule(
            &c,
            schedule_input(a.id, Recurrence::Once { at: 10 }),
            1_000,
            &Utc,
        );
        assert!(matches!(err, Err(Error::InvalidInput(_))));
    }
}
