use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::domain::{
    ApprovalDecision, ChatId, ContentBlock, Message, Role, RunEvent, RunEventPayload, RunId,
    RunPhase, StopReason, ToolCallInfo, ToolCallStatus, ToolPolicy,
};
use crate::providers::{CompletionRequest, ModelProvider, ProviderEvent};
use crate::{Error, Result};

use super::tools::{cap_result_with, RunTools, ToolOutput, FIND_TOOLS};
use super::{EmitterConfig, RunEmitter, ToolHost};

/// Maximum model↔tool round trips in one run; beyond that the run stops with a notice.
const MAX_TOOL_STEPS: usize = 8;
/// Maximum tool calls executed from a single model turn; extra calls get an error result.
const MAX_CALLS_PER_STEP: usize = 16;
/// How long a tool call may wait for the user's approval before it is auto-denied. Bounds
/// the run slot a stalled prompt holds (e.g. when the webview is closed mid-approval).
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// While waiting for approval, re-check that the UI is still listening this often.
const APPROVAL_LIVENESS_CHECK: Duration = Duration::from_secs(15);
/// Result returned to the model for calls to a user-disabled tool.
const DISABLED_TOOL_MSG: &str = "This tool is disabled by the user and cannot be called. \
    Tell the user it is disabled (they can re-enable it in the Tools menu) instead of \
    working around it with other tools.";

/// Destination for run events. The host implements this over its IPC channel.
/// Returns `false` when the receiver is gone so the run can stop emitting.
pub trait RunSink: Send + 'static {
    fn send(&self, event: RunEvent) -> bool;
}

impl RunSink for mpsc::UnboundedSender<RunEvent> {
    fn send(&self, event: RunEvent) -> bool {
        mpsc::UnboundedSender::send(self, event).is_ok()
    }
}

pub struct RunRequest {
    pub chat_id: ChatId,
    pub provider: Arc<dyn ModelProvider>,
    pub completion: CompletionRequest,
    /// Tools offered this run; `None` disables tool calling entirely.
    pub tools: Option<RunTools>,
    /// Surfaced to the user before generation starts (e.g. "older messages were trimmed").
    pub notices: Vec<String>,
}

struct PendingApproval {
    tx: oneshot::Sender<ApprovalDecision>,
    /// Needed to persist an `AlwaysAllow` decision as a policy change.
    host: Arc<dyn ToolHost>,
    server_id: String,
    tool: String,
}

type Approvals = Arc<Mutex<HashMap<String, PendingApproval>>>;

struct ActiveRun {
    run_id: RunId,
    cancel: CancellationToken,
    /// Tool calls paused on user approval, keyed by tool call id.
    approvals: Approvals,
}

/// Owns all in-flight runs. Guarantees at most one active run per chat and bounds the total
/// number of concurrent runs. No lock is held across awaits.
pub struct RunCoordinator {
    active: Mutex<HashMap<ChatId, ActiveRun>>,
    slots: Arc<Semaphore>,
    emitter_config: EmitterConfig,
    /// Runs are spawned on this runtime explicitly so `start` may be called from any thread
    /// (e.g. a host UI thread with no ambient Tokio context).
    runtime: Handle,
}

impl RunCoordinator {
    pub fn new(max_concurrent_runs: usize, runtime: Handle) -> Arc<Self> {
        Arc::new(Self {
            active: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(max_concurrent_runs.max(1))),
            emitter_config: EmitterConfig::default(),
            runtime,
        })
    }

    pub fn start<S: RunSink>(self: &Arc<Self>, request: RunRequest, sink: S) -> Result<RunId> {
        let run_id = RunId::new();
        self.start_with_id(run_id, request, sink)?;
        Ok(run_id)
    }

    /// Like `start`, but with a caller-chosen run id (so the host can persist it before the
    /// first event is emitted).
    pub fn start_with_id<S: RunSink>(
        self: &Arc<Self>,
        run_id: RunId,
        request: RunRequest,
        sink: S,
    ) -> Result<()> {
        let cancel = CancellationToken::new();
        let approvals: Approvals = Arc::new(Mutex::new(HashMap::new()));
        {
            let mut active = self.active.lock().expect("coordinator lock poisoned");
            if active.contains_key(&request.chat_id) {
                return Err(Error::RunBusy);
            }
            active.insert(
                request.chat_id,
                ActiveRun {
                    run_id,
                    cancel: cancel.clone(),
                    approvals: approvals.clone(),
                },
            );
        }

        let this = Arc::clone(self);
        let emitter = RunEmitter::new(sink, request.chat_id, run_id, self.emitter_config);
        self.runtime.spawn(async move {
            let chat_id = request.chat_id;
            run_loop(request, emitter, cancel, Arc::clone(&this.slots), approvals).await;
            this.active
                .lock()
                .expect("coordinator lock poisoned")
                .remove(&chat_id);
        });
        Ok(())
    }

    /// Requests cancellation. Returns `false` if the run is unknown or already finished.
    pub fn cancel(&self, run_id: RunId) -> bool {
        let active = self.active.lock().expect("coordinator lock poisoned");
        match active.values().find(|r| r.run_id == run_id) {
            Some(run) => {
                run.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Resolves a tool call waiting on user approval. Returns `false` when no matching
    /// approval is pending (unknown id, already answered, or run finished). An
    /// `AlwaysAllow` decision also writes the tool's policy before the call proceeds.
    pub fn resolve_approval(
        &self,
        run_id: RunId,
        call_id: &str,
        decision: ApprovalDecision,
    ) -> bool {
        let approval = {
            let active = self.active.lock().expect("coordinator lock poisoned");
            active
                .values()
                .find(|r| r.run_id == run_id)
                .and_then(|r| r.approvals.lock().expect("approvals lock").remove(call_id))
        };
        match approval {
            Some(a) => {
                if decision == ApprovalDecision::AlwaysAllow {
                    a.host
                        .set_policy(&a.server_id, &a.tool, ToolPolicy::AlwaysAllow);
                }
                a.tx.send(decision).is_ok()
            }
            None => false,
        }
    }

    pub fn active_run(&self, chat_id: ChatId) -> Option<RunId> {
        self.active
            .lock()
            .expect("coordinator lock poisoned")
            .get(&chat_id)
            .map(|r| r.run_id)
    }

    pub fn active_count(&self) -> usize {
        self.active.lock().expect("coordinator lock poisoned").len()
    }
}

/// One generation step of the agent loop.
struct Step {
    /// Text streamed this step (may be empty for a pure tool turn).
    text: String,
    calls: Vec<ToolCallInfo>,
    stop_reason: StopReason,
}

/// Streams a single completion. Returns `None` once a terminal payload has been emitted
/// (cancel/failure), `Some(step)` when the model finished the turn — possibly with tool calls.
async fn stream_step<S: RunSink>(
    provider: &Arc<dyn ModelProvider>,
    completion: &CompletionRequest,
    emitter: &mut RunEmitter<S>,
    cancel: &CancellationToken,
) -> Option<Step> {
    let mut rx = match provider.stream(completion, cancel.clone()) {
        Ok(rx) => rx,
        Err(e) => {
            emitter.emit(RunEventPayload::Failed {
                message: e.to_string(),
            });
            return None;
        }
    };
    emitter.emit(RunEventPayload::Phase {
        phase: RunPhase::Generating,
    });

    let mut step = Step {
        text: String::new(),
        calls: Vec::new(),
        stop_reason: StopReason::EndTurn,
    };
    loop {
        let flush_at = emitter.flush_deadline();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                emitter.emit(RunEventPayload::Cancelled);
                return None;
            }
            _ = async { match flush_at { Some(t) => tokio::time::sleep_until(t).await, None => std::future::pending().await } } => {
                emitter.flush();
            }
            ev = rx.recv() => match ev {
                Some(ProviderEvent::TextDelta(t)) => {
                    step.text.push_str(&t);
                    emitter.push_text(&t);
                }
                Some(ProviderEvent::ReasoningDelta(text)) => emitter.push_thinking(&text),
                Some(ProviderEvent::Usage(usage)) => emitter.emit(RunEventPayload::Usage { usage }),
                Some(ProviderEvent::ToolCall { id, name, arguments }) => {
                    let arguments = serde_json::from_str::<Value>(&arguments)
                        .unwrap_or(Value::String(arguments));
                    step.calls.push(ToolCallInfo {
                        id,
                        name: name.clone(),
                        server: None,
                        tool: name,
                        arguments,
                    });
                }
                Some(ProviderEvent::Done(stop_reason)) => {
                    step.stop_reason = stop_reason;
                    return Some(step);
                }
                Some(ProviderEvent::Error(message)) => {
                    emitter.emit(RunEventPayload::Phase { phase: RunPhase::Failed });
                    emitter.emit(RunEventPayload::Failed { message });
                    return None;
                }
                None => {
                    emitter.emit(RunEventPayload::Failed { message: "provider stream ended unexpectedly".into() });
                    return None;
                }
            },
        }
        if emitter.is_disconnected() {
            cancel.cancel();
        }
    }
}

/// How one tool call ends. Every variant emits `ToolResult`; `Abort` additionally ends the run.
enum CallEnd {
    Emit {
        content: Value,
        is_error: bool,
        status: ToolCallStatus,
    },
    Abort,
}

async fn run_loop<S: RunSink>(
    request: RunRequest,
    mut emitter: RunEmitter<S>,
    cancel: CancellationToken,
    slots: Arc<Semaphore>,
    approvals: Approvals,
) {
    emitter.emit(RunEventPayload::Phase {
        phase: RunPhase::Queued,
    });
    let _permit = tokio::select! {
        _ = cancel.cancelled() => {
            emitter.emit(RunEventPayload::Cancelled);
            return;
        }
        permit = slots.acquire_owned() => match permit {
            Ok(p) => p,
            Err(_) => {
                emitter.emit(RunEventPayload::Failed { message: "run scheduler is shut down".into() });
                return;
            }
        },
    };

    emitter.emit(RunEventPayload::Phase {
        phase: RunPhase::Preparing,
    });
    for message in request.notices {
        emitter.emit(RunEventPayload::Notice { message });
    }

    let provider = request.provider;
    let mut tools = request.tools;
    let mut completion = request.completion;
    let mut tool_steps = 0usize;

    loop {
        // Re-derive the offered definitions each step: on-demand loading adds tools as
        // `find_tools` loads them, and a "always allow" decision can unblock a tool mid-run.
        completion.tools = tools.as_ref().map(|t| t.specs()).unwrap_or_default();

        let Some(step) = stream_step(&provider, &completion, &mut emitter, &cancel).await else {
            return;
        };
        if step.calls.is_empty() || tools.is_none() {
            emitter.emit(RunEventPayload::Phase {
                phase: RunPhase::Completed,
            });
            emitter.emit(RunEventPayload::Completed {
                stop_reason: step.stop_reason,
            });
            return;
        }
        tool_steps += 1;
        if tool_steps > MAX_TOOL_STEPS {
            emitter.emit(RunEventPayload::Notice {
                message: format!(
                    "Stopped after {MAX_TOOL_STEPS} tool steps; run the prompt again to continue."
                ),
            });
            emitter.emit(RunEventPayload::Phase {
                phase: RunPhase::Completed,
            });
            emitter.emit(RunEventPayload::Completed {
                stop_reason: StopReason::EndTurn,
            });
            return;
        }

        // Persist the assistant step (text + calls) in the working transcript.
        let mut blocks = Vec::new();
        if !step.text.trim().is_empty() {
            blocks.push(ContentBlock::Text { text: step.text });
        }
        blocks.extend(step.calls.iter().map(|c| ContentBlock::ToolCall {
            id: c.id.clone(),
            name: c.name.clone(),
            arguments: c.arguments.clone(),
            server: c.server.clone(),
            tool: Some(c.tool.clone()),
        }));
        completion.messages.push(Message {
            role: Role::Assistant,
            blocks,
        });

        // Execute the calls in order; each emits ToolCall → (statuses) → ToolResult.
        let mut results = Vec::new();
        let mut first_execute = true;
        for (idx, mut call) in step.calls.into_iter().enumerate() {
            // Fill display routing (server name, original tool name) from the catalog.
            if let Some(e) = tools.as_ref().and_then(|t| t.entry(&call.name)) {
                call.server = Some(e.server_name.clone());
                call.tool = e.tool.clone();
            }
            emitter.emit(RunEventPayload::ToolCall { call: call.clone() });
            if idx >= MAX_CALLS_PER_STEP {
                let content = serde_json::json!([{ "type": "text", "text": format!(
                    "Too many tool calls in one step (max {MAX_CALLS_PER_STEP}); this call was not executed."
                )}]);
                results.push(ContentBlock::ToolResult {
                    call_id: call.id.clone(),
                    content: content.clone(),
                    is_error: true,
                    status: Some(ToolCallStatus::Failed),
                });
                emitter.emit(RunEventPayload::ToolResult {
                    call_id: call.id,
                    content,
                    is_error: true,
                    status: ToolCallStatus::Failed,
                });
                continue;
            }
            let end = execute_call(
                &call,
                &mut tools,
                &approvals,
                &mut emitter,
                &cancel,
                &mut first_execute,
            )
            .await;
            match end {
                CallEnd::Abort => {
                    emitter.emit(RunEventPayload::ToolResult {
                        call_id: call.id,
                        content: Value::Array(Vec::new()),
                        is_error: true,
                        status: ToolCallStatus::Cancelled,
                    });
                    emitter.emit(RunEventPayload::Cancelled);
                    return;
                }
                CallEnd::Emit {
                    content,
                    is_error,
                    status,
                } => {
                    results.push(ContentBlock::ToolResult {
                        call_id: call.id.clone(),
                        content: content.clone(),
                        is_error,
                        status: Some(status),
                    });
                    emitter.emit(RunEventPayload::ToolResult {
                        call_id: call.id,
                        content,
                        is_error,
                        status,
                    });
                }
            }
            if emitter.is_disconnected() {
                cancel.cancel();
            }
        }
        completion.messages.push(Message {
            role: Role::Tool,
            blocks: results,
        });
    }
}

/// Executes one model tool call: resolve it against the run's catalog, enforce policy,
/// wait for approval when required, then invoke the host. Emits `ToolStatus` transitions.
async fn execute_call<S: RunSink>(
    call: &ToolCallInfo,
    tools: &mut Option<RunTools>,
    approvals: &Approvals,
    emitter: &mut RunEmitter<S>,
    cancel: &CancellationToken,
    first_execute: &mut bool,
) -> CallEnd {
    macro_rules! emit_now {
        ($status:expr) => {
            emitter.emit(RunEventPayload::ToolStatus {
                call_id: call.id.clone(),
                status: $status,
            })
        };
    }
    // Arguments must be a JSON object. Anything else (malformed text, a bare scalar) must
    // not reach the tool: it would be called with silently-empty arguments, possibly after
    // the user approved a different-looking call.
    let parsed = match &call.arguments {
        Value::Null => Value::Object(serde_json::Map::new()),
        Value::Object(_) => call.arguments.clone(),
        Value::String(s) if s.trim().is_empty() => Value::Object(serde_json::Map::new()),
        _ => {
            return CallEnd::Emit {
                content: serde_json::json!([{ "type": "text", "text": format!(
                    "Tool `{}` was called with malformed arguments (expected a JSON object).",
                    call.name
                )}]),
                is_error: true,
                status: ToolCallStatus::Failed,
            };
        }
    };

    // The built-in search tool is read-only metadata: it never needs approval.
    if call.name == FIND_TOOLS {
        emit_now!(ToolCallStatus::Running);
        let out = match tools {
            Some(t) if t.is_on_demand() => t.find_tools(&parsed),
            _ => ToolOutput::text(
                "Tool search is not active; all tools are already available.",
                true,
            ),
        };
        return CallEnd::Emit {
            content: cap_result_with(
                out.content,
                tools.as_ref().map(|t| t.result_cap).unwrap_or(usize::MAX),
            ),
            is_error: out.is_error,
            status: if out.is_error {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            },
        };
    }

    let Some(run_tools) = tools.as_ref() else {
        return CallEnd::Emit {
            content: Value::Array(Vec::new()),
            is_error: true,
            status: ToolCallStatus::Failed,
        };
    };
    let Some(entry) = run_tools.entry(&call.name) else {
        // Check disabled first: in on-demand mode a disabled tool must not send the model
        // looking for it with `find_tools` (which can never return it).
        let (msg, status) = if run_tools.is_disabled(&call.name) {
            (DISABLED_TOOL_MSG.to_owned(), ToolCallStatus::Blocked)
        } else if run_tools.is_on_demand() {
            (
                format!(
                    "Tool `{}` is not loaded. Call `{FIND_TOOLS}` to find and load tools first.",
                    call.name
                ),
                ToolCallStatus::Failed,
            )
        } else {
            (
                format!("Unknown tool `{}`.", call.name),
                ToolCallStatus::Failed,
            )
        };
        return CallEnd::Emit {
            content: serde_json::json!([{ "type": "text", "text": msg }]),
            is_error: true,
            status,
        };
    };
    let result_cap = run_tools.result_cap;
    let host = run_tools.host.clone();
    let (server_id, tool_name) = (entry.server_id.clone(), entry.tool.clone());
    match host.policy(&server_id, &tool_name) {
        ToolPolicy::Block => {
            return CallEnd::Emit {
                content: serde_json::json!([{ "type": "text", "text": DISABLED_TOOL_MSG }]),
                is_error: true,
                status: ToolCallStatus::Blocked,
            };
        }
        ToolPolicy::Ask => {
            emitter.emit(RunEventPayload::Phase {
                phase: RunPhase::AwaitingApproval,
            });
            emit_now!(ToolCallStatus::PendingApproval);
            if emitter.is_disconnected() {
                cancel.cancel();
                return CallEnd::Abort;
            }
            let (tx, mut rx) = oneshot::channel::<ApprovalDecision>();
            approvals.lock().expect("approvals lock").insert(
                call.id.clone(),
                PendingApproval {
                    tx,
                    host: host.clone(),
                    server_id: server_id.clone(),
                    tool: tool_name.clone(),
                },
            );
            enum Waited {
                Decision(ApprovalDecision),
                TimedOut,
                Gone,
            }
            let deadline = tokio::time::Instant::now() + APPROVAL_TIMEOUT;
            let waited = loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        approvals.lock().expect("approvals lock").remove(&call.id);
                        return CallEnd::Abort;
                    }
                    d = &mut rx => break match d {
                        Ok(d) => Waited::Decision(d),
                        Err(_) => Waited::Gone,
                    },
                    _ = tokio::time::sleep_until(deadline) => break Waited::TimedOut,
                    _ = tokio::time::sleep(APPROVAL_LIVENESS_CHECK) => {
                        if emitter.is_disconnected() {
                            cancel.cancel();
                            approvals.lock().expect("approvals lock").remove(&call.id);
                            return CallEnd::Abort;
                        }
                    }
                }
            };
            let decision = match waited {
                Waited::Decision(d) => d,
                Waited::TimedOut => {
                    approvals.lock().expect("approvals lock").remove(&call.id);
                    emitter.emit(RunEventPayload::Notice {
                        message: format!(
                            "Tool call `{}` was denied: approval timed out.",
                            call.name
                        ),
                    });
                    return CallEnd::Emit {
                        content: serde_json::json!([{ "type": "text", "text": "The user did not respond to the approval prompt in time; this call was denied." }]),
                        is_error: true,
                        status: ToolCallStatus::Denied,
                    };
                }
                // The approval entry vanished without an answer (run torn down).
                Waited::Gone => return CallEnd::Abort,
            };
            match decision {
                ApprovalDecision::Deny => {
                    return CallEnd::Emit {
                        content: serde_json::json!([{ "type": "text", "text": "The user denied this tool call." }]),
                        is_error: true,
                        status: ToolCallStatus::Denied,
                    };
                }
                ApprovalDecision::AllowOnce | ApprovalDecision::AlwaysAllow => {}
            }
        }
        ToolPolicy::AlwaysAllow => {}
    }

    if *first_execute {
        *first_execute = false;
        emitter.emit(RunEventPayload::Phase {
            phase: RunPhase::CallingTools,
        });
    }
    emit_now!(ToolCallStatus::Running);
    let out = tokio::select! {
        biased;
        _ = cancel.cancelled() => return CallEnd::Abort,
        out = host.call(&server_id, &tool_name, parsed, cancel.clone()) => out,
    };
    CallEnd::Emit {
        content: cap_result_with(out.content, result_cap),
        is_error: out.is_error,
        status: if out.is_error {
            ToolCallStatus::Failed
        } else {
            ToolCallStatus::Completed
        },
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::domain::Message;
    use crate::providers::fake::FakeProvider;

    fn request(chat_id: ChatId, delay: Duration) -> RunRequest {
        RunRequest {
            chat_id,
            provider: Arc::new(FakeProvider {
                chunk_delay: delay,
                chunk_chars: 2,
            }),
            completion: CompletionRequest {
                model: "fake".into(),
                system: None,
                messages: vec![Message::text(Role::User, "ping")],
                max_output_tokens: None,
                tools: Vec::new(),
            },
            tools: None,
            notices: Vec::new(),
        }
    }

    async fn collect(mut rx: mpsc::UnboundedReceiver<RunEvent>) -> Vec<RunEvent> {
        let mut out = Vec::new();
        while let Some(e) = rx.recv().await {
            out.push(e);
        }
        out
    }

    #[tokio::test]
    async fn completes_with_ordered_events_and_frees_slot() {
        let coord = RunCoordinator::new(2, Handle::current());
        let (tx, rx) = mpsc::unbounded_channel();
        let chat = ChatId::new();
        coord.start(request(chat, Duration::ZERO), tx).unwrap();
        let events = collect(rx).await;
        assert!(events.windows(2).all(|w| w[0].seq + 1 == w[1].seq));
        assert_eq!(
            events.first().unwrap().payload,
            RunEventPayload::Phase {
                phase: RunPhase::Queued
            }
        );
        assert_eq!(
            events.last().unwrap().payload,
            RunEventPayload::Completed {
                stop_reason: StopReason::EndTurn
            }
        );
        let text: String = events
            .iter()
            .filter_map(|e| match &e.payload {
                RunEventPayload::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains("> ping"));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(coord.active_count(), 0);
    }

    #[tokio::test]
    async fn rejects_second_run_on_same_chat_and_cancels() {
        let coord = RunCoordinator::new(2, Handle::current());
        let chat = ChatId::new();
        let (tx, rx) = mpsc::unbounded_channel();
        let run_id = coord
            .start(request(chat, Duration::from_millis(20)), tx)
            .unwrap();
        let (tx2, _rx2) = mpsc::unbounded_channel();
        assert!(matches!(
            coord.start(request(chat, Duration::ZERO), tx2),
            Err(Error::RunBusy)
        ));
        assert_eq!(coord.active_run(chat), Some(run_id));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(coord.cancel(run_id));
        let events = collect(rx).await;
        assert_eq!(events.last().unwrap().payload, RunEventPayload::Cancelled);
        assert!(
            !coord.cancel(run_id),
            "cancel after completion should report false"
        );
    }

    #[test]
    fn start_works_from_a_thread_without_runtime_context() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let coord = RunCoordinator::new(1, rt.handle().clone());
        let (tx, mut rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            coord
                .start(request(ChatId::new(), Duration::ZERO), tx)
                .unwrap()
        })
        .join()
        .unwrap();
        let last = rt.block_on(async {
            let mut last = None;
            while let Some(e) = rx.recv().await {
                last = Some(e);
            }
            last
        });
        assert_eq!(
            last.unwrap().payload,
            RunEventPayload::Completed {
                stop_reason: StopReason::EndTurn
            }
        );
    }

    #[tokio::test]
    async fn malformed_arguments_never_reach_the_tool() {
        use crate::agent::tools::tests as tool_tests;
        let mut namer = crate::agent::tools::ToolNamer::new();
        let catalog = vec![tool_tests::entry("fs", "read", "Read", &mut namer)];
        let mut tools = Some(RunTools::new(
            Arc::new(tool_tests::NoopHost),
            catalog,
            false,
            5,
            &[],
            8192,
        ));
        let approvals: Approvals = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut emitter =
            RunEmitter::new(tx, ChatId::new(), RunId::new(), EmitterConfig::default());
        let cancel = CancellationToken::new();
        let mut first = true;
        let call = ToolCallInfo {
            id: "c1".into(),
            name: "fs__read".into(),
            server: Some("fs".into()),
            tool: "read".into(),
            arguments: Value::String("this is not json".into()),
        };
        // A non-object argument blob must fail before policy/approval/host dispatch —
        // otherwise the tool would run with silently-empty arguments.
        let end = execute_call(
            &call,
            &mut tools,
            &approvals,
            &mut emitter,
            &cancel,
            &mut first,
        )
        .await;
        assert!(matches!(
            end,
            CallEnd::Emit {
                is_error: true,
                status: ToolCallStatus::Failed,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn disabled_tools_are_reported_as_disabled_not_unknown() {
        use crate::agent::tools::tests as tool_tests;
        let mut namer = crate::agent::tools::ToolNamer::new();
        let catalog = vec![tool_tests::entry("fs", "read", "Read", &mut namer)];
        // On-demand: the "not loaded, call find_tools" hint must not apply to disabled tools.
        let mut tools = Some(
            RunTools::new(Arc::new(tool_tests::NoopHost), catalog, true, 5, &[], 8192)
                .with_disabled(["fs__delete".to_owned()]),
        );
        let approvals: Approvals = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut emitter =
            RunEmitter::new(tx, ChatId::new(), RunId::new(), EmitterConfig::default());
        let cancel = CancellationToken::new();
        let mut first = true;
        let call = |name: &str| ToolCallInfo {
            id: "c1".into(),
            name: name.into(),
            server: None,
            tool: String::new(),
            arguments: Value::Null,
        };
        let disabled = call("fs__delete");
        match execute_call(
            &disabled,
            &mut tools,
            &approvals,
            &mut emitter,
            &cancel,
            &mut first,
        )
        .await
        {
            CallEnd::Emit {
                status: ToolCallStatus::Blocked,
                content,
                ..
            } => assert!(content.to_string().contains("disabled by the user")),
            _ => panic!("disabled tool must be Blocked"),
        }
        // A real but not-yet-loaded tool gets the find_tools hint as a plain failure.
        let unloaded = call("fs__read");
        match execute_call(
            &unloaded,
            &mut tools,
            &approvals,
            &mut emitter,
            &cancel,
            &mut first,
        )
        .await
        {
            CallEnd::Emit {
                status: ToolCallStatus::Failed,
                content,
                ..
            } => assert!(content.to_string().contains("find_tools")),
            _ => panic!("unloaded tool must fail with a find_tools hint"),
        }
    }

    #[tokio::test]
    async fn dropped_sink_cancels_run() {
        let coord = RunCoordinator::new(1, Handle::current());
        let (tx, rx) = mpsc::unbounded_channel();
        coord
            .start(request(ChatId::new(), Duration::from_millis(5)), tx)
            .unwrap();
        drop(rx);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(coord.active_count(), 0);
    }
}
