//! Immutable, content-addressed attachment snapshots. Bytes live at
//! `<data_root>/attachments/<sha256>`; metadata lives in SQLite. Only bounded text-like files
//! are accepted in this phase; images/PDFs come with capable providers and explicit parsers.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use super::db::{now_ms, sql_err};
use crate::{Error, Result};

pub const MAX_ATTACHMENT_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AttachmentMeta {
    pub id: String,
    pub sha256: String,
    pub name: String,
    pub mime: String,
    #[ts(type = "number")]
    pub size_bytes: i64,
    #[ts(type = "number")]
    pub char_count: i64,
}

pub struct AttachmentStore {
    dir: PathBuf,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn guess_mime(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("md" | "markdown") => "text/markdown",
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        Some("yaml" | "yml") => "application/yaml",
        Some("toml") => "application/toml",
        Some("xml") => "application/xml",
        Some("html" | "htm") => "text/html",
        Some(
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "c" | "h" | "cpp" | "hpp"
            | "cs" | "rb" | "php" | "swift" | "kt" | "sh" | "sql" | "css" | "svelte" | "vue",
        ) => "text/x-source",
        _ => "text/plain",
    }
}

impl AttachmentStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn blob_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(sha256)
    }

    /// Reads a user-selected file, validates it is bounded UTF-8 text, stores an immutable copy
    /// and records metadata. Re-importing identical content returns the existing record.
    pub fn import_file(&self, conn: &Connection, path: &Path) -> Result<AttachmentMeta> {
        let meta = std::fs::metadata(path)?;
        if !meta.is_file() {
            return Err(Error::InvalidInput(
                "only regular files can be attached".into(),
            ));
        }
        if meta.len() > MAX_ATTACHMENT_BYTES {
            return Err(Error::InvalidInput(format!(
                "file is larger than {} MiB",
                MAX_ATTACHMENT_BYTES / 1024 / 1024
            )));
        }
        let bytes = std::fs::read(path)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            Error::InvalidInput("only UTF-8 text files are supported in this version".into())
        })?;
        if text.contains('\0') {
            return Err(Error::InvalidInput("file appears to be binary".into()));
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .to_owned();
        self.import_text(conn, &name, text)
    }

    pub fn import_text(&self, conn: &Connection, name: &str, text: &str) -> Result<AttachmentMeta> {
        let sha256 = hex(&Sha256::digest(text.as_bytes()));
        if let Some(existing) = self.get_by_hash(conn, &sha256)? {
            return Ok(existing);
        }
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, self.blob_path(&sha256))?;

        let record = AttachmentMeta {
            id: uuid::Uuid::new_v4().to_string(),
            sha256,
            name: name.chars().take(255).collect(),
            mime: guess_mime(name).to_owned(),
            size_bytes: text.len() as i64,
            char_count: text.chars().count() as i64,
        };
        conn.execute(
            "INSERT INTO attachments (id, sha256, name, mime, size_bytes, char_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.id,
                record.sha256,
                record.name,
                record.mime,
                record.size_bytes,
                record.char_count,
                now_ms()
            ],
        )
        .map_err(sql_err)?;
        Ok(record)
    }

    pub fn get(&self, conn: &Connection, id: &str) -> Result<Option<AttachmentMeta>> {
        conn.query_row(
            "SELECT id, sha256, name, mime, size_bytes, char_count FROM attachments WHERE id = ?1",
            params![id],
            row_to_meta,
        )
        .optional()
        .map_err(sql_err)
    }

    fn get_by_hash(&self, conn: &Connection, sha256: &str) -> Result<Option<AttachmentMeta>> {
        conn.query_row(
            "SELECT id, sha256, name, mime, size_bytes, char_count FROM attachments WHERE sha256 = ?1",
            params![sha256],
            row_to_meta,
        )
        .optional()
        .map_err(sql_err)
    }

    pub fn for_message(&self, conn: &Connection, message_id: &str) -> Result<Vec<AttachmentMeta>> {
        let mut stmt = conn
            .prepare_cached(
                "SELECT a.id, a.sha256, a.name, a.mime, a.size_bytes, a.char_count FROM attachments a
                 JOIN message_attachments ma ON ma.attachment_id = a.id WHERE ma.message_id = ?1 ORDER BY a.name",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![message_id], row_to_meta)
            .map_err(sql_err)?;
        rows.map(|r| r.map_err(sql_err)).collect()
    }

    /// Loads the stored text. The blob is immutable, so the hash is the integrity check.
    pub fn read_text(&self, meta: &AttachmentMeta) -> Result<String> {
        let bytes = std::fs::read(self.blob_path(&meta.sha256))?;
        if hex(&Sha256::digest(&bytes)) != meta.sha256 {
            return Err(Error::Io(format!(
                "attachment {} failed integrity check",
                meta.name
            )));
        }
        String::from_utf8(bytes).map_err(|_| Error::Io("attachment is not valid UTF-8".into()))
    }
}

fn row_to_meta(r: &rusqlite::Row<'_>) -> rusqlite::Result<AttachmentMeta> {
    Ok(AttachmentMeta {
        id: r.get(0)?,
        sha256: r.get(1)?,
        name: r.get(2)?,
        mime: r.get(3)?,
        size_bytes: r.get(4)?,
        char_count: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Connection, AttachmentStore, PathBuf) {
        let mut c = Connection::open_in_memory().unwrap();
        super::super::migrations::apply(&mut c).unwrap();
        let dir = std::env::temp_dir().join(format!("opengent-att-{}", uuid::Uuid::new_v4()));
        (c, AttachmentStore::new(dir.clone()), dir)
    }

    #[test]
    fn import_dedupes_by_content_and_reads_back() {
        let (c, store, dir) = setup();
        let file = dir.join("notes.md");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "# hello\nworld → ✓").unwrap();
        let a = store.import_file(&c, &file).unwrap();
        let b = store.import_file(&c, &file).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.mime, "text/markdown");
        assert_eq!(a.char_count, "# hello\nworld → ✓".chars().count() as i64);
        assert_eq!(store.read_text(&a).unwrap(), "# hello\nworld → ✓");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_binary_and_oversized() {
        let (c, store, dir) = setup();
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("x.bin");
        std::fs::write(&bin, [0u8, 159, 146, 150]).unwrap();
        assert!(store.import_file(&c, &bin).is_err());
        let big = dir.join("big.txt");
        std::fs::write(&big, vec![b'a'; MAX_ATTACHMENT_BYTES as usize + 1]).unwrap();
        assert!(store
            .import_file(&c, &big)
            .unwrap_err()
            .to_string()
            .contains("larger"));
        assert!(store.import_file(&c, &dir).is_err(), "directories rejected");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
