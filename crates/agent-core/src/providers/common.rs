//! Building blocks shared by HTTP provider adapters: transport/status error mapping, bounded
//! body reads, tool-call id hygiene, and the SSE pump that drives a streaming response. An
//! adapter supplies only its request body and a per-event handler.

use std::future::Future;
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::sse::{SseEvent, SseParser};
use super::ProviderEvent;
use crate::{Error, Result};

/// A hostile or buggy server could stream unbounded tool arguments; adapters cap them here.
pub const MAX_TOOL_ARG_CHARS: usize = 512_000;
/// Error bodies are only mined for a message; never buffer more than this.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
/// Model listings can be large (aggregators list hundreds of models) but not unbounded.
pub const MAX_LISTING_BYTES: usize = 16 * 1024 * 1024;

/// Spawns a provider stream task. The task stops as soon as `cancel` fires; an `Err` from
/// `fut` is delivered to the consumer as `ProviderEvent::Error`.
pub fn spawn_stream<F>(tx: mpsc::Sender<ProviderEvent>, cancel: CancellationToken, fut: F)
where
    F: Future<Output = Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        let result = tokio::select! {
            _ = cancel.cancelled() => return,
            r = fut => r,
        };
        if let Err(e) = result {
            let _ = tx.send(ProviderEvent::Error(e.to_string())).await;
        }
    });
}

/// Sends `req` within `connect_timeout` and fails on a non-success status with a readable,
/// bounded error message.
pub async fn send_checked(
    req: reqwest::RequestBuilder,
    base_url: &str,
    connect_timeout: Duration,
) -> Result<reqwest::Response> {
    let resp = tokio::time::timeout(connect_timeout, req.send())
        .await
        .map_err(|_| Error::Provider(format!("timed out connecting to {base_url}")))?
        .map_err(|e| map_transport(base_url, e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(status_error(resp).await);
    }
    Ok(resp)
}

/// Maps a non-success response to an error, reading at most a bounded prefix of its body.
pub async fn status_error(resp: reqwest::Response) -> Error {
    let status = resp.status();
    let body = read_body(resp, MAX_ERROR_BODY_BYTES, true)
        .await
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    map_status(status, &body)
}

/// Reads at most `max` bytes of a response body. Past the cap, `truncate` keeps the prefix
/// (for mining error text); otherwise the read fails instead of buffering without bound.
async fn read_body(mut resp: reqwest::Response, max: usize, truncate: bool) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| Error::Provider(format!("failed to read response: {e}")))?
    {
        let room = max - out.len();
        if chunk.len() > room {
            if truncate {
                out.extend_from_slice(&chunk[..room]);
                return Ok(out);
            }
            return Err(Error::Provider(format!(
                "response exceeded {} KiB",
                max / 1024
            )));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Bounded JSON body read for listing endpoints.
pub async fn read_json(resp: reqwest::Response, what: &str) -> Result<Value> {
    let bytes = read_body(resp, MAX_LISTING_BYTES, false).await?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::Provider(format!("unexpected {what} response: {e}")))
}

/// How an SSE pump ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEnd {
    /// The server closed the body; the adapter decides whether that was a clean finish.
    Eof,
    /// Cancelled, or the consumer dropped its receiver: stop without emitting anything.
    Abandoned,
}

/// Drives an SSE response: enforces the idle timeout, honours cancellation, feeds the
/// incremental parser, and forwards whatever `handle` pushes into its output buffer (in
/// order, with backpressure). `handle` is synchronous so adapters stay plain state machines.
pub async fn pump_sse<F>(
    mut resp: reqwest::Response,
    idle_timeout: Duration,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<ProviderEvent>,
    mut handle: F,
) -> Result<StreamEnd>
where
    F: FnMut(SseEvent, &mut Vec<ProviderEvent>) -> Result<()>,
{
    let mut parser = SseParser::new();
    let mut out = Vec::new();
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Ok(StreamEnd::Abandoned),
            c = tokio::time::timeout(idle_timeout, resp.chunk()) => match c {
                Err(_) => return Err(Error::Provider(format!("no data from provider for {idle_timeout:?}"))),
                Ok(Err(e)) => return Err(Error::Provider(format!("stream error: {e}"))),
                Ok(Ok(None)) => return Ok(StreamEnd::Eof),
                Ok(Ok(Some(bytes))) => bytes,
            },
        };
        for ev in parser
            .feed(&chunk)
            .map_err(|e| Error::Provider(e.to_string()))?
        {
            handle(ev, &mut out)?;
            for e in out.drain(..) {
                if tx.send(e).await.is_err() {
                    return Ok(StreamEnd::Abandoned);
                }
            }
        }
    }
}

/// A tool call assembled from a stream, before it is emitted.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PartialCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Gives every call a unique non-empty id: empty ids and duplicates get a generated one.
/// Approvals, UI cards and stored call↔result pairing all key on this id, so a model that
/// repeats or omits ids must not alias two different calls.
pub fn normalize_call_ids(calls: &mut [PartialCall]) {
    let mut seen = std::collections::HashSet::new();
    for (i, c) in calls.iter_mut().enumerate() {
        if c.id.is_empty() || !seen.insert(c.id.clone()) {
            c.id = format!("call_{i}_{}", uuid::Uuid::new_v4().simple());
            seen.insert(c.id.clone());
        }
    }
}

/// Walks the error source chain looking for a TLS certificate failure. These surface as
/// connect-phase errors in reqwest, so they must be distinguished from "server unreachable".
fn tls_cert_error(e: &reqwest::Error) -> bool {
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        if s.downcast_ref::<rustls::Error>().is_some() {
            return true;
        }
        // The platform verifier may wrap OS-specific errors (e.g. Security.framework codes)
        // rather than rustls::Error; fall back to the message.
        let msg = s.to_string().to_lowercase();
        if msg.contains("certificate") || msg.contains("self signed") || msg.contains("self-signed")
        {
            return true;
        }
        src = s.source();
    }
    false
}

pub fn map_transport(base_url: &str, e: reqwest::Error) -> Error {
    if tls_cert_error(&e) {
        Error::Provider(format!(
            "TLS certificate verification failed for {base_url} — the server presents a \
             self-signed or untrusted certificate. Trust its certificate in the OS, paste it as \
             a custom CA in provider settings, or set TLS verification to 'skip verification'."
        ))
    } else if e.is_connect() {
        Error::Provider(format!(
            "could not connect to {base_url} — is the server running?"
        ))
    } else if e.is_timeout() {
        Error::Provider(format!("request to {base_url} timed out"))
    } else {
        // reqwest's Display includes the URL; strip it so query strings never reach logs.
        Error::Provider(format!("request failed: {}", e.without_url()))
    }
}

pub fn map_status(status: StatusCode, body: &str) -> Error {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let detail = parsed
        .as_ref()
        .and_then(extract_error_message)
        .unwrap_or_else(|| body.chars().take(300).collect::<String>().trim().to_owned());
    let detail: String = detail.chars().take(500).collect();
    // Google APIs report a bad key as HTTP 400 with an `API_KEY_INVALID` reason, and missing
    // credentials as `UNAUTHENTICATED`/`PERMISSION_DENIED` statuses.
    let google_auth = body.contains("API_KEY_INVALID")
        || matches!(
            parsed
                .as_ref()
                .and_then(|v| v.pointer("/error/status"))
                .and_then(Value::as_str),
            Some("UNAUTHENTICATED" | "PERMISSION_DENIED")
        );
    let hint = match status {
        _ if google_auth => "authentication failed — check the API key",
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            "authentication failed — check the API key"
        }
        StatusCode::NOT_FOUND => "endpoint or model not found — check the base URL and model name",
        StatusCode::TOO_MANY_REQUESTS => "rate limited by the provider",
        s if s.is_server_error() => "provider server error",
        _ => "provider rejected the request",
    };
    if detail.is_empty() {
        Error::Provider(format!("{hint} (HTTP {})", status.as_u16()))
    } else {
        Error::Provider(format!("{hint} (HTTP {}): {detail}", status.as_u16()))
    }
}

pub fn extract_error_message(v: &Value) -> Option<String> {
    v.get("error")
        .and_then(|e| e.get("message").or(Some(e)))
        .or_else(|| v.get("message"))
        .and_then(|m| m.as_str().map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_and_empty_call_ids_are_replaced() {
        let call = |id: &str, name: &str| PartialCall {
            id: id.into(),
            name: name.into(),
            arguments: String::new(),
        };
        let mut calls = vec![call("same", "a"), call("same", "b"), call("", "c")];
        normalize_call_ids(&mut calls);
        let ids: Vec<_> = calls.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids[0], "same");
        assert!(ids.iter().all(|i| !i.is_empty()));
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            3
        );
    }

    #[test]
    fn status_mapping_extracts_message() {
        let e = map_status(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"bad key","type":"auth"}}"#,
        );
        assert!(e.to_string().contains("bad key"));
        assert!(e.to_string().contains("401"));
    }

    #[test]
    fn google_invalid_key_maps_to_auth_hint() {
        let e = map_status(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"code":400,"message":"API key not valid.","status":"INVALID_ARGUMENT","details":[{"reason":"API_KEY_INVALID"}]}}"#,
        )
        .to_string();
        assert!(
            e.contains("authentication failed") && e.contains("API key not valid"),
            "{e}"
        );
        let e = map_status(
            StatusCode::FORBIDDEN,
            r#"{"error":{"message":"denied","status":"PERMISSION_DENIED"}}"#,
        );
        assert!(e.to_string().contains("authentication failed"));
        let e = map_status(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"bad field"}}"#,
        );
        assert!(e.to_string().contains("rejected the request"));
    }

    #[test]
    fn status_mapping_bounds_detail() {
        let body = format!(r#"{{"error":{{"message":"{}"}}}}"#, "x".repeat(10_000));
        let e = map_status(StatusCode::BAD_REQUEST, &body).to_string();
        assert!(e.len() < 700, "error text is bounded: {}", e.len());
    }
}
