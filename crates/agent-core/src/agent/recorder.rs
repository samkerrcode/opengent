//! `RunRecorder` wraps a `RunSink`, forwarding every event unchanged while persisting the
//! assistant message: periodic checkpoints during streaming and a final status on completion.
//! Content is kept as ordered blocks — text, thinking, tool calls and results interleaved as
//! they happened — so a multi-step run reloads exactly as it streamed.
//! Persistence is fire-and-forget on the database worker so it never delays the UI.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::domain::{ContentBlock, RunEvent, RunEventPayload, Usage};
use crate::storage::chats::{
    checkpoint_assistant_blocks, finalize_assistant_message, set_message_model,
};
use crate::storage::{Db, MessageStatus};

use super::RunSink;

/// Minimum interval between partial-content checkpoints.
const CHECKPOINT_EVERY: Duration = Duration::from_millis(750);

struct Progress {
    blocks: Vec<ContentBlock>,
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

fn append_block(blocks: &mut Vec<ContentBlock>, text: &str, thinking: bool) {
    let matches_tail = |b: &&mut ContentBlock| match b {
        ContentBlock::Thinking { .. } => thinking,
        ContentBlock::Text { .. } => !thinking,
        _ => false,
    };
    if let Some(last) = blocks.last_mut().filter(matches_tail) {
        match last {
            ContentBlock::Text { text: buf } | ContentBlock::Thinking { text: buf } => {
                buf.push_str(text)
            }
            _ => unreachable!(),
        }
    } else if thinking {
        blocks.push(ContentBlock::Thinking {
            text: text.to_owned(),
        });
    } else {
        blocks.push(ContentBlock::Text {
            text: text.to_owned(),
        });
    }
}

impl<S: RunSink> RunRecorder<S> {
    pub fn new(inner: S, db: Db, message_id: String) -> Self {
        Self {
            inner,
            db,
            message_id,
            progress: Mutex::new(Progress {
                blocks: Vec::new(),
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
                append_block(&mut p.blocks, text, false);
                self.maybe_checkpoint(&mut p, false);
            }
            RunEventPayload::ThinkingDelta { text } => {
                append_block(&mut p.blocks, text, true);
                self.maybe_checkpoint(&mut p, false);
            }
            // Tool events are rare and a call can then sit in `pending approval` for a
            // long time: checkpoint immediately so a crash doesn't lose them.
            RunEventPayload::ToolCall { call } => {
                p.blocks.push(ContentBlock::ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                    server: call.server.clone(),
                    tool: Some(call.tool.clone()),
                });
                self.maybe_checkpoint(&mut p, true);
            }
            RunEventPayload::ToolResult {
                call_id,
                content,
                is_error,
                status,
            } => {
                p.blocks.push(ContentBlock::ToolResult {
                    call_id: call_id.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                    status: Some(*status),
                });
                self.maybe_checkpoint(&mut p, true);
            }
            RunEventPayload::Usage { usage } => p.usage = Some(*usage),
            // Attribute the message to the model that actually answered.
            RunEventPayload::ModelSwitched { model, .. } => {
                let (id, model) = (self.message_id.clone(), model.clone());
                self.db
                    .spawn_job(move |conn| set_message_model(conn, &id, &model));
            }
            RunEventPayload::Completed { .. } => self.finish(&mut p, MessageStatus::Complete, None),
            RunEventPayload::Cancelled => self.finish(&mut p, MessageStatus::Cancelled, None),
            RunEventPayload::Failed { message } => {
                self.finish(&mut p, MessageStatus::Failed, Some(message.clone()))
            }
            RunEventPayload::Phase { .. }
            | RunEventPayload::Notice { .. }
            | RunEventPayload::ToolStatus { .. } => {}
        }
    }

    fn maybe_checkpoint(&self, p: &mut Progress, force: bool) {
        p.dirty = true;
        if !force && p.last_checkpoint.elapsed() < CHECKPOINT_EVERY {
            return;
        }
        p.last_checkpoint = Instant::now();
        p.dirty = false;
        let (id, blocks) = (self.message_id.clone(), p.blocks.clone());
        self.db
            .spawn_job(move |conn| checkpoint_assistant_blocks(conn, &id, &blocks));
    }

    fn finish(&self, p: &mut Progress, status: MessageStatus, error: Option<String>) {
        p.finished = true;
        let (id, blocks, usage) = (self.message_id.clone(), p.blocks.clone(), p.usage);
        self.db.spawn_job(move |conn| {
            finalize_assistant_message(conn, &id, &blocks, status, usage, error.as_deref())
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
            let (id, blocks, usage) = (self.message_id.clone(), p.blocks.clone(), p.usage);
            self.db.spawn_job(move |conn| {
                finalize_assistant_message(
                    conn,
                    &id,
                    &blocks,
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
    use crate::domain::{
        ChatId, Role, RunId, StopReason, ToolCallInfo, ToolCallStatus, RUN_EVENT_SCHEMA_VERSION,
    };
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

    #[tokio::test]
    async fn tool_calls_and_results_persist_in_order() {
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
                text: "first".into(),
            },
            0,
        ));
        rec.send(ev(
            RunEventPayload::ToolCall {
                call: ToolCallInfo {
                    id: "c1".into(),
                    name: "fs__list".into(),
                    server: Some("fs".into()),
                    tool: "list".into(),
                    arguments: serde_json::json!({ "dir": "/" }),
                },
            },
            1,
        ));
        rec.send(ev(
            RunEventPayload::ToolResult {
                call_id: "c1".into(),
                content: serde_json::json!([{ "type": "text", "text": "a" }]),
                is_error: false,
                status: ToolCallStatus::Completed,
            },
            2,
        ));
        rec.send(ev(
            RunEventPayload::TextDelta {
                text: "last".into(),
            },
            3,
        ));
        rec.send(ev(
            RunEventPayload::Completed {
                stop_reason: StopReason::EndTurn,
            },
            4,
        ));
        drop(rec);
        let m = db
            .call(move |c| get_chat(c, chat.id))
            .await
            .unwrap()
            .messages
            .remove(0);
        assert_eq!(m.status, MessageStatus::Complete);
        assert_eq!(m.blocks.len(), 4);
        assert!(matches!(&m.blocks[1], ContentBlock::ToolCall { id, .. } if id == "c1"));
        assert!(matches!(
            &m.blocks[2],
            ContentBlock::ToolResult {
                status: Some(ToolCallStatus::Completed),
                ..
            }
        ));
        assert!(matches!(&m.blocks[3], ContentBlock::Text { text } if text == "last"));
    }
}
