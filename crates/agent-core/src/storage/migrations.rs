use rusqlite::Connection;

use super::db::sql_err;
use crate::config::product;
use crate::{Error, Result};

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
    // v4: PDF attachments — pages in the document, and one image blob per rendered page.
    "ALTER TABLE attachments ADD COLUMN page_count INTEGER;
    CREATE TABLE attachment_pages (
        attachment_id TEXT NOT NULL REFERENCES attachments(id),
        page_index    INTEGER NOT NULL,
        sha256        TEXT NOT NULL,
        mime          TEXT NOT NULL,
        PRIMARY KEY (attachment_id, page_index)
    );",
    // v5: renderer output version (`pdf::RENDER_VERSION`), a page-1 thumbnail blob, and
    // page rows that cascade with their attachment (SQLite can't ALTER in a foreign key,
    // so the table is rebuilt).
    "ALTER TABLE attachments ADD COLUMN render_version INTEGER;
    ALTER TABLE attachments ADD COLUMN thumbnail_sha256 TEXT;
    ALTER TABLE attachments ADD COLUMN thumbnail_mime TEXT;
    CREATE TABLE attachment_pages_cascade (
        attachment_id TEXT NOT NULL REFERENCES attachments(id) ON DELETE CASCADE,
        page_index    INTEGER NOT NULL,
        sha256        TEXT NOT NULL,
        mime          TEXT NOT NULL,
        PRIMARY KEY (attachment_id, page_index)
    );
    INSERT INTO attachment_pages_cascade SELECT * FROM attachment_pages;
    DROP TABLE attachment_pages;
    ALTER TABLE attachment_pages_cascade RENAME TO attachment_pages;",
];

/// The schema version this build creates and understands.
pub fn latest() -> i64 {
    MIGRATIONS.len() as i64
}

fn version(conn: &Connection) -> Result<i64> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(sql_err)
}

fn newer_schema(found: i64) -> Error {
    Error::Io(format!(
        "the database was created by a newer version of {} (schema {found}; this build knows \
         {}) — update this app",
        product().display_name,
        latest()
    ))
}

/// Upgrades the schema to `latest()`. A database from a newer build is refused rather than
/// used: this build can't know what the newer schema means.
pub fn apply(conn: &mut Connection) -> Result<()> {
    let current = version(conn)?;
    if current > latest() {
        return Err(newer_schema(current));
    }
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

/// For a process that shares the database with another one: the schema must already be
/// exactly this build's, since only a process that is alone may migrate it.
pub fn require_current(conn: &Connection) -> Result<()> {
    match version(conn)? {
        v if v == latest() => Ok(()),
        v if v > latest() => Err(newer_schema(v)),
        v => Err(Error::Io(format!(
            "the database is in use by an older version of {} (schema {v}; this build needs \
             {}) — close it and try again",
            product().display_name,
            latest()
        ))),
    }
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

    #[test]
    fn a_newer_or_older_schema_is_refused_where_it_must_be() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        require_current(&conn).unwrap();
        conn.pragma_update(None, "user_version", latest() + 1)
            .unwrap();
        let e = apply(&mut conn).unwrap_err().to_string();
        assert!(e.contains("newer version"), "{e}");
        assert!(require_current(&conn)
            .unwrap_err()
            .to_string()
            .contains("newer version"));
        conn.pragma_update(None, "user_version", latest() - 1)
            .unwrap();
        let e = require_current(&conn).unwrap_err().to_string();
        assert!(e.contains("older version"), "{e}");
    }

    /// A database that stopped at v4 (built by a pre-v5 dev build) upgrades cleanly:
    /// the new columns appear, page rows survive the table rebuild, and they cascade.
    #[test]
    fn v5_upgrades_a_database_stopped_at_v4() {
        let mut conn = Connection::open_in_memory().unwrap();
        for (i, sql) in MIGRATIONS.iter().take(4).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", i as i64 + 1)
                .unwrap();
        }
        conn.execute(
            "INSERT INTO attachments
                 (id, sha256, name, mime, size_bytes, char_count, page_count, created_at)
             VALUES ('a', 'h', 'n.pdf', 'application/pdf', 1, 0, 2, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attachment_pages (attachment_id, page_index, sha256, mime)
             VALUES ('a', 0, 'p', 'image/png')",
            [],
        )
        .unwrap();

        apply(&mut conn).unwrap();
        let row: (Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT render_version, thumbnail_sha256 FROM attachments WHERE id = 'a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (None, None));
        let pages: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attachment_pages WHERE attachment_id = 'a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pages, 1, "page rows survive the rebuild");

        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute("DELETE FROM attachments WHERE id = 'a'", [])
            .unwrap();
        let pages: i64 = conn
            .query_row("SELECT COUNT(*) FROM attachment_pages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pages, 0, "pages cascade");
    }
}
