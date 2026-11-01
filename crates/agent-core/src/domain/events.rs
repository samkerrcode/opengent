use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{ChatId, RunId, ToolCallInfo, ToolCallStatus};

/// Bump when the wire shape of `RunEvent` changes incompatibly.
pub const RUN_EVENT_SCHEMA_VERSION: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunPhase {
    Queued,
    Preparing,
    Generating,
    AwaitingApproval,
    CallingTools,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Usage {
    #[ts(type = "number | null")]
    pub input_tokens: Option<u64>,
    #[ts(type = "number | null")]
    pub output_tokens: Option<u64>,
    #[ts(type = "number | null")]
    pub cache_read_tokens: Option<u64>,
    #[ts(type = "number | null")]
    pub cache_write_tokens: Option<u64>,
}

/// Ordered, versioned event streamed from a run to the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunEvent {
    pub schema: u16,
    pub chat_id: ChatId,
    pub run_id: RunId,
    /// Monotonic per run, starting at 0. Lets the UI detect gaps and request a snapshot.
    #[ts(type = "number")]
    pub seq: u64,
    pub payload: RunEventPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum RunEventPayload {
    Phase {
        phase: RunPhase,
    },
    /// Coalesced text; one event may contain several network chunks.
    TextDelta {
        text: String,
    },
    /// Coalesced reasoning ("thinking") content for models that expose it.
    ThinkingDelta {
        text: String,
    },
    Usage {
        usage: Usage,
    },
    /// Non-fatal, user-visible information about how the run was prepared (e.g. trimmed history).
    Notice {
        message: String,
    },
    /// The model failed before producing output and the run continues on a fallback model
    /// (agents with a configured fallback). `reason` is the primary model's error.
    ModelSwitched {
        provider_id: String,
        model: String,
        reason: String,
    },
    /// The model requested a tool call. Followed by `ToolStatus` and finally `ToolResult`.
    ToolCall {
        call: ToolCallInfo,
    },
    /// Non-terminal status change (awaiting approval, running).
    ToolStatus {
        call_id: String,
        status: ToolCallStatus,
    },
    ToolResult {
        call_id: String,
        #[ts(type = "unknown")]
        content: serde_json::Value,
        is_error: bool,
        status: ToolCallStatus,
    },
    Completed {
        stop_reason: StopReason,
    },
    Cancelled,
    Failed {
        message: String,
    },
}

impl RunEventPayload {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Cancelled | Self::Failed { .. }
        )
    }
}
