use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::db::{now_ms, sql_err};
use crate::domain::{AgentId, ChatId, ContentBlock, Message, Role, RunId, ScheduleId, Usage};
use crate::mcp::permissions::ChatToolOverrides;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MessageStatus {
    Complete,
    Streaming,
    /// The app exited while this message was still being generated.
    Interrupted,
    Cancelled,
    Failed,
}

impl MessageStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Streaming => "streaming",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "streaming" => Self::Streaming,
            "interrupted" => Self::Interrupted,
            "cancelled" => Self::Cancelled,
            "failed" => Self::Failed,
            _ => Self::Complete,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatSummary {
    pub id: ChatId,
    pub title: String,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    #[ts(type = "number")]
    pub message_count: i64,
    /// Set for chats with a user-configured agent; `agent_name` is its current name.
    pub agent_id: Option<AgentId>,
    pub agent_name: Option<String>,
    /// Set for chats created by a schedule run.
    pub schedule_id: Option<ScheduleId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StoredMessage {
    pub id: String,
    pub chat_id: ChatId,
    #[ts(type = "number")]
    pub seq: i64,
    pub role: Role,
    pub blocks: Vec<ContentBlock>,
    pub status: MessageStatus,
    pub model: Option<String>,
    pub run_id: Option<RunId>,
    pub usage: Option<Usage>,
    pub error: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
}

impl StoredMessage {
    pub fn to_message(&self) -> Message {
        Message {
            role: self.role,
            blocks: self.blocks.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatDetail {
    pub chat: ChatSummary,
    pub messages: Vec<StoredMessage>,
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}
fn parse_role(s: &str) -> Role {
    match s {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

pub(super) fn json<T: Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|e| Error::Internal(e.to_string()))
}
pub(super) fn parse_id<T: std::str::FromStr>(s: String, what: &str) -> Result<T> {
    s.parse()
        .map_err(|_| Error::Internal(format!("corrupt {what} id `{s}`")))
}

type SummaryRow = (
    String,
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn summary_from_row(r: &Row<'_>) -> rusqlite::Result<SummaryRow> {
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
    ))
}
fn to_summary(t: SummaryRow) -> Result<ChatSummary> {
    Ok(ChatSummary {
        id: parse_id(t.0, "chat")?,
        title: t.1,
        created_at: t.2,
        updated_at: t.3,
        provider_id: t.4,
        model: t.5,
        message_count: t.6,
        agent_id: t.7.map(|s| parse_id(s, "agent")).transpose()?,
        agent_name: t.8,
        schedule_id: t.9.map(|s| parse_id(s, "schedule")).transpose()?,
    })
}

const SUMMARY_SELECT: &str =
    "SELECT c.id, c.title, c.created_at, c.updated_at, c.provider_id, c.model,
    (SELECT COUNT(*) FROM messages m WHERE m.chat_id = c.id), c.agent_id, a.name, c.schedule_id
    FROM chats c LEFT JOIN agents a ON a.id = c.agent_id";

/// Marks messages left in `streaming` state by a previous process as interrupted.
pub(crate) fn recover_interrupted(conn: &Connection) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE messages SET status = 'interrupted', updated_at = ?1 WHERE status = 'streaming'",
            params![now_ms()],
        )
        .map_err(sql_err)?;
    if n > 0 {
        tracing::warn!(
            count = n,
            "marked unfinished messages from a previous session as interrupted"
        );
    }
    Ok(())
}

pub fn create_chat(conn: &Connection, title: &str) -> Result<ChatSummary> {
    create_chat_with(conn, title, None, None)
}

/// Creates a chat, optionally bound to an agent (and the schedule that started it).
pub fn create_chat_with(
    conn: &Connection,
    title: &str,
    agent_id: Option<AgentId>,
    schedule_id: Option<ScheduleId>,
) -> Result<ChatSummary> {
    if let Some(agent) = agent_id {
        super::agents::get_agent(conn, agent)?;
    }
    let id = ChatId::new();
    let now = now_ms();
    conn.execute(
        "INSERT INTO chats (id, title, created_at, updated_at, agent_id, schedule_id)
         VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
        params![
            id.to_string(),
            title,
            now,
            agent_id.map(|a| a.to_string()),
            schedule_id.map(|s| s.to_string())
        ],
    )
    .map_err(sql_err)?;
    get_summary(conn, id)
}

pub fn get_summary(conn: &Connection, id: ChatId) -> Result<ChatSummary> {
    conn.query_row(
        &format!("{SUMMARY_SELECT} WHERE c.id = ?1"),
        params![id.to_string()],
        summary_from_row,
    )
    .optional()
    .map_err(sql_err)?
    .ok_or_else(|| Error::NotFound(format!("chat {id}")))
    .and_then(to_summary)
}

pub fn list_chats(conn: &Connection, limit: usize) -> Result<Vec<ChatSummary>> {
    let mut stmt = conn
        .prepare_cached(&format!(
            "{SUMMARY_SELECT} ORDER BY c.updated_at DESC LIMIT ?1"
        ))
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![limit as i64], summary_from_row)
        .map_err(sql_err)?;
    rows.map(|r| r.map_err(sql_err).and_then(to_summary))
        .collect()
}

/// Case-insensitive substring search over titles and message text.
pub fn search_chats(conn: &Connection, query: &str, limit: usize) -> Result<Vec<ChatSummary>> {
    let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
    let mut stmt = conn
        .prepare_cached(&format!(
            "{SUMMARY_SELECT} WHERE c.title LIKE ?1 ESCAPE '\\' OR EXISTS (
                SELECT 1 FROM messages m WHERE m.chat_id = c.id AND m.blocks LIKE ?1 ESCAPE '\\')
             ORDER BY c.updated_at DESC LIMIT ?2"
        ))
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![pattern, limit as i64], summary_from_row)
        .map_err(sql_err)?;
    rows.map(|r| r.map_err(sql_err).and_then(to_summary))
        .collect()
}

pub fn get_chat(conn: &Connection, id: ChatId) -> Result<ChatDetail> {
    let chat = get_summary(conn, id)?;
    Ok(ChatDetail {
        chat,
        messages: list_messages(conn, id)?,
    })
}

pub fn list_messages(conn: &Connection, chat_id: ChatId) -> Result<Vec<StoredMessage>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, chat_id, seq, role, blocks, status, model, run_id, usage, error, created_at
             FROM messages WHERE chat_id = ?1 ORDER BY seq",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![chat_id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, i64>(10)?,
            ))
        })
        .map_err(sql_err)?;
    rows.map(|r| {
        let (id, chat_id, seq, role, blocks, status, model, run_id, usage, error, created_at) =
            r.map_err(sql_err)?;
        Ok(StoredMessage {
            id,
            chat_id: parse_id(chat_id, "chat")?,
            seq,
            role: parse_role(&role),
            blocks: serde_json::from_str(&blocks)
                .map_err(|e| Error::Internal(format!("corrupt message blocks: {e}")))?,
            status: MessageStatus::parse(&status),
            model,
            run_id: run_id.map(|s| parse_id(s, "run")).transpose()?,
            usage: usage
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(|e| Error::Internal(e.to_string()))?,
            error,
            created_at,
        })
    })
    .collect()
}

pub fn rename_chat(conn: &Connection, id: ChatId, title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return Err(Error::InvalidInput("title must be 1–120 characters".into()));
    }
    let n = conn
        .execute(
            "UPDATE chats SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![id.to_string(), title, now_ms()],
        )
        .map_err(sql_err)?;
    if n == 0 {
        return Err(Error::NotFound(format!("chat {id}")));
    }
    Ok(())
}

pub fn delete_chat(conn: &Connection, id: ChatId) -> Result<()> {
    let n = conn
        .execute("DELETE FROM chats WHERE id = ?1", params![id.to_string()])
        .map_err(sql_err)?;
    if n == 0 {
        return Err(Error::NotFound(format!("chat {id}")));
    }
    Ok(())
}

/// The chat's tool permission overrides (empty if none were set).
pub fn get_tool_overrides(conn: &Connection, id: ChatId) -> Result<ChatToolOverrides> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT tool_overrides FROM chats WHERE id = ?1",
            params![id.to_string()],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_err)?
        .ok_or_else(|| Error::NotFound(format!("chat {id}")))?;
    match raw {
        Some(s) => serde_json::from_str(&s)
            .map_err(|e| Error::Internal(format!("corrupt chat tool overrides: {e}"))),
        None => Ok(ChatToolOverrides::default()),
    }
}

pub fn set_tool_overrides(conn: &Connection, id: ChatId, o: &ChatToolOverrides) -> Result<()> {
    let value = if o.servers.is_empty() {
        None
    } else {
        Some(json(o)?)
    };
    let n = conn
        .execute(
            "UPDATE chats SET tool_overrides = ?2 WHERE id = ?1",
            params![id.to_string(), value],
        )
        .map_err(sql_err)?;
    if n == 0 {
        return Err(Error::NotFound(format!("chat {id}")));
    }
    Ok(())
}

/// Ids of all chats, or one agent's.
pub fn list_chat_ids(conn: &Connection, agent: Option<AgentId>) -> Result<Vec<ChatId>> {
    let mut stmt = conn
        .prepare_cached("SELECT id FROM chats WHERE ?1 IS NULL OR agent_id = ?1")
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![agent.map(|a| a.to_string())], |r| {
            r.get::<_, String>(0)
        })
        .map_err(sql_err)?;
    rows.map(|r| r.map_err(sql_err).and_then(|s| parse_id(s, "chat")))
        .collect()
}

fn next_seq(conn: &Connection, chat_id: ChatId) -> Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(seq), -1) + 1 FROM messages WHERE chat_id = ?1",
        params![chat_id.to_string()],
        |r| r.get(0),
    )
    .map_err(sql_err)
}

/// Appends a complete message (user turns, tool results). Returns its id.
pub fn append_message(
    conn: &mut Connection,
    chat_id: ChatId,
    role: Role,
    blocks: &[ContentBlock],
    attachment_ids: &[String],
) -> Result<String> {
    let tx = conn.transaction().map_err(sql_err)?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    let seq = next_seq(&tx, chat_id)?;
    tx.execute(
        "INSERT INTO messages (id, chat_id, seq, role, blocks, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'complete', ?6, ?6)",
        params![
            id,
            chat_id.to_string(),
            seq,
            role_str(role),
            json(&blocks)?,
            now
        ],
    )
    .map_err(sql_err)?;
    for att in attachment_ids {
        tx.execute(
            "INSERT OR IGNORE INTO message_attachments (message_id, attachment_id) VALUES (?1, ?2)",
            params![id, att],
        )
        .map_err(sql_err)?;
    }
    // Title from the first user message; bump recency.
    if seq == 0 && role == Role::User {
        if let Some(ContentBlock::Text { text }) = blocks
            .iter()
            .find(|b| matches!(b, ContentBlock::Text { .. }))
        {
            let title: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let title = if title.chars().count() > 60 {
                format!("{}…", title.chars().take(60).collect::<String>())
            } else {
                title
            };
            if !title.is_empty() {
                tx.execute(
                    "UPDATE chats SET title = ?2 WHERE id = ?1",
                    params![chat_id.to_string(), title],
                )
                .map_err(sql_err)?;
            }
        }
    }
    tx.execute(
        "UPDATE chats SET updated_at = ?2 WHERE id = ?1",
        params![chat_id.to_string(), now],
    )
    .map_err(sql_err)?;
    tx.commit().map_err(sql_err)?;
    Ok(id)
}

/// Creates the assistant placeholder a run streams into. Returns its id.
pub fn begin_assistant_message(
    conn: &mut Connection,
    chat_id: ChatId,
    run_id: RunId,
    provider_id: &str,
    model: &str,
) -> Result<String> {
    let tx = conn.transaction().map_err(sql_err)?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    let seq = next_seq(&tx, chat_id)?;
    tx.execute(
        "INSERT INTO messages (id, chat_id, seq, role, blocks, status, model, run_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'assistant', '[]', 'streaming', ?4, ?5, ?6, ?6)",
        params![id, chat_id.to_string(), seq, model, run_id.to_string(), now],
    )
    .map_err(sql_err)?;
    tx.execute(
        "UPDATE chats SET provider_id = ?2, model = ?3, updated_at = ?4 WHERE id = ?1",
        params![chat_id.to_string(), provider_id, model, now],
    )
    .map_err(sql_err)?;
    tx.commit().map_err(sql_err)?;
    Ok(id)
}

/// Periodic checkpoint of partial assistant content while streaming.
pub fn checkpoint_assistant_blocks(
    conn: &Connection,
    message_id: &str,
    blocks: &[ContentBlock],
) -> Result<()> {
    conn.execute(
        "UPDATE messages SET blocks = ?2, updated_at = ?3 WHERE id = ?1 AND status = 'streaming'",
        params![message_id, json(&blocks)?, now_ms()],
    )
    .map_err(sql_err)?;
    Ok(())
}

pub fn finalize_assistant_message(
    conn: &Connection,
    message_id: &str,
    blocks: &[ContentBlock],
    status: MessageStatus,
    usage: Option<Usage>,
    error: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE messages SET blocks = ?2, status = ?3, usage = ?4, error = ?5, updated_at = ?6 WHERE id = ?1",
        params![message_id, json(&blocks)?, status.as_str(), usage.map(|u| json(&u)).transpose()?, error, now_ms()],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Records the model that actually produced a message (after a fallback switch).
pub fn set_message_model(conn: &Connection, message_id: &str, model: &str) -> Result<()> {
    conn.execute(
        "UPDATE messages SET model = ?2 WHERE id = ?1",
        params![message_id, model],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Removes a message and everything after it (used by retry/regenerate).
pub fn truncate_from(conn: &Connection, chat_id: ChatId, message_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM messages WHERE chat_id = ?1 AND seq >= (SELECT seq FROM messages WHERE id = ?2 AND chat_id = ?1)",
        params![chat_id.to_string(), message_id],
    )
    .map_err(sql_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        super::super::migrations::apply(&mut c).unwrap();
        c
    }

    #[test]
    fn tool_overrides_round_trip() {
        let c = conn();
        let chat = create_chat(&c, "x").unwrap();
        assert_eq!(get_tool_overrides(&c, chat.id).unwrap(), Default::default());
        let mut o = ChatToolOverrides::default();
        o.set_server_off("fs", true);
        o.set_tool("web", "fetch", Some(crate::domain::ToolPolicy::AlwaysAllow));
        set_tool_overrides(&c, chat.id, &o).unwrap();
        assert_eq!(get_tool_overrides(&c, chat.id).unwrap(), o);
        set_tool_overrides(&c, chat.id, &ChatToolOverrides::default()).unwrap();
        assert!(get_tool_overrides(&c, chat.id).unwrap().servers.is_empty());
        assert_eq!(list_chat_ids(&c, None).unwrap(), vec![chat.id]);
        assert!(get_tool_overrides(&c, ChatId::new()).is_err());
    }

    #[test]
    fn chat_lifecycle_and_title_from_first_prompt() {
        let mut c = conn();
        let chat = create_chat(&c, "New chat").unwrap();
        let text = "  Explain   WAL mode in SQLite please  ";
        append_message(
            &mut c,
            chat.id,
            Role::User,
            &[ContentBlock::Text { text: text.into() }],
            &[],
        )
        .unwrap();
        let detail = get_chat(&c, chat.id).unwrap();
        assert_eq!(detail.chat.title, "Explain WAL mode in SQLite please");
        assert_eq!(detail.messages.len(), 1);
        assert_eq!(detail.chat.message_count, 1);

        let run = RunId::new();
        let mid = begin_assistant_message(&mut c, chat.id, run, "prov", "model-x").unwrap();
        checkpoint_assistant_blocks(
            &c,
            &mid,
            &[ContentBlock::Text {
                text: "partial".into(),
            }],
        )
        .unwrap();
        finalize_assistant_message(
            &c,
            &mid,
            &[
                ContentBlock::Thinking {
                    text: "thinking it through".into(),
                },
                ContentBlock::Text {
                    text: "partial done".into(),
                },
            ],
            MessageStatus::Complete,
            Some(Usage::default()),
            None,
        )
        .unwrap();
        let detail = get_chat(&c, chat.id).unwrap();
        let last = detail.messages.last().unwrap();
        assert_eq!(last.status, MessageStatus::Complete);
        assert_eq!(
            last.blocks,
            vec![
                ContentBlock::Thinking {
                    text: "thinking it through".into()
                },
                ContentBlock::Text {
                    text: "partial done".into()
                }
            ]
        );
        assert_eq!(last.run_id, Some(run));
        assert_eq!(detail.chat.model.as_deref(), Some("model-x"));

        assert_eq!(search_chats(&c, "wal mode", 10).unwrap().len(), 1);
        assert_eq!(search_chats(&c, "partial done", 10).unwrap().len(), 1);
        assert_eq!(search_chats(&c, "zzz", 10).unwrap().len(), 0);

        rename_chat(&c, chat.id, "Renamed").unwrap();
        assert_eq!(list_chats(&c, 10).unwrap()[0].title, "Renamed");
        delete_chat(&c, chat.id).unwrap();
        assert!(get_chat(&c, chat.id).is_err());
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "cascade delete");
    }

    #[test]
    fn recovery_marks_streaming_as_interrupted() {
        let mut c = conn();
        let chat = create_chat(&c, "x").unwrap();
        let mid = begin_assistant_message(&mut c, chat.id, RunId::new(), "p", "m").unwrap();
        checkpoint_assistant_blocks(
            &c,
            &mid,
            &[ContentBlock::Text {
                text: "half".into(),
            }],
        )
        .unwrap();
        recover_interrupted(&c).unwrap();
        let m = &get_chat(&c, chat.id).unwrap().messages[0];
        assert_eq!(m.status, MessageStatus::Interrupted);
        assert_eq!(
            m.blocks,
            vec![ContentBlock::Text {
                text: "half".into()
            }]
        );
    }

    #[test]
    fn truncate_from_removes_tail() {
        let mut c = conn();
        let chat = create_chat(&c, "x").unwrap();
        let a = append_message(
            &mut c,
            chat.id,
            Role::User,
            &[ContentBlock::Text { text: "a".into() }],
            &[],
        )
        .unwrap();
        let b = begin_assistant_message(&mut c, chat.id, RunId::new(), "p", "m").unwrap();
        append_message(
            &mut c,
            chat.id,
            Role::User,
            &[ContentBlock::Text { text: "c".into() }],
            &[],
        )
        .unwrap();
        truncate_from(&c, chat.id, &b).unwrap();
        let msgs = get_chat(&c, chat.id).unwrap().messages;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, a);
    }
}
