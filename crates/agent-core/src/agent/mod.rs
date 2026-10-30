//! Agent loop orchestration. Phase 0 ships the run state machine skeleton with real
//! streaming, coalescing, cancellation and per-chat exclusivity; tool calls, approvals and
//! context compilation are layered on in later phases.

pub mod context;
mod coordinator;
mod emitter;
mod recorder;

pub use coordinator::{RunCoordinator, RunRequest, RunSink};
pub use emitter::{EmitterConfig, RunEmitter};
pub use recorder::RunRecorder;
