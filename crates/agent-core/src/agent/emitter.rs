use std::time::Duration;

use tokio::time::Instant;

use crate::domain::{
    ChatId, ContentBlock, RunEvent, RunEventPayload, RunId, RUN_EVENT_SCHEMA_VERSION,
};

use super::RunSink;

#[derive(Debug, Clone, Copy)]
pub struct EmitterConfig {
    /// Maximum time a text delta waits in the buffer before being flushed to the UI.
    pub flush_interval: Duration,
    /// Flush immediately once buffered text reaches this many bytes.
    pub flush_bytes: usize,
}

impl Default for EmitterConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_millis(30),
            flush_bytes: 2048,
        }
    }
}

/// Assigns monotonic sequence numbers and coalesces text deltas so the UI receives a bounded
/// number of updates per second regardless of how finely the provider chunks its output.
pub struct RunEmitter<S: RunSink> {
    sink: S,
    chat_id: ChatId,
    run_id: RunId,
    seq: u64,
    text_buf: String,
    thinking_buf: String,
    buffer_since: Option<Instant>,
    config: EmitterConfig,
    /// Set once the sink reports the receiver is gone; further sends are dropped.
    disconnected: bool,
}

impl<S: RunSink> RunEmitter<S> {
    pub fn new(sink: S, chat_id: ChatId, run_id: RunId, config: EmitterConfig) -> Self {
        Self {
            sink,
            chat_id,
            run_id,
            seq: 0,
            text_buf: String::new(),
            thinking_buf: String::new(),
            buffer_since: None,
            config,
            disconnected: false,
        }
    }

    pub fn is_disconnected(&self) -> bool {
        self.disconnected
    }

    pub fn push_text(&mut self, text: &str) {
        if self.text_buf.is_empty() && self.thinking_buf.is_empty() {
            self.buffer_since = Some(Instant::now());
        }
        self.text_buf.push_str(text);
        if self.text_buf.len() >= self.config.flush_bytes {
            self.flush();
        }
    }

    pub fn push_thinking(&mut self, text: &str) {
        if self.text_buf.is_empty() && self.thinking_buf.is_empty() {
            self.buffer_since = Some(Instant::now());
        }
        self.thinking_buf.push_str(text);
        if self.thinking_buf.len() >= self.config.flush_bytes {
            self.flush();
        }
    }

    /// Deadline at which buffered deltas must be flushed, if any are buffered.
    pub fn flush_deadline(&self) -> Option<Instant> {
        self.buffer_since.map(|t| t + self.config.flush_interval)
    }

    pub fn flush(&mut self) {
        if self.text_buf.is_empty() && self.thinking_buf.is_empty() {
            return;
        }
        let thinking = std::mem::take(&mut self.thinking_buf);
        let text = std::mem::take(&mut self.text_buf);
        self.buffer_since = None;
        // Reasoning precedes the answer in stream order; emit it first.
        if !thinking.is_empty() {
            self.send_now(RunEventPayload::ThinkingDelta { text: thinking });
        }
        if !text.is_empty() {
            self.send_now(RunEventPayload::TextDelta { text });
        }
    }

    /// Flushes pending text first so ordering is preserved, then sends `payload`.
    pub fn emit(&mut self, payload: RunEventPayload) {
        self.flush();
        self.send_now(payload);
    }

    /// Hands a persist-only block to the sink at its stream position (buffered text is
    /// flushed first). Takes no sequence number: it is not a UI event.
    pub fn record(&mut self, block: &ContentBlock) {
        self.flush();
        self.sink.record(block);
    }

    fn send_now(&mut self, payload: RunEventPayload) {
        if self.disconnected {
            return;
        }
        let event = RunEvent {
            schema: RUN_EVENT_SCHEMA_VERSION,
            chat_id: self.chat_id,
            run_id: self.run_id,
            seq: self.seq,
            payload,
        };
        self.seq += 1;
        if !self.sink.send(event) {
            self.disconnected = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct VecSink(Arc<Mutex<Vec<RunEvent>>>);
    impl RunSink for VecSink {
        fn send(&self, event: RunEvent) -> bool {
            self.0.lock().unwrap().push(event);
            true
        }
    }

    #[tokio::test]
    async fn coalesces_text_and_keeps_sequence_monotonic() {
        let sink = VecSink::default();
        let mut em = RunEmitter::new(
            sink.clone(),
            ChatId::new(),
            RunId::new(),
            EmitterConfig::default(),
        );
        em.push_text("a");
        em.push_text("b");
        em.push_text("c");
        em.emit(RunEventPayload::Completed {
            stop_reason: crate::domain::StopReason::EndTurn,
        });
        let events = sink.0.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].payload,
            RunEventPayload::TextDelta { text: "abc".into() }
        );
        assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![0, 1]);
    }

    #[tokio::test]
    async fn flushes_on_byte_threshold() {
        let sink = VecSink::default();
        let cfg = EmitterConfig {
            flush_interval: Duration::from_secs(10),
            flush_bytes: 4,
        };
        let mut em = RunEmitter::new(sink.clone(), ChatId::new(), RunId::new(), cfg);
        em.push_text("abc");
        assert_eq!(sink.0.lock().unwrap().len(), 0);
        em.push_text("d");
        assert_eq!(sink.0.lock().unwrap().len(), 1);
    }
}
