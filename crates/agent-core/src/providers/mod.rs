//! Model provider abstraction. Each adapter turns a `CompletionRequest` into a bounded
//! stream of normalized `ProviderEvent`s. Provider dispatch stays out of the per-token hot
//! path: the coordinator receives events over a bounded channel.

pub mod capabilities;
pub mod fake;
pub mod openai_compat;
pub mod registry;
pub mod sse;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::domain::{Message, StopReason, Usage};
use crate::Result;

/// Static capability declaration. `None`/`false` means "not known to be supported".
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Capabilities {
    pub streaming: bool,
    pub tools: bool,
    pub parallel_tool_calls: bool,
    pub images: bool,
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub max_output_tokens: Option<u32>,
}

/// Normalized streaming events. Tool-call deltas are added in Phase 3.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    TextDelta(String),
    /// Reasoning/thinking content for models that expose it (e.g. OpenAI `reasoning_content`).
    ReasoningDelta(String),
    Usage(Usage),
    Done(StopReason),
    Error(String),
}

/// Bound on buffered provider events before the producer is back-pressured.
pub const PROVIDER_EVENT_BUFFER: usize = 256;

pub trait ModelProvider: Send + Sync + 'static {
    fn id(&self) -> &str;
    fn capabilities(&self) -> Capabilities;

    /// Starts streaming. The returned receiver yields events until `Done`/`Error` or until the
    /// channel closes; implementations must stop promptly when `cancel` fires.
    fn stream(
        &self,
        request: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ProviderEvent>>;
}
