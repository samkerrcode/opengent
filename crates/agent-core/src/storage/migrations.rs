use rusqlite::Connection;

use super::db::sql_err;
use crate::Result;

/// Append-only. Each entry runs inside a transaction and bumps `PRAGMA user_version`.
const MIGRATIONS: &[&str] = &[
    // v1: chats, messages, attachments
    "CREATE TABLE chats (
        id          TEXT PRIMARY KEY,
        title       TEXT NOT NULL,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL,
        provider_id TEXT,
        model       TEXT
    );
    CREATE INDEX chats_updated ON chats(updated_at DESC);

    CREATE TABLE messages (
        id          TEXT PRIMARY KEY,
        chat_id     TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
        seq         INTEGER NOT NULL,
        role        TEXT NOT NULL,
        blocks      TEXT NOT NULL,
        status      TEXT NOT NULL,
        model       TEXT,
        run_id      TEXT,
        usage       TEXT,
        error       TEXT,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL,
        UNIQUE(chat_id, seq)
    );
    CREATE INDEX messages_chat ON messages(chat_id, seq);

    CREATE TABLE attachments (
        id          TEXT PRIMARY KEY,
        sha256      TEXT NOT NULL UNIQUE,
        name        TEXT NOT NULL,
        mime        TEXT NOT NULL,
        size_bytes  INTEGER NOT NULL,
        char_count  INTEGER NOT NULL,
        created_at  INTEGER NOT NULL
    );

    CREATE TABLE message_attachments (
        message_id    TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
        attachment_id TEXT NOT NULL REFERENCES attachments(id),
        PRIMARY KEY (message_id, attachment_id)
    );",
    // v2: user-configured agents, their schedules, and agent/schedule links on chats.
    // Deleting an agent removes its schedules but keeps its chats (as plain chats).
    "CREATE TABLE agents (
        id          TEXT PRIMARY KEY,
        name        TEXT NOT NULL,
        config      TEXT NOT NULL,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL
    );

    CREATE TABLE agent_schedules (
        id            TEXT PRIMARY KEY,
        agent_id      TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
        prompt        TEXT NOT NULL,
        recurrence    TEXT NOT NULL,
        missed_policy TEXT NOT NULL,
        enabled       INTEGER NOT NULL,
        next_run_at   INTEGER,
        last_run_at   INTEGER,
        last_outcome  TEXT,
        last_chat_id  TEXT,
        created_at    INTEGER NOT NULL,
        updated_at    INTEGER NOT NULL
    );
    CREATE INDEX agent_schedules_due ON agent_schedules(enabled, next_run_at);
    CREATE INDEX agent_schedules_agent ON agent_schedules(agent_id);

    ALTER TABLE chats ADD COLUMN agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL;
    ALTER TABLE chats ADD COLUMN schedule_id TEXT;
    CREATE INDEX chats_agent ON chats(agent_id);",
    // v3: per-chat tool permission overrides (JSON `ChatToolOverrides`; NULL = none).
    "ALTER TABLE chats ADD COLUMN tool_overrides TEXT;",
];

pub fn apply(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(sql_err)?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction().map_err(sql_err)?;
        tx.execute_batch(sql).map_err(sql_err)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)
            .map_err(sql_err)?;
        tx.commit().map_err(sql_err)?;
        tracing::info!(version = i + 1, "applied database migration");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        apply(&mut conn).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
    }
}
