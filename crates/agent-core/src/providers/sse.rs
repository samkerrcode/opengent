//! Incremental Server-Sent Events parser. Fed arbitrary byte chunks (which may split lines,
//! events, or UTF-8 sequences), yields complete events. Comments and unknown fields are ignored.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    current: SseEvent,
    has_data: bool,
    /// Bytes buffered for the in-progress event; protects against unbounded growth.
    pub max_event_bytes: usize,
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            max_event_bytes: 4 * 1024 * 1024,
            ..Default::default()
        }
    }

    /// Appends `chunk` and returns every event completed by it.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        self.buf.extend_from_slice(chunk);
        if self.buf.len() > self.max_event_bytes {
            return Err(SseError::TooLarge);
        }
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let mut line = &line[..line.len() - 1];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            let line = std::str::from_utf8(line).map_err(|_| SseError::InvalidUtf8)?;
            if let Some(ev) = self.handle_line(line) {
                out.push(ev);
            }
        }
        Ok(out)
    }

    fn handle_line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            if !self.has_data && self.current.event.is_none() {
                return None;
            }
            self.has_data = false;
            let mut ev = std::mem::take(&mut self.current);
            // Spec: strip the single trailing newline added after each data line.
            if ev.data.ends_with('\n') {
                ev.data.pop();
            }
            return Some(ev);
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "data" => {
                self.current.data.push_str(value);
                self.current.data.push('\n');
                self.has_data = true;
            }
            "event" => self.current.event = Some(value.to_owned()),
            _ => {} // id, retry, unknown fields: ignored
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SseError {
    #[error("SSE event exceeded size limit")]
    TooLarge,
    #[error("SSE stream contained invalid UTF-8")]
    InvalidUtf8,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(input: &[u8], split: usize) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        let mut out = Vec::new();
        for chunk in input.chunks(split.max(1)) {
            out.extend(p.feed(chunk).unwrap());
        }
        out
    }

    const STREAM: &[u8] = b": keep-alive\n\
        event: message\ndata: {\"a\":1}\n\n\
        data: line one\ndata: line two\n\n\
        data:no-space\r\n\r\n\
        data: [DONE]\n\n";

    #[test]
    fn parses_events_identically_at_every_split_size() {
        let expected = vec![
            SseEvent {
                event: Some("message".into()),
                data: "{\"a\":1}".into(),
            },
            SseEvent {
                event: None,
                data: "line one\nline two".into(),
            },
            SseEvent {
                event: None,
                data: "no-space".into(),
            },
            SseEvent {
                event: None,
                data: "[DONE]".into(),
            },
        ];
        for split in 1..=STREAM.len() {
            assert_eq!(parse_all(STREAM, split), expected, "split={split}");
        }
    }

    #[test]
    fn multibyte_utf8_split_across_chunks() {
        let input = "data: héllo → 世界\n\n".as_bytes();
        for split in 1..=input.len() {
            let evs = parse_all(input, split);
            assert_eq!(evs.len(), 1);
            assert_eq!(evs[0].data, "héllo → 世界");
        }
    }

    #[test]
    fn rejects_oversized_event() {
        let mut p = SseParser::new();
        p.max_event_bytes = 16;
        assert_eq!(
            p.feed(b"data: 0123456789012345678901234567890"),
            Err(SseError::TooLarge)
        );
    }

    #[test]
    fn blank_lines_without_data_produce_nothing() {
        assert!(parse_all(b"\n\n\n: c\n\n", 3).is_empty());
    }
}
