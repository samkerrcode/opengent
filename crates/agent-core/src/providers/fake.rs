//! Deterministic in-process provider used to exercise the streaming pipeline end to end
//! without network access. It emits the prompt back in small chunks with a configurable delay.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{Capabilities, CompletionRequest, ModelProvider, ProviderEvent, PROVIDER_EVENT_BUFFER};
use crate::domain::{ContentBlock, Role, StopReason, Usage};
use crate::Result;

#[derive(Debug, Clone)]
pub struct FakeProvider {
    pub chunk_delay: Duration,
    pub chunk_chars: usize,
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self {
            chunk_delay: Duration::from_millis(12),
            chunk_chars: 3,
        }
    }
}

impl FakeProvider {
    fn compose_reply(request: &CompletionRequest) -> String {
        let last_user = request.messages.iter().rev().find(|m| m.role == Role::User);
        let text = last_user
            .and_then(|m| {
                m.blocks.iter().find_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
            })
            .unwrap_or("");
        let image_count = last_user
            .map(|m| {
                m.blocks
                    .iter()
                    .filter(|b| matches!(b, ContentBlock::Image { .. }))
                    .count()
            })
            .unwrap_or(0);
        let images_note = if image_count > 0 {
            format!("\n\nI also received {image_count} image(s) with your message.")
        } else {
            String::new()
        };
        format!(
            "This is a simulated response from the `{}` model.\n\nYou wrote:\n\n> {}{}\n\n\
             The streaming pipeline, cancellation, and event ordering you see here are real; \
             only the text is synthetic. Configure a provider to talk to a live model.",
            request.model,
            text.replace('\n', "\n> "),
            images_note,
        )
    }
}

impl ModelProvider for FakeProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            images: true,
            ..Default::default()
        }
    }

    fn stream(
        &self,
        request: &CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ProviderEvent>> {
        let (tx, rx) = mpsc::channel(PROVIDER_EVENT_BUFFER);
        let reply = Self::compose_reply(request);
        let msg_count = request.messages.len();
        let delay = self.chunk_delay;
        let chunk_chars = self.chunk_chars.max(1);

        tokio::spawn(async move {
            let chars: Vec<char> = reply.chars().collect();
            let mut emitted = 0usize;
            for chunk in chars.chunks(chunk_chars) {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(delay) => {}
                }
                if tx
                    .send(ProviderEvent::TextDelta(chunk.iter().collect()))
                    .await
                    .is_err()
                {
                    return;
                }
                emitted += chunk.len();
            }
            let usage = Usage {
                input_tokens: Some((msg_count * 8) as u64),
                output_tokens: Some(emitted.div_ceil(4) as u64),
                ..Default::default()
            };
            let _ = tx.send(ProviderEvent::Usage(usage)).await;
            let _ = tx.send(ProviderEvent::Done(StopReason::EndTurn)).await;
        });
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Message;

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "fake-1".into(),
            system: None,
            messages: vec![Message::text(Role::User, "hello")],
            max_output_tokens: None,
            tools: Vec::new(),
        }
    }

    #[tokio::test]
    async fn streams_text_then_usage_then_done() {
        let provider = FakeProvider {
            chunk_delay: Duration::ZERO,
            chunk_chars: 7,
        };
        let mut rx = provider
            .stream(&request(), CancellationToken::new())
            .unwrap();
        let mut text = String::new();
        let mut saw_usage = false;
        let mut done = None;
        while let Some(ev) = rx.recv().await {
            match ev {
                ProviderEvent::TextDelta(t) => text.push_str(&t),
                ProviderEvent::ReasoningDelta(_)
                | ProviderEvent::ToolCall { .. }
                | ProviderEvent::Opaque { .. } => {}
                ProviderEvent::Usage(_) => saw_usage = true,
                ProviderEvent::Done(r) => done = Some(r),
                ProviderEvent::Error(e) => panic!("unexpected error {e}"),
            }
        }
        assert!(text.contains("> hello"));
        assert!(saw_usage);
        assert_eq!(done, Some(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn stops_when_cancelled() {
        let provider = FakeProvider {
            chunk_delay: Duration::from_millis(5),
            chunk_chars: 1,
        };
        let cancel = CancellationToken::new();
        let mut rx = provider.stream(&request(), cancel.clone()).unwrap();
        let first = rx.recv().await;
        assert!(matches!(first, Some(ProviderEvent::TextDelta(_))));
        cancel.cancel();
        let mut remaining = 0;
        while rx.recv().await.is_some() {
            remaining += 1;
        }
        assert!(
            remaining < 5,
            "provider kept streaming after cancel: {remaining} events"
        );
    }
}
