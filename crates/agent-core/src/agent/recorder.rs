//! `RunRecorder` wraps a `RunSink`, forwarding every event unchanged while persisting the
//! assistant message: periodic text checkpoints during streaming and a final status on
//! completion. Persistence is fire-and-forget on the database worker so it never delays the UI.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::domain::{RunEvent, RunEventPayload, Usage};
use crate::storage::chats::{checkpoint_assistant_text, finalize_assistant_message};
use crate::storage::{Db, MessageStatus};

use super::RunSink;

/// Minimum interval between partial-text checkpoints.
const CHECKPOINT_EVERY: Duration = Duration::from_millis(750);

struct Progress {
    text: String,
    thinking: String,
    usage: Option<Usage>,
    last_checkpoint: Instant,
    dirty: bool,
    finished: bool,
}

pub struct RunRecorder<S: RunSink> {
    inner: S,
    db: Db,
    message_id: String,
    progress: Mutex<Progress>,
}

impl<S: RunSink> RunRecorder<S> {
    pub fn new(inner: S, db: Db, message_id: String) -> Self {
        Self {
            inner,
            db,
            message_id,
            progress: Mutex::new(Progress {
                text: String::new(),
                thinking: String::new(),
                usage: None,
                last_checkpoint: Instant::now(),
                dirty: false,
                finished: false,
            }),
        }
    }

    fn persist(&self, event: &RunEventPayload) {
        let mut p = self.progress.lock().unwrap();
        if p.finished {
            return;
        }
        match event {
            RunEventPayload::TextDelta { text } => {
                p.text.push_str(text);
                self.maybe_checkpoint(&mut p);
            }
            RunEventPayload::ThinkingDelta { text } => {
                p.thinking.push_str(text);
                self.maybe_checkpoint(&mut p);
            }
            RunEventPayload::Usage { usage } => p.usage = Some(*usage),
            RunEventPayload::Completed { .. } => self.finish(&mut p, MessageStatus::Complete, None),
            RunEventPayload::Cancelled => self.finish(&mut p, MessageStatus::Cancelled, None),
            RunEventPayload::Failed { message } => {
                self.finish(&mut p, MessageStatus::Failed, Some(message.clone()))
            }
            RunEventPayload::Phase { .. } | RunEventPayload::Notice { .. } => {}
        }
    }

    fn maybe_checkpoint(&self, p: &mut Progress) {
        p.dirty = true;
        if p.last_checkpoint.elapsed() < CHECKPOINT_EVERY {
            return;
        }
        p.last_checkpoint = Instant::now();
        p.dirty = false;
        let (id, text, thinking) = (self.message_id.clone(), p.text.clone(), p.thinking.clone());
        self.db
            .spawn_job(move |conn| checkpoint_assistant_text(conn, &id, &text, &thinking));
    }

    fn finish(&self, p: &mut Progress, status: MessageStatus, error: Option<String>) {
        p.finished = true;
        let (id, text, thinking, usage) = (
            self.message_id.clone(),
            p.text.clone(),
            p.thinking.clone(),
            p.usage,
        );
        self.db.spawn_job(move |conn| {
            finalize_assistant_message(conn, &id, &text, &thinking, status, usage, error.as_deref())
        });
    }
}

impl<S: RunSink> RunSink for RunRecorder<S> {
    fn send(&self, event: RunEvent) -> bool {
        self.persist(&event.payload);
        self.inner.send(event)
    }
}

impl<S: RunSink> Drop for RunRecorder<S> {
    /// If the run task ends without a terminal event (should not happen), do not leave the row
    /// in `streaming`: a later startup recovery would mark it interrupted anyway, but do it now.
    fn drop(&mut self) {
        let p = self.progress.get_mut().unwrap();
        if !p.finished {
            let (id, text, thinking, usage) = (
                self.message_id.clone(),
                p.text.clone(),
                p.thinking.clone(),
                p.usage,
            );
            self.db.spawn_job(move |conn| {
                finalize_assistant_message(
                    conn,
                    &id,
                    &text,
                    &thinking,
                    MessageStatus::Interrupted,
                    usage,
                    None,
                )
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChatId, ContentBlock, Role, RunId, StopReason, RUN_EVENT_SCHEMA_VERSION};
    use crate::storage::chats::{begin_assistant_message, create_chat, get_chat};
    use tokio::sync::mpsc;

    fn ev(payload: RunEventPayload, seq: u64) -> RunEvent {
        RunEvent {
            schema: RUN_EVENT_SCHEMA_VERSION,
            chat_id: ChatId::new(),
            run_id: RunId::new(),
            seq,
            payload,
        }
    }

    #[tokio::test]
    async fn forwards_events_and_finalizes_message() {
        let db = Db::in_memory().unwrap();
        let chat = db.call(|c| create_chat(c, "t")).await.unwrap();
        let run_id = RunId::new();
        let mid = db
            .call(move |c| begin_assistant_message(c, chat.id, run_id, "p", "m"))
            .await
            .unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let rec = RunRecorder::new(tx, db.clone(), mid.clone());
        rec.send(ev(RunEventPayload::TextDelta { text: "Hel".into() }, 0));
        rec.send(ev(RunEventPayload::TextDelta { text: "lo".into() }, 1));
        rec.send(ev(
            RunEventPayload::Usage {
                usage: Usage {
                    output_tokens: Some(2),
                    ..Default::default()
                },
            },
            2,
        ));
        rec.send(ev(
            RunEventPayload::Completed {
                stop_reason: StopReason::EndTurn,
            },
            3,
        ));
        assert_eq!(rx.recv().await.unwrap().seq, 0);
        drop(rec);

        let detail = db.call(move |c| get_chat(c, chat.id)).await.unwrap();
        let m = &detail.messages[0];
        assert_eq!(m.status, MessageStatus::Complete);
        assert_eq!(
            m.blocks,
            vec![ContentBlock::Text {
                text: "Hello".into()
            }]
        );
        assert_eq!(m.usage.unwrap().output_tokens, Some(2));
        assert_eq!(m.role, Role::Assistant);
    }

    #[tokio::test]
    async fn failure_records_error_and_partial_text() {
        let db = Db::in_memory().unwrap();
        let chat = db.call(|c| create_chat(c, "t")).await.unwrap();
        let mid = db
            .call(move |c| begin_assistant_message(c, chat.id, RunId::new(), "p", "m"))
            .await
            .unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let rec = RunRecorder::new(tx, db.clone(), mid);
        rec.send(ev(
            RunEventPayload::TextDelta {
                text: "part".into(),
            },
            0,
        ));
        rec.send(ev(
            RunEventPayload::Failed {
                message: "boom".into(),
            },
            1,
        ));
        drop(rec);
        let m = db
            .call(move |c| get_chat(c, chat.id))
            .await
            .unwrap()
            .messages
            .remove(0);
        assert_eq!(m.status, MessageStatus::Failed);
        assert_eq!(m.error.as_deref(), Some("boom"));
        assert_eq!(
            m.blocks,
            vec![ContentBlock::Text {
                text: "part".into()
            }]
        );
    }
}
