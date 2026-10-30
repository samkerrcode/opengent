//! `agent-core` is the UI-independent runtime behind the desktop app.
//!
//! It owns product identity, data-root resolution, the canonical domain model, the run
//! coordinator (agent loop), provider adapters, secret storage, and the MCP client.
//! It must never depend on Tauri or any UI framework; the host supplies platform
//! services through the small traits defined here.

pub mod agent;
pub mod config;
pub mod domain;
pub mod error;
pub mod http;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod providers;
pub mod secrets;
pub mod storage;

pub use error::{Error, Result};
