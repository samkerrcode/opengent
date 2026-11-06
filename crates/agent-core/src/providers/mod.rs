//! Model provider abstraction. Each adapter turns a `CompletionRequest` into a bounded
//! stream of normalized `ProviderEvent`s. Provider dispatch stays out of the per-token hot
//! path: the coordinator receives events over a bounded channel.

pub mod capabilities;
pub mod common;
pub mod fake;
pub mod gemini;
pub mod openai;
pub mod openai_compat;
pub mod registry;
pub mod sse;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

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
    /// Provider continuation state (reasoning items, thought signatures, …) emitted in stream
    /// order. The core stores it as a `ContentBlock::ProviderOpaque` at this position and
    /// replays it only to adapters whose `family()` equals `provider`; `data` is private to
    /// that adapter. Never shown to the user and never sent over IPC.
    Opaque {
        provider: &'static str,
        data: serde_json::Value,
    },
    Usage(Usage),
    Done(StopReason),
    Error(String),
}

/// Bound on buffered provider events before the producer is back-pressured.
pub const PROVIDER_EVENT_BUFFER: usize = 256;

/// Upper bound on one opaque block's serialized size. Larger blocks are dropped: a hostile or
/// buggy server must not be able to bloat stored history and every later request.
pub const MAX_OPAQUE_BYTES: usize = 64 * 1024;

/// Whether `data` serializes within `MAX_OPAQUE_BYTES`. Stops serializing at the limit, so
/// an oversized blob costs at most the limit to reject.
pub fn opaque_within_limit(data: &serde_json::Value) -> bool {
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(buf.len())
                .ok_or_else(|| std::io::Error::other("over budget"))?;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(MAX_OPAQUE_BYTES), data).is_ok()
}

/// Per-model metadata reported by a provider's listing endpoint. `None` = not reported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DiscoveredModel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
}

/// Result of model discovery. `models` is sorted; `details` carries whatever metadata the
/// provider exposes and may be empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelDiscovery {
    pub models: Vec<String>,
    pub details: HashMap<String, DiscoveredModel>,
}

/// Boxed future returned by async trait methods (keeps `ModelProvider` object-safe without
/// an `async-trait` dependency).
pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub trait ModelProvider: Send + Sync + 'static {
    fn id(&self) -> &str;
    fn capabilities(&self) -> Capabilities;

    /// Wire-protocol family used to tag `ProviderOpaque` blocks (e.g. `"google_gemini"`).
    /// `None` = this adapter never consumes opaque state, so history compiled for it drops
    /// every opaque block.
    fn family(&self) -> Option<&'static str> {
        None
    }

    /// Starts streaming. The returned receiver yields events until `Done`/`Error` or until the
    /// channel closes; implementations must stop promptly when `cancel` fires.
    fn stream(
        &self,
        request: &CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ProviderEvent>>;

    /// Lists available models with any metadata the provider reports. Default: nothing to
    /// discover (callers keep the cached list).
    fn discover(&self) -> ProviderFuture<'_, ModelDiscovery> {
        Box::pin(async { Ok(ModelDiscovery::default()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_size_limit() {
        assert!(opaque_within_limit(&serde_json::json!({ "sig": "abc" })));
        let big = "x".repeat(MAX_OPAQUE_BYTES);
        assert!(!opaque_within_limit(&serde_json::json!({ "sig": big })));
    }
}
