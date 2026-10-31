//! Agent loop orchestration: the run state machine with real streaming, coalescing,
//! cancellation and per-chat exclusivity; tool calls loop model → policy → approval →
//! tool → result → model for as many steps as the model needs.

pub mod context;
mod coordinator;
mod emitter;
mod recorder;
pub mod tools;

pub use coordinator::{RunCoordinator, RunRequest, RunSink};
pub use emitter::{EmitterConfig, RunEmitter};
pub use recorder::RunRecorder;
pub use tools::{
    cap_result, cap_result_with, definitions_tokens, resolve_on_demand, RunTools, ToolEntry,
    ToolFuture, ToolHost, ToolLoading, ToolNamer, ToolOutput, ToolSettings, FIND_TOOLS,
};
