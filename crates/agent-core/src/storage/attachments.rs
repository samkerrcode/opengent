//! Immutable, content-addressed attachment snapshots. Bytes live at
//! `<data_root>/attachments/<sha256>`; metadata lives in SQLite. Text-like files are stored
//! after UTF-8 validation; images are sniffed by magic bytes and stored raw so they can be
//! sent to vision-capable models as base64. PDFs keep the original file plus one rendered
//! image per leading page and a page-1 thumbnail (see `crate::pdf`), each its own
//! content-addressed blob.
//!
//! Importing is split so nothing slow runs on the SQLite worker: `load` and `prepare` touch
//! only the file system and the PDF renderer, `find` and `commit` only do bounded writes.
//!
//! Nothing deletes attachments or blobs yet; garbage collection of blobs no message refers
//! to is planned (page rows already cascade with their attachment).

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use super::db::{now_ms, sql_err};
use crate::agent::context::{encode_base64, AttachmentContent, PageData};
use crate::pdf::{
    self, PdfRasterizer, RenderOptions, RenderedPdf, PDF_MAGIC, PDF_MIME, RENDER_VERSION,
};
use crate::providers::Capabilities;
use crate::{Error, Result};

/// Size limit for text files and images.
pub const MAX_ATTACHMENT_BYTES: u64 = 2 * 1024 * 1024;
/// Size limit for PDFs, which are routinely larger than any text file worth attaching.
pub const MAX_PDF_BYTES: u64 = 32 * 1024 * 1024;
/// Base64 kept by the in-memory payload cache (`EncodedCache`).
const ENCODED_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// Why an image cannot go to a text-only model. `src/lib/attachments.ts` shows the same
/// text; `attachment-reasons.json` pins the two together.
pub const IMAGE_NEEDS_VISION: &str = "the selected model does not accept image attachments";
/// Why a PDF cannot go to a text-only model.
pub const PDF_NEEDS_VISION: &str =
    "the selected model does not accept images, and PDFs are sent as page images";

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
    /// PDFs only: pages in the document.
    pub page_count: Option<u32>,
    /// PDFs only: leading pages stored as images (at most `page_count`).
    pub pages_rendered: u32,
}

/// How an attachment reaches the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Text,
    Image,
    Pdf,
}

impl AttachmentKind {
    fn of(mime: &str) -> Self {
        if mime == PDF_MIME {
            Self::Pdf
        } else if mime.starts_with("image/") {
            Self::Image
        } else {
            Self::Text
        }
    }

    /// Why a model that does (`images`) or does not accept images cannot be sent this
    /// kind, or `None` if it can.
    pub fn unusable_reason(self, images: bool) -> Option<&'static str> {
        match self {
            Self::Image if !images => Some(IMAGE_NEEDS_VISION),
            Self::Pdf if !images => Some(PDF_NEEDS_VISION),
            _ => None,
        }
    }
}

impl AttachmentMeta {
    pub fn kind(&self) -> AttachmentKind {
        AttachmentKind::of(&self.mime)
    }

    /// Why a model with `caps` cannot be sent this attachment, or `None` if it can. The one
    /// gate for picking, dropping and sending; `src/lib/attachments.ts` mirrors it for the UI.
    pub fn unusable_reason(&self, caps: &Capabilities) -> Option<&'static str> {
        self.kind().unusable_reason(caps.images)
    }
}

/// A file read, size-checked, classified and hashed, but not yet stored.
#[derive(Debug)]
pub struct LoadedFile {
    name: String,
    mime: &'static str,
    bytes: Vec<u8>,
    sha256: String,
    char_count: i64,
}

impl LoadedFile {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> AttachmentKind {
        AttachmentKind::of(self.mime)
    }
}

/// A `LoadedFile` with everything derived from it, ready for `AttachmentStore::commit`.
#[derive(Debug)]
pub struct PreparedAttachment {
    file: LoadedFile,
    pdf: Option<RenderedPdf>,
}

/// One rendered PDF image (a page or the thumbnail): where its blob lives and what it is.
#[derive(Debug, Clone, PartialEq)]
pub struct PageRef {
    pub sha256: String,
    pub mime: String,
}

/// What to read from disk to send one attachment, resolved on the database thread
/// (`AttachmentStore::source`) and read off it (`AttachmentStore::load_content`).
#[derive(Debug, Clone)]
pub struct AttachmentSource {
    pub meta: AttachmentMeta,
    /// Whether the model can see images. If not, images and PDFs are not read at all:
    /// they compile to a placeholder either way.
    visible: bool,
    pages: Vec<PageRef>,
}

/// Base64 payloads of recently sent images and pages, keyed by content hash, so each turn
/// does not re-read, re-hash and re-encode the whole history. Blobs are immutable and were
/// verified when first read, so an entry never goes stale. Oldest entries go first.
#[derive(Default)]
struct EncodedCache {
    entries: HashMap<String, Arc<str>>,
    order: VecDeque<String>,
    bytes: usize,
}

impl EncodedCache {
    fn get(&self, sha256: &str) -> Option<Arc<str>> {
        self.entries.get(sha256).cloned()
    }

    fn insert(&mut self, sha256: &str, data: Arc<str>) {
        if data.len() > ENCODED_CACHE_BYTES || self.entries.contains_key(sha256) {
            return;
        }
        while self.bytes + data.len() > ENCODED_CACHE_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(gone) = self.entries.remove(&oldest) {
                self.bytes -= gone.len();
            }
        }
        self.bytes += data.len();
        self.order.push_back(sha256.to_owned());
        self.entries.insert(sha256.to_owned(), data);
    }
}

/// Blobs written by one `commit`. Dropped before `keep`, it deletes the ones that did not
/// exist before, so a failed commit leaves nothing behind; blobs that other attachments
/// already shared are never touched.
struct NewBlobs<'a> {
    store: &'a AttachmentStore,
    created: Vec<PathBuf>,
}

impl NewBlobs<'_> {
    fn write(&mut self, sha256: &str, bytes: &[u8]) -> Result<()> {
        let path = self.store.blob_path(sha256);
        let existed = path.exists();
        self.store.write_blob(sha256, bytes)?;
        if !existed {
            self.created.push(path);
        }
        Ok(())
    }

    fn keep(mut self) {
        self.created.clear();
    }
}

impl Drop for NewBlobs<'_> {
    fn drop(&mut self) {
        for path in &self.created {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub struct AttachmentStore {
    dir: PathBuf,
    rasterizer: Arc<dyn PdfRasterizer>,
    cache: Mutex<EncodedCache>,
}

/// `AttachmentMeta` columns for a query over `attachments a`.
const META_COLUMNS: &str = "a.id, a.sha256, a.name, a.mime, a.size_bytes, a.char_count, \
    a.page_count, (SELECT COUNT(*) FROM attachment_pages p WHERE p.attachment_id = a.id)";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Sniffs the magic bytes of common raster image formats. Returns the canonical mime when
/// the prefix matches, so a `.png` rename of arbitrary binary data is not accepted as an
/// image (it falls through to the UTF-8 text check and is rejected as binary).
// TODO: additional formats worth supporting — HEIC/HEIF (iPhone photos), BMP, TIFF.
// Non-UTF-8 text (UTF-16, Latin-1) needs a separate import path, not just sniffing here.
fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
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

fn regular_file(meta: std::fs::Metadata) -> Result<()> {
    if meta.is_file() {
        Ok(())
    } else {
        Err(Error::InvalidInput(
            "only regular files can be attached".into(),
        ))
    }
}

impl AttachmentStore {
    /// A store that renders PDFs with the operating system's engine.
    pub fn new(dir: PathBuf) -> Self {
        Self::with_rasterizer(dir, pdf::native())
    }

    pub fn with_rasterizer(dir: PathBuf, rasterizer: Arc<dyn PdfRasterizer>) -> Self {
        Self {
            dir,
            rasterizer,
            cache: Mutex::default(),
        }
    }

    fn blob_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(sha256)
    }

    /// Reads a user-selected file and decides what it is by content: a PDF (leading
    /// `%PDF-`), bounded UTF-8 text, or a known image format. No database access.
    pub fn load(&self, path: &Path) -> Result<LoadedFile> {
        // The path is checked first so a FIFO or device is never opened (that can block),
        // and the open handle again, because that is what is actually read.
        regular_file(std::fs::metadata(path)?)?;
        let mut file = std::fs::File::open(path)?;
        regular_file(file.metadata()?)?;
        // The size limit depends on the kind, so sniff the first bytes and then read at
        // most one byte past that kind's limit — never trusting a size reported earlier.
        let mut bytes = Vec::new();
        file.by_ref()
            .take(PDF_MAGIC.len() as u64)
            .read_to_end(&mut bytes)?;
        // PDF before text: a PDF without binary streams is valid UTF-8 too.
        let is_pdf = bytes == PDF_MAGIC;
        let (limit, what) = if is_pdf {
            (MAX_PDF_BYTES, "PDF")
        } else {
            (MAX_ATTACHMENT_BYTES, "file")
        };
        file.take(limit + 1 - bytes.len() as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(Error::InvalidInput(format!(
                "{what} is larger than {} MiB",
                limit / 1024 / 1024
            )));
        }
        let name: String = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .chars()
            .take(255)
            .collect();
        // Text before images: real image blobs always fail UTF-8 validation, while a text
        // file that merely starts with "GIF89a" must not be imported as an image.
        let text = std::str::from_utf8(&bytes)
            .ok()
            .filter(|t| !t.contains('\0'));
        let (mime, char_count) = if is_pdf {
            (PDF_MIME, 0)
        } else if let Some(text) = text {
            (guess_mime(&name), text.chars().count() as i64)
        } else if let Some(mime) = sniff_image(&bytes) {
            (mime, 0)
        } else {
            return Err(Error::InvalidInput(
                "only UTF-8 text, PDFs and PNG/JPEG/GIF/WebP images are supported".into(),
            ));
        };
        Ok(LoadedFile {
            name,
            mime,
            sha256: hex(&Sha256::digest(&bytes)),
            bytes,
            char_count,
        })
    }

    /// The stored attachment with the same content, if any and if it is complete. Checking
    /// before `prepare` means a file that was attached before is never rendered again —
    /// unless its stored pages are stale (`is_stale`).
    pub fn find(&self, conn: &Connection, file: &LoadedFile) -> Result<Option<AttachmentMeta>> {
        match self.get_by_hash(conn, &file.sha256)? {
            Some(meta) if !self.is_stale(conn, &meta)? => Ok(Some(meta)),
            _ => Ok(None),
        }
    }

    /// A PDF whose pages should be rendered again: by an older renderer
    /// (`pdf::RENDER_VERSION`), or cut short by the time budget on an earlier import.
    fn is_stale(&self, conn: &Connection, meta: &AttachmentMeta) -> Result<bool> {
        if meta.kind() != AttachmentKind::Pdf {
            return Ok(false);
        }
        let version: Option<u32> = conn
            .query_row(
                "SELECT render_version FROM attachments WHERE id = ?1",
                params![meta.id],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        let expected = RenderOptions::default().expected_pages(meta.page_count.unwrap_or(0));
        Ok(version != Some(RENDER_VERSION) || meta.pages_rendered < expected)
    }

    /// Derives what will be stored next to the original — for PDFs, the page images.
    /// Slow and CPU-bound; no database access.
    pub fn prepare(&self, file: LoadedFile) -> Result<PreparedAttachment> {
        let pdf = (file.mime == PDF_MIME)
            .then(|| {
                self.rasterizer
                    .render(&file.bytes, &RenderOptions::default())
            })
            .transpose()?;
        Ok(PreparedAttachment { file, pdf })
    }

    /// Stores the blobs and records the attachment and its pages in one transaction; on
    /// failure, blobs it created are removed again. Identical content committed in the
    /// meantime returns the existing record, unless that record is stale: then its pages
    /// are replaced and it keeps its id, so messages that refer to it see the new pages.
    pub fn commit(
        &self,
        conn: &Connection,
        prepared: PreparedAttachment,
    ) -> Result<AttachmentMeta> {
        let PreparedAttachment { file, pdf } = prepared;
        let existing = self.get_by_hash(conn, &file.sha256)?;
        if let Some(existing) = &existing {
            if !self.is_stale(conn, existing)? {
                return Ok(existing.clone());
            }
        }
        std::fs::create_dir_all(&self.dir)?;
        let mut blobs = NewBlobs {
            store: self,
            created: Vec::new(),
        };
        blobs.write(&file.sha256, &file.bytes)?;
        let mut pages = Vec::new();
        for page in pdf.iter().flat_map(|p| &p.pages) {
            let sha256 = hex(&Sha256::digest(&page.bytes));
            blobs.write(&sha256, &page.bytes)?;
            pages.push((sha256, page.mime));
        }
        let thumbnail = match &pdf {
            Some(p) => {
                let sha256 = hex(&Sha256::digest(&p.thumbnail.bytes));
                blobs.write(&sha256, &p.thumbnail.bytes)?;
                Some((sha256, p.thumbnail.mime))
            }
            None => None,
        };
        let (thumb_sha, thumb_mime) = thumbnail.unzip();
        let render_version = pdf.is_some().then_some(RENDER_VERSION);

        let replacing = existing.is_some();
        let mut record = existing.unwrap_or_else(|| AttachmentMeta {
            id: uuid::Uuid::new_v4().to_string(),
            sha256: file.sha256,
            name: file.name,
            mime: file.mime.to_owned(),
            size_bytes: file.bytes.len() as i64,
            char_count: file.char_count,
            page_count: None,
            pages_rendered: 0,
        });
        record.page_count = pdf.as_ref().map(|p| p.page_count);
        record.pages_rendered = pages.len() as u32;

        let tx = conn.unchecked_transaction().map_err(sql_err)?;
        if replacing {
            tx.execute(
                "UPDATE attachments SET page_count = ?2, render_version = ?3,
                     thumbnail_sha256 = ?4, thumbnail_mime = ?5 WHERE id = ?1",
                params![
                    record.id,
                    record.page_count,
                    render_version,
                    thumb_sha,
                    thumb_mime
                ],
            )
            .map_err(sql_err)?;
            tx.execute(
                "DELETE FROM attachment_pages WHERE attachment_id = ?1",
                params![record.id],
            )
            .map_err(sql_err)?;
        } else {
            tx.execute(
                "INSERT INTO attachments
                     (id, sha256, name, mime, size_bytes, char_count, page_count, created_at,
                      render_version, thumbnail_sha256, thumbnail_mime)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    record.id,
                    record.sha256,
                    record.name,
                    record.mime,
                    record.size_bytes,
                    record.char_count,
                    record.page_count,
                    now_ms(),
                    render_version,
                    thumb_sha,
                    thumb_mime
                ],
            )
            .map_err(sql_err)?;
        }
        for (index, (sha256, mime)) in pages.iter().enumerate() {
            tx.execute(
                "INSERT INTO attachment_pages (attachment_id, page_index, sha256, mime)
                 VALUES (?1, ?2, ?3, ?4)",
                params![record.id, index as i64, sha256, mime],
            )
            .map_err(sql_err)?;
        }
        tx.commit().map_err(sql_err)?;
        blobs.keep();
        Ok(record)
    }

    /// `load` → `find` → `prepare` → `commit` in one call, for callers that are not on an
    /// async runtime. The host runs the steps separately to keep rendering off the
    /// database thread.
    pub fn import_file(&self, conn: &Connection, path: &Path) -> Result<AttachmentMeta> {
        let file = self.load(path)?;
        match self.find(conn, &file)? {
            Some(existing) => Ok(existing),
            None => self.commit(conn, self.prepare(file)?),
        }
    }

    fn write_blob(&self, sha256: &str, bytes: &[u8]) -> Result<()> {
        let tmp = self.dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let written = std::fs::write(&tmp, bytes)
            .and_then(|()| std::fs::rename(&tmp, self.blob_path(sha256)));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        Ok(written?)
    }

    pub fn get(&self, conn: &Connection, id: &str) -> Result<Option<AttachmentMeta>> {
        conn.query_row(
            &format!("SELECT {META_COLUMNS} FROM attachments a WHERE a.id = ?1"),
            params![id],
            row_to_meta,
        )
        .optional()
        .map_err(sql_err)
    }

    fn get_by_hash(&self, conn: &Connection, sha256: &str) -> Result<Option<AttachmentMeta>> {
        conn.query_row(
            &format!("SELECT {META_COLUMNS} FROM attachments a WHERE a.sha256 = ?1"),
            params![sha256],
            row_to_meta,
        )
        .optional()
        .map_err(sql_err)
    }

    pub fn for_message(&self, conn: &Connection, message_id: &str) -> Result<Vec<AttachmentMeta>> {
        let mut stmt = conn
            .prepare_cached(&format!(
                "SELECT {META_COLUMNS} FROM attachments a
                 JOIN message_attachments ma ON ma.attachment_id = a.id WHERE ma.message_id = ?1 ORDER BY a.name"
            ))
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![message_id], row_to_meta)
            .map_err(sql_err)?;
        rows.map(|r| r.map_err(sql_err)).collect()
    }

    /// The rendered pages of a PDF in page order (empty for anything else). Only the
    /// references: read the images with `read_page`, off the database thread.
    pub fn pages(&self, conn: &Connection, meta: &AttachmentMeta) -> Result<Vec<PageRef>> {
        let mut stmt = conn
            .prepare_cached(
                "SELECT sha256, mime FROM attachment_pages WHERE attachment_id = ?1
                 ORDER BY page_index",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![meta.id], |r| {
                Ok(PageRef {
                    sha256: r.get(0)?,
                    mime: r.get(1)?,
                })
            })
            .map_err(sql_err)?;
        rows.map(|r| r.map_err(sql_err)).collect()
    }

    /// The small page-1 image of a PDF, for chips in the UI.
    pub fn thumbnail(&self, conn: &Connection, meta: &AttachmentMeta) -> Result<Option<PageRef>> {
        let row: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT thumbnail_sha256, thumbnail_mime FROM attachments WHERE id = ?1",
                params![meta.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql_err)?;
        Ok(match row {
            Some((Some(sha256), Some(mime))) => Some(PageRef { sha256, mime }),
            _ => None,
        })
    }

    /// Looks up what `load_content` will need for `meta`, for a model that does
    /// (`images`) or does not accept image input.
    pub fn source(
        &self,
        conn: &Connection,
        meta: AttachmentMeta,
        images: bool,
    ) -> Result<AttachmentSource> {
        let pages = if images && meta.kind() == AttachmentKind::Pdf {
            self.pages(conn, &meta)?
        } else {
            Vec::new()
        };
        Ok(AttachmentSource {
            meta,
            visible: images,
            pages,
        })
    }

    /// Reads and encodes an attachment's payload for the request compiler. File access
    /// only — page images run to megabytes, so call this off the database thread.
    /// Recently encoded images and pages come from memory.
    pub fn load_content(&self, source: &AttachmentSource) -> Result<AttachmentContent> {
        let meta = &source.meta;
        Ok(match meta.kind() {
            AttachmentKind::Text => AttachmentContent::Text(self.read_text(meta)?),
            _ if !source.visible => AttachmentContent::Omitted,
            AttachmentKind::Image => AttachmentContent::Image {
                mime: meta.mime.clone(),
                data_base64: self.encoded(&meta.sha256, || self.read_bytes(meta))?,
            },
            AttachmentKind::Pdf => AttachmentContent::Pages {
                total: meta.page_count.unwrap_or(source.pages.len() as u32),
                pages: source
                    .pages
                    .iter()
                    .map(|p| {
                        Ok(PageData {
                            mime: p.mime.clone(),
                            data_base64: self.encoded(&p.sha256, || self.read_page(p))?,
                        })
                    })
                    .collect::<Result<_>>()?,
            },
        })
    }

    /// Base64 of the blob `sha256`, from the cache or else `read` and cached.
    fn encoded(&self, sha256: &str, read: impl FnOnce() -> Result<Vec<u8>>) -> Result<Arc<str>> {
        if let Some(hit) = self.cache.lock().ok().and_then(|c| c.get(sha256)) {
            return Ok(hit);
        }
        let data = encode_base64(&read()?);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(sha256, data.clone());
        }
        Ok(data)
    }

    /// Loads the stored text. The blob is immutable, so the hash is the integrity check.
    pub fn read_text(&self, meta: &AttachmentMeta) -> Result<String> {
        String::from_utf8(self.read_bytes(meta)?)
            .map_err(|_| Error::Io("attachment is not valid UTF-8".into()))
    }

    /// Loads the original file's bytes, with the same integrity check, naming the
    /// attachment in the error.
    pub fn read_bytes(&self, meta: &AttachmentMeta) -> Result<Vec<u8>> {
        self.read_blob(&meta.sha256).map_err(|e| {
            let why = match e {
                Error::Io(why) => why,
                other => other.to_string(),
            };
            Error::Io(format!("attachment {}: {why}", meta.name))
        })
    }

    /// Loads one rendered page or thumbnail image.
    pub fn read_page(&self, page: &PageRef) -> Result<Vec<u8>> {
        self.read_blob(&page.sha256)
    }

    fn read_blob(&self, sha256: &str) -> Result<Vec<u8>> {
        // Hashes come from the database, but a blob name must never become a path.
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Io(
                "the stored copy has an invalid content hash".into(),
            ));
        }
        let bytes = std::fs::read(self.blob_path(sha256)).map_err(|e| {
            Error::Io(match e.kind() {
                std::io::ErrorKind::NotFound => "the stored copy is missing".into(),
                _ => format!("the stored copy could not be read ({e})"),
            })
        })?;
        if hex(&Sha256::digest(&bytes)) != sha256 {
            return Err(Error::Io(
                "the stored copy failed its integrity check".into(),
            ));
        }
        Ok(bytes)
    }
}

/// `data:{mime};base64,…` URL for rendering image bytes in a webview (`<img src>`).
pub fn data_url(mime: &str, bytes: &[u8]) -> String {
    use base64::Engine;
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn row_to_meta(r: &rusqlite::Row<'_>) -> rusqlite::Result<AttachmentMeta> {
    Ok(AttachmentMeta {
        id: r.get(0)?,
        sha256: r.get(1)?,
        name: r.get(2)?,
        mime: r.get(3)?,
        size_bytes: r.get(4)?,
        char_count: r.get(5)?,
        page_count: r.get(6)?,
        pages_rendered: r.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::pdf::{fixtures, PageImage};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Stands in for the OS renderer: one distinct fake image per page, capped like the
    /// real one, and a count of how often it ran. `slow` makes it stop after one page, as
    /// if the time budget ran out.
    #[derive(Default)]
    struct FakeRasterizer {
        renders: AtomicUsize,
        slow: AtomicBool,
    }

    impl PdfRasterizer for FakeRasterizer {
        fn render(&self, bytes: &[u8], opts: &RenderOptions) -> Result<RenderedPdf> {
            self.renders.fetch_add(1, Ordering::SeqCst);
            let text = String::from_utf8_lossy(bytes);
            if text.contains("/Encrypt") {
                return Err(Error::InvalidInput("the PDF is password-protected".into()));
            }
            let page_count = text.matches("/Type /Page ").count() as u32;
            if page_count == 0 {
                return Err(Error::InvalidInput("the PDF is damaged".into()));
            }
            let rendered = if self.slow.load(Ordering::SeqCst) {
                1
            } else {
                opts.expected_pages(page_count)
            };
            Ok(RenderedPdf {
                page_count,
                pages: (0..rendered)
                    .map(|i| PageImage {
                        mime: "image/png",
                        bytes: format!("page {i} of {}", bytes.len()).into_bytes(),
                    })
                    .collect(),
                thumbnail: PageImage {
                    mime: "image/png",
                    bytes: format!("thumbnail of {}", bytes.len()).into_bytes(),
                },
            })
        }
    }

    fn setup_with(rasterizer: Arc<dyn PdfRasterizer>) -> (Connection, AttachmentStore, PathBuf) {
        let mut c = Connection::open_in_memory().unwrap();
        super::super::migrations::apply(&mut c).unwrap();
        let dir = std::env::temp_dir().join(format!("opengent-att-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (
            c,
            AttachmentStore::with_rasterizer(dir.clone(), rasterizer),
            dir,
        )
    }

    fn setup() -> (Connection, AttachmentStore, PathBuf) {
        setup_with(Arc::new(FakeRasterizer::default()))
    }

    fn pages(n: usize) -> Vec<fixtures::Page> {
        (0..n).map(|_| fixtures::page(200, 100)).collect()
    }

    fn vision(images: bool) -> Capabilities {
        Capabilities {
            images,
            ..Capabilities::default()
        }
    }

    #[test]
    fn ascii_only_pdf_is_imported_as_a_pdf_with_its_pages() {
        let (c, store, dir) = setup();
        // Wrong extension on purpose: the content decides, and this file is valid UTF-8.
        let file = dir.join("report.txt");
        let bytes = fixtures::pdf(&pages(3));
        assert!(std::str::from_utf8(&bytes).is_ok());
        std::fs::write(&file, &bytes).unwrap();

        let a = store.import_file(&c, &file).unwrap();
        assert_eq!(a.kind(), AttachmentKind::Pdf);
        assert_eq!(a.mime, "application/pdf");
        assert_eq!(
            (a.page_count, a.pages_rendered, a.char_count),
            (Some(3), 3, 0)
        );
        assert_eq!(store.read_bytes(&a).unwrap(), bytes, "original is kept");

        let refs = store.pages(&c, &a).unwrap();
        assert_eq!(refs.len(), 3);
        let thumb = store.thumbnail(&c, &a).unwrap().unwrap();
        let expected = format!("thumbnail of {}", bytes.len());
        assert_eq!(store.read_page(&thumb).unwrap(), expected.as_bytes());
        for (i, page) in refs.iter().enumerate() {
            assert_eq!(page.mime, "image/png");
            let expected = format!("page {i} of {}", bytes.len());
            assert_eq!(store.read_page(page).unwrap(), expected.as_bytes());
        }
        assert_eq!(store.get(&c, &a.id).unwrap().unwrap(), a);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn long_pdfs_keep_the_real_page_count_and_the_leading_pages() {
        let (c, store, dir) = setup();
        let file = dir.join("long.pdf");
        std::fs::write(&file, fixtures::pdf(&pages(57))).unwrap();
        let a = store.import_file(&c, &file).unwrap();
        assert_eq!((a.page_count, a.pages_rendered), (Some(57), 20));
        assert_eq!(store.pages(&c, &a).unwrap().len(), 20);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reattaching_a_pdf_keeps_its_pages_and_does_not_render_again() {
        let fake = Arc::new(FakeRasterizer::default());
        let (c, store, dir) = setup_with(fake.clone());
        let file = dir.join("a.pdf");
        std::fs::write(&file, fixtures::pdf(&pages(2))).unwrap();
        let a = store.import_file(&c, &file).unwrap();
        let copy = dir.join("copy of a.pdf");
        std::fs::copy(&file, &copy).unwrap();
        let b = store.import_file(&c, &copy).unwrap();
        assert_eq!(a, b);
        assert_eq!(b.pages_rendered, 2);
        assert_eq!(fake.renders.load(Ordering::SeqCst), 1);

        // Two imports racing past `find` still end up as one record.
        let prepared = store.prepare(store.load(&file).unwrap()).unwrap();
        assert_eq!(store.commit(&c, prepared).unwrap(), a);
        assert_eq!(store.pages(&c, &a).unwrap().len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pdf_size_limit_is_separate_from_the_text_limit() {
        let (c, store, dir) = setup();
        let mut bytes = fixtures::pdf(&pages(1));
        bytes.resize(MAX_ATTACHMENT_BYTES as usize + 1, b'\n');
        let file = dir.join("big.pdf");
        std::fs::write(&file, &bytes).unwrap();
        let a = store.import_file(&c, &file).unwrap();
        assert_eq!(a.size_bytes, bytes.len() as i64);

        bytes.resize(MAX_PDF_BYTES as usize + 1, b'\n');
        std::fs::write(&file, &bytes).unwrap();
        let err = store.import_file(&c, &file).unwrap_err().to_string();
        assert!(err.contains("PDF is larger than 32 MiB"), "{err}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unrenderable_pdfs_are_rejected_and_leave_nothing_behind() {
        let (c, store, dir) = setup();
        let locked = dir.join("locked.pdf");
        std::fs::write(&locked, fixtures::encrypted_pdf()).unwrap();
        let err = store.import_file(&c, &locked).unwrap_err().to_string();
        assert!(err.contains("password-protected"), "{err}");

        let broken = dir.join("broken.pdf");
        std::fs::write(&broken, b"%PDF-1.4 but nothing else").unwrap();
        assert!(store.import_file(&c, &broken).is_err());

        let rows: i64 = c
            .query_row("SELECT COUNT(*) FROM attachments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "no blobs");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pdf_marker_must_be_the_first_bytes() {
        let (c, store, dir) = setup();
        let mut bytes = b"\x00\x00junk".to_vec();
        bytes.extend(fixtures::pdf(&pages(1)));
        let file = dir.join("junk.pdf");
        std::fs::write(&file, bytes).unwrap();
        let err = store.import_file(&c, &file).unwrap_err().to_string();
        assert!(err.contains("are supported"), "{err}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn visual_attachments_need_a_vision_model() {
        let (c, store, dir) = setup();
        let import = |name: &str, bytes: &[u8]| {
            let file = dir.join(name);
            std::fs::write(&file, bytes).unwrap();
            store.import_file(&c, &file).unwrap()
        };
        let text = import("a.txt", b"hello");
        let image = import("a.png", b"\x89PNG\r\n\x1a\n\0\0\0\0");
        let pdf = import("a.pdf", &fixtures::pdf(&pages(1)));
        for meta in [&text, &image, &pdf] {
            assert_eq!(meta.unusable_reason(&vision(true)), None);
        }
        assert_eq!(text.unusable_reason(&vision(false)), None);
        assert_eq!(
            image.unusable_reason(&vision(false)),
            Some(IMAGE_NEEDS_VISION)
        );
        assert_eq!(pdf.unusable_reason(&vision(false)), Some(PDF_NEEDS_VISION));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The UI shows the same reasons (`src/lib/attachments.ts`); both sides are pinned to
    /// one file so they cannot drift apart.
    #[test]
    fn reasons_match_the_ui() {
        let pinned: HashMap<String, String> =
            serde_json::from_str(include_str!("../../../../src/lib/attachment-reasons.json"))
                .unwrap();
        assert_eq!(pinned["image"], IMAGE_NEEDS_VISION);
        assert_eq!(pinned["pdf"], PDF_NEEDS_VISION);
        assert_eq!(pinned.len(), 2);
    }

    #[test]
    fn a_cut_short_render_is_redone_on_the_next_attach() {
        let fake = Arc::new(FakeRasterizer::default());
        let (c, store, dir) = setup_with(fake.clone());
        let file = dir.join("a.pdf");
        std::fs::write(&file, fixtures::pdf(&pages(3))).unwrap();
        fake.slow.store(true, Ordering::SeqCst);
        let partial = store.import_file(&c, &file).unwrap();
        assert_eq!(partial.pages_rendered, 1);

        fake.slow.store(false, Ordering::SeqCst);
        let full = store.import_file(&c, &file).unwrap();
        assert_eq!(fake.renders.load(Ordering::SeqCst), 2);
        assert_eq!(full.id, partial.id, "messages keep pointing at it");
        assert_eq!(full.pages_rendered, 3);
        assert_eq!(store.pages(&c, &full).unwrap().len(), 3);
        assert_eq!(store.get(&c, &full.id).unwrap().unwrap(), full);

        // Complete now, so the next attach reuses it.
        store.import_file(&c, &file).unwrap();
        assert_eq!(fake.renders.load(Ordering::SeqCst), 2);

        // Pages from an older renderer are redone too.
        c.execute("UPDATE attachments SET render_version = 0", [])
            .unwrap();
        store.import_file(&c, &file).unwrap();
        assert_eq!(fake.renders.load(Ordering::SeqCst), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_commit_removes_the_blobs_it_wrote() {
        let (c, store, dir) = setup();
        let shared = dir.join("shared.txt");
        std::fs::write(&shared, "kept").unwrap();
        let kept = store.import_file(&c, &shared).unwrap();

        let file = dir.join("a.pdf");
        std::fs::write(&file, fixtures::pdf(&pages(2))).unwrap();
        let prepared = store.prepare(store.load(&file).unwrap()).unwrap();
        c.execute_batch("DROP TABLE attachment_pages").unwrap();
        assert!(store.commit(&c, prepared).is_err());

        let rows: i64 = c
            .query_row("SELECT COUNT(*) FROM attachments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "the insert was rolled back");
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        let mut expected = vec![kept.sha256.clone(), "a.pdf".into(), "shared.txt".into()];
        expected.sort();
        assert_eq!(left, expected, "only blobs that existed before remain");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_errors_say_what_went_wrong() {
        let (c, store, dir) = setup();
        let file = dir.join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        let a = store.import_file(&c, &file).unwrap();
        let blob = dir.join(&a.sha256);

        std::fs::write(&blob, "tampered").unwrap();
        let err = store.read_bytes(&a).unwrap_err().to_string();
        assert!(
            err.contains("a.txt: the stored copy failed its integrity check"),
            "{err}"
        );

        std::fs::remove_file(&blob).unwrap();
        let err = store.read_bytes(&a).unwrap_err().to_string();
        assert!(err.contains("a.txt: the stored copy is missing"), "{err}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sent_images_are_encoded_once() {
        let (c, store, dir) = setup();
        let file = dir.join("a.pdf");
        std::fs::write(&file, fixtures::pdf(&pages(2))).unwrap();
        let pdf = store.import_file(&c, &file).unwrap();
        let source = store.source(&c, pdf.clone(), true).unwrap();
        let AttachmentContent::Pages { pages: first, .. } = store.load_content(&source).unwrap()
        else {
            panic!("expected pages")
        };
        // Served from memory: the blobs are no longer read, and the payload is shared.
        for page in store.pages(&c, &pdf).unwrap() {
            std::fs::remove_file(dir.join(&page.sha256)).unwrap();
        }
        let AttachmentContent::Pages { pages: again, .. } = store.load_content(&source).unwrap()
        else {
            panic!("expected pages")
        };
        assert!(Arc::ptr_eq(&first[0].data_base64, &again[0].data_base64));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_cache_evicts_the_oldest_entries() {
        let mut cache = EncodedCache::default();
        let big: Arc<str> = "x".repeat(ENCODED_CACHE_BYTES / 2).into();
        cache.insert("a", big.clone());
        cache.insert("b", big.clone());
        cache.insert("c", big.clone());
        assert!(cache.get("a").is_none());
        assert!(cache.get("b").is_some() && cache.get("c").is_some());
        assert_eq!(cache.bytes, ENCODED_CACHE_BYTES);
        cache.insert("huge", "x".repeat(ENCODED_CACHE_BYTES + 1).into());
        assert!(cache.get("huge").is_none());
    }

    #[test]
    fn content_is_loaded_per_kind_and_skipped_for_text_only_models() {
        use base64::Engine;
        let (c, store, dir) = setup();
        let import = |name: &str, bytes: &[u8]| {
            let file = dir.join(name);
            std::fs::write(&file, bytes).unwrap();
            store.import_file(&c, &file).unwrap()
        };
        let text = import("a.txt", b"hello");
        let image = import("a.png", b"\x89PNG\r\n\x1a\n\0\0\0\0");
        let pdf = import("a.pdf", &fixtures::pdf(&pages(25)));
        let load = |meta: &AttachmentMeta, images: bool| {
            let source = store.source(&c, meta.clone(), images).unwrap();
            store.load_content(&source).unwrap()
        };

        assert!(matches!(load(&text, false), AttachmentContent::Text(t) if t == "hello"));
        assert!(
            matches!(load(&image, true), AttachmentContent::Image { mime, .. } if mime == "image/png")
        );
        let AttachmentContent::Pages { total, pages } = load(&pdf, true) else {
            panic!("expected pages")
        };
        assert_eq!((total, pages.len()), (25, 20));
        let first = base64::engine::general_purpose::STANDARD
            .decode(&*pages[0].data_base64)
            .unwrap();
        assert!(first.starts_with(b"page 0 of "));

        // Nothing is read for a model that cannot see it — not even a missing blob matters.
        std::fs::remove_file(dir.join(&image.sha256)).unwrap();
        assert!(matches!(load(&image, false), AttachmentContent::Omitted));
        assert!(matches!(load(&pdf, false), AttachmentContent::Omitted));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn blob_names_that_are_not_hashes_are_never_read() {
        let (_c, store, dir) = setup();
        std::fs::write(dir.join("secret"), b"x").unwrap();
        for sha256 in ["secret", "../secret", ""] {
            let page = PageRef {
                sha256: sha256.into(),
                mime: "image/png".into(),
            };
            assert!(store.read_page(&page).is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn import_dedupes_by_content_and_reads_back() {
        let (c, store, dir) = setup();
        let file = dir.join("notes.md");
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
    fn imports_images_by_magic_bytes_not_extension() {
        let (c, store, dir) = setup();
        // Minimal PNG header + payload; extension deliberately wrong to prove sniffing wins.
        let png = dir.join("mystery.dat");
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        std::fs::write(&png, &bytes).unwrap();
        let a = store.import_file(&c, &png).unwrap();
        assert_eq!(a.mime, "image/png");
        assert_eq!(a.char_count, 0);
        assert_eq!(store.read_bytes(&a).unwrap(), bytes);

        // Text that happens to start with GIF magic stays text.
        let fake_gif = dir.join("note.txt");
        std::fs::write(&fake_gif, "GIF89a is a format I am writing about").unwrap();
        let t = store.import_file(&c, &fake_gif).unwrap();
        assert_eq!(t.mime, "text/plain");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_binary_and_oversized() {
        let (c, store, dir) = setup();
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
