use serde::Serialize;
use ts_rs::TS;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Error type crossing the core boundary. Serializable so the host can forward it over IPC.
/// Messages must never contain secrets.
#[derive(Debug, Clone, thiserror::Error, Serialize, TS)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
#[ts(export)]
pub enum Error {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("run is already active for this chat")]
    RunBusy,
    #[error("provider error: {0}")]
    Provider(String),
    #[error("secret store error: {0}")]
    Secrets(String),
    #[error("mcp error: {0}")]
    Mcp(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}
