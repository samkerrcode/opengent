//! SQLite persistence. A single worker thread owns the connection; async callers submit
//! closures over a bounded queue and await the result, so database work never blocks Tokio's
//! worker threads and writes are naturally serialized.

pub mod agents;
pub mod attachments;
pub mod chats;
mod db;
mod migrations;

pub use attachments::{AttachmentMeta, AttachmentStore, MAX_ATTACHMENT_BYTES};
pub use chats::{ChatDetail, ChatSummary, MessageStatus, StoredMessage};
pub use db::Db;
