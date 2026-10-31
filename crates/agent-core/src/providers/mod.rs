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

/// A function the model may call. `parameters` is a JSON Schema object.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub max_output_tokens: Option<u32>,
    /// Tools offered for this request; empty = no tool calling.
    pub tools: Vec<ToolSpec>,
}

/// Normalized streaming events.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    TextDelta(String),
    /// Reasoning/thinking content for models that expose it (e.g. OpenAI `reasoning_content`).
    ReasoningDelta(String),
    /// A complete tool call, emitted once its streamed fragments are assembled (before
    /// `Done`). `arguments` is the raw JSON text from the model and may be malformed.
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
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
        request: &CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ProviderEvent>>;
}
