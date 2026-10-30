use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::runtime::Handle;
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::domain::{ChatId, RunEvent, RunEventPayload, RunId, RunPhase};
use crate::providers::{CompletionRequest, ModelProvider, ProviderEvent};
use crate::{Error, Result};

use super::{EmitterConfig, RunEmitter};

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
    /// Surfaced to the user before generation starts (e.g. "older messages were trimmed").
    pub notices: Vec<String>,
}

struct ActiveRun {
    run_id: RunId,
    cancel: CancellationToken,
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
        let cancel = CancellationToken::new();
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
                },
            );
        }

        let this = Arc::clone(self);
        let emitter = RunEmitter::new(sink, request.chat_id, run_id, self.emitter_config);
        self.runtime.spawn(async move {
            let chat_id = request.chat_id;
            run_loop(request, emitter, cancel, Arc::clone(&this.slots)).await;
            this.active
                .lock()
                .expect("coordinator lock poisoned")
                .remove(&chat_id);
        });
        Ok(run_id)
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

async fn run_loop<S: RunSink>(
    request: RunRequest,
    mut emitter: RunEmitter<S>,
    cancel: CancellationToken,
    slots: Arc<Semaphore>,
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
    let mut rx = match request.provider.stream(request.completion, cancel.clone()) {
        Ok(rx) => rx,
        Err(e) => {
            emitter.emit(RunEventPayload::Failed {
                message: e.to_string(),
            });
            return;
        }
    };
    emitter.emit(RunEventPayload::Phase {
        phase: RunPhase::Generating,
    });

    loop {
        let flush_at = emitter.flush_deadline();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                emitter.emit(RunEventPayload::Cancelled);
                return;
            }
            _ = async { match flush_at { Some(t) => tokio::time::sleep_until(t).await, None => std::future::pending().await } } => {
                emitter.flush();
            }
            ev = rx.recv() => match ev {
                Some(ProviderEvent::TextDelta(text)) => emitter.push_text(&text),
                Some(ProviderEvent::Usage(usage)) => emitter.emit(RunEventPayload::Usage { usage }),
                Some(ProviderEvent::Done(stop_reason)) => {
                    emitter.emit(RunEventPayload::Phase { phase: RunPhase::Completed });
                    emitter.emit(RunEventPayload::Completed { stop_reason });
                    return;
                }
                Some(ProviderEvent::Error(message)) => {
                    emitter.emit(RunEventPayload::Phase { phase: RunPhase::Failed });
                    emitter.emit(RunEventPayload::Failed { message });
                    return;
                }
                None => {
                    emitter.emit(RunEventPayload::Failed { message: "provider stream ended unexpectedly".into() });
                    return;
                }
            },
        }
        if emitter.is_disconnected() {
            cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::domain::{Message, Role, StopReason};
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
            },
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
