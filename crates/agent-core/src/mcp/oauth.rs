//! OAuth for remote MCP servers on top of rmcp's `auth` module: keychain-backed token
//! storage, the RFC 8252 loopback redirect listener used by browser sign-in, and mapping
//! of rmcp/transport errors to short, actionable messages.
//!
//! rmcp owns the protocol (discovery, DCR, PKCE, state/`iss` checks, refresh, client
//! credentials); this module only supplies storage, the callback and the wording.

use std::error::Error as StdError;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmcp::service::ClientInitializeError;
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, CredentialRefreshGuard, CredentialStore, StoredCredentials,
};
use rmcp::transport::streamable_http_client::{
    AuthRequiredError, InsufficientScopeError, StreamableHttpError,
};
use rmcp::ServiceError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use super::config::{oauth_ref, McpAuth};
use crate::secrets::SecretStore;
use crate::{Error, Result};

/// Keychain key holding the chunk count of the token blob; chunks are `credentials.<n>`.
const TOKENS: &str = "credentials";
/// Windows Credential Manager caps a credential blob at 2560 bytes (and may store
/// UTF-16), so the JSON token blob is split into chunks of at most this many bytes.
const CHUNK_BYTES: usize = 1_000;
/// Upper bound on chunks (≈64 KB of tokens); larger blobs are refused.
const MAX_CHUNKS: usize = 64;

fn chunk_ref(server_id: &str, n: usize) -> Result<crate::secrets::SecretRef> {
    oauth_ref(server_id, &format!("{TOKENS}.{n}"))
}

fn chunk_count(secrets: &dyn SecretStore, server_id: &str) -> Result<usize> {
    Ok(secrets
        .get(&oauth_ref(server_id, TOKENS)?)?
        .and_then(|c| c.trim().parse::<usize>().ok())
        .map_or(0, |c| c.min(MAX_CHUNKS)))
}

/// Splits on char boundaries so each chunk is valid UTF-8 of at most `CHUNK_BYTES`.
fn chunks(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    while !rest.is_empty() {
        let mut end = rest.len().min(CHUNK_BYTES);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (head, tail) = rest.split_at(end);
        out.push(head);
        rest = tail;
    }
    out
}

/// The stored token blob (JSON `StoredCredentials`), or `None` if absent or incomplete.
pub(crate) fn read_tokens(secrets: &dyn SecretStore, server_id: &str) -> Result<Option<String>> {
    let count = chunk_count(secrets, server_id)?;
    if count == 0 {
        return Ok(None);
    }
    let mut blob = String::new();
    for n in 0..count {
        match secrets.get(&chunk_ref(server_id, n)?)? {
            Some(part) => blob.push_str(&part),
            None => {
                tracing::warn!("stored OAuth tokens are incomplete; ignoring them");
                return Ok(None);
            }
        }
    }
    Ok(Some(blob))
}

pub(crate) fn write_tokens(secrets: &dyn SecretStore, server_id: &str, blob: &str) -> Result<()> {
    let parts = chunks(blob);
    if parts.len() > MAX_CHUNKS {
        return Err(Error::InvalidInput(
            "OAuth token response is too large".into(),
        ));
    }
    let old = chunk_count(secrets, server_id)?;
    // Write the parts before the count so a reader never sees a count with missing parts
    // beyond the old ones; stale trailing parts are removed afterwards.
    for (n, part) in parts.iter().enumerate() {
        secrets.set(&chunk_ref(server_id, n)?, part)?;
    }
    secrets.set(&oauth_ref(server_id, TOKENS)?, &parts.len().to_string())?;
    for n in parts.len()..old {
        let _ = secrets.delete(&chunk_ref(server_id, n)?);
    }
    Ok(())
}

pub(crate) fn clear_tokens(secrets: &dyn SecretStore, server_id: &str) -> Result<()> {
    let count = chunk_count(secrets, server_id)?;
    secrets.delete(&oauth_ref(server_id, TOKENS)?)?;
    for n in 0..count.max(1) {
        let _ = secrets.delete(&chunk_ref(server_id, n)?);
    }
    Ok(())
}

/// rmcp credential store over the app's `SecretStore` (OS keychain). Shares a per-server
/// refresh lock so concurrent refreshes of the same tokens serialise.
pub(crate) struct KeychainCredentialStore {
    secrets: Arc<dyn SecretStore>,
    server_id: String,
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
}

impl KeychainCredentialStore {
    pub(crate) fn new(
        secrets: Arc<dyn SecretStore>,
        server_id: &str,
        refresh_lock: Arc<tokio::sync::Mutex<()>>,
    ) -> Self {
        Self {
            secrets,
            server_id: server_id.to_owned(),
            refresh_lock,
        }
    }

    pub(crate) fn load_sync(&self) -> Result<Option<StoredCredentials>> {
        // Chunks aren't written atomically: an interrupted write reads as signed out.
        Ok(
            read_tokens(self.secrets.as_ref(), &self.server_id)?.and_then(|blob| {
                serde_json::from_str(&blob)
                    .inspect_err(|_| {
                        tracing::warn!("stored OAuth tokens are corrupt; ignoring them")
                    })
                    .ok()
            }),
        )
    }
}

fn store_error(e: Error) -> AuthError {
    AuthError::CredentialStoreError(e.to_string())
}

#[async_trait::async_trait]
impl CredentialStore for KeychainCredentialStore {
    async fn load(&self) -> std::result::Result<Option<StoredCredentials>, AuthError> {
        self.load_sync().map_err(store_error)
    }

    async fn save(&self, credentials: StoredCredentials) -> std::result::Result<(), AuthError> {
        let blob = serde_json::to_string(&credentials)
            .map_err(|e| AuthError::CredentialStoreError(e.to_string()))?;
        write_tokens(self.secrets.as_ref(), &self.server_id, &blob).map_err(store_error)
    }

    async fn clear(&self) -> std::result::Result<(), AuthError> {
        clear_tokens(self.secrets.as_ref(), &self.server_id).map_err(store_error)
    }

    async fn acquire_refresh_guard(
        &self,
    ) -> std::result::Result<Option<CredentialRefreshGuard>, AuthError> {
        let guard = Arc::clone(&self.refresh_lock).lock_owned().await;
        Ok(Some(CredentialRefreshGuard::new(guard)))
    }
}

/// An rmcp authorization manager for `url` that uses the app's HTTP client (redirect-free,
/// platform-verified TLS) and keeps credentials in the keychain.
pub(crate) async fn build_manager(
    url: &str,
    http: reqwest::Client,
    store: KeychainCredentialStore,
) -> std::result::Result<AuthorizationManager, AuthError> {
    let mut manager = AuthorizationManager::new(url).await?;
    manager.with_client(http)?;
    manager.set_credential_store(store);
    Ok(manager)
}

/// Time left on a stored access token, when the token response carried `expires_in`.
pub(crate) fn token_remaining(c: &StoredCredentials) -> Option<Duration> {
    let v = serde_json::to_value(c.token_response.as_ref()?).ok()?;
    let expires_in = v.get("expires_in")?.as_u64()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let elapsed = now.saturating_sub(c.token_received_at?);
    Some(Duration::from_secs(expires_in.saturating_sub(elapsed)))
}

// ---------------------------------------------------------------------------------------
// Loopback redirect (RFC 8252 §7.3)

const MAX_REQUEST_BYTES: usize = 8 * 1024;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONNECTIONS: usize = 16;
const CALLBACK_PATH: &str = "/callback";

const PAGE_DONE: &str = "<!doctype html><meta charset=utf-8><title>Signed in</title>\
<body style=\"font:16px system-ui,sans-serif;margin:3em\"><h1>Sign-in complete</h1>\
<p>You can close this tab and return to the app.</p>";
const PAGE_FAILED: &str = "<!doctype html><meta charset=utf-8><title>Sign-in failed</title>\
<body style=\"font:16px system-ui,sans-serif;margin:3em\"><h1>Sign-in didn't complete</h1>\
<p>You can close this tab and return to the app for details.</p>";

/// A temporary `127.0.0.1` listener receiving the authorization redirect.
pub(crate) struct LoopbackCallback {
    listener: TcpListener,
    port: u16,
}

impl LoopbackCallback {
    /// Binds `127.0.0.1:<port>` (`None` = a free ephemeral port).
    pub(crate) async fn bind(port: Option<u16>) -> std::result::Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
            .await
            .map_err(|e| match port {
                Some(p) => format!("couldn't listen on callback port {p}: {e}"),
                None => format!("couldn't open a local callback port: {e}"),
            })?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("couldn't open a local callback port: {e}"))?
            .port();
        Ok(Self { listener, port })
    }

    pub(crate) fn redirect_uri(&self) -> String {
        redirect_uri(self.port)
    }

    /// Serves connections until a `GET /callback` carrying `expected_state` arrives, and
    /// returns its full URL — or the authorization server's `error=` as a message. Other
    /// requests (wrong path, foreign state, oversized) are answered and ignored, so a
    /// stray local request can't end the sign-in. Connections are served concurrently
    /// (bounded), so an idle one — browsers open speculative connections — can't hold up
    /// the real redirect or a cancel.
    pub(crate) async fn wait(
        self,
        expected_state: &str,
        cancel: &CancellationToken,
    ) -> std::result::Result<String, String> {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err("cancelled".into()),
                r = self.listener.accept() => match r {
                    Ok((stream, _)) if connections.len() < MAX_CONNECTIONS => {
                        let (port, state) = (self.port, expected_state.to_owned());
                        connections.spawn(async move {
                            tokio::time::timeout(
                                CONNECTION_TIMEOUT,
                                handle_connection(stream, port, &state),
                            )
                            .await
                            .ok()
                            .flatten()
                        });
                    }
                    Ok(_) => tracing::debug!("loopback connection limit reached"),
                    Err(e) => tracing::debug!(error = %e, "loopback accept failed"),
                },
                Some(handled) = connections.join_next() => {
                    if let Ok(Some(outcome)) = handled {
                        return outcome;
                    }
                }
            }
        }
    }
}

pub(crate) fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}{CALLBACK_PATH}")
}

enum Request {
    Callback(String),
    NotFound,
    Bad,
    TooLarge,
}

/// Parses the request line only (`GET <target> HTTP/1.x`); headers and body are ignored.
fn parse_request(head: &[u8]) -> Request {
    let Some(end) = head.windows(2).position(|w| w == b"\r\n") else {
        return if head.len() >= MAX_REQUEST_BYTES {
            Request::TooLarge
        } else {
            Request::Bad
        };
    };
    let Ok(line) = std::str::from_utf8(&head[..end]) else {
        return Request::Bad;
    };
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Request::Bad;
    };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Request::Bad;
    }
    let path = target.split('?').next().unwrap_or("");
    if method != "GET" || path != CALLBACK_PATH {
        return Request::NotFound;
    }
    Request::Callback(target.to_owned())
}

/// What a callback URL means: `Some(Ok(url))` on a code for our state, `Some(Err(msg))`
/// on an authorization error for our state, `None` when it isn't ours to act on.
fn callback_outcome(
    url: &reqwest::Url,
    expected_state: &str,
) -> Option<std::result::Result<String, String>> {
    let param = |k: &str| {
        url.query_pairs()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.into_owned())
    };
    if param("state").as_deref() != Some(expected_state) {
        return None;
    }
    if let Some(error) = param("error") {
        return Some(Err(authorization_error(
            &error,
            param("error_description").as_deref(),
        )));
    }
    param("code").map(|_| Ok(url.to_string()))
}

/// Readable text for an RFC 6749 §4.1.2.1 error. Server text is untrusted: the code is
/// restricted to its grammar and the description is cleaned and capped.
fn authorization_error(code: &str, description: Option<&str>) -> String {
    let code: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect();
    let base = match code.as_str() {
        "access_denied" => "access was denied".to_owned(),
        "invalid_scope" => "the requested scopes were rejected".to_owned(),
        "unauthorized_client" => "this client isn't allowed to sign in this way".to_owned(),
        "server_error" | "temporarily_unavailable" => {
            "the authorization server had a problem; try again".to_owned()
        }
        "" => "the authorization server returned an error".to_owned(),
        other => format!("the authorization server returned `{other}`"),
    };
    match description.map(clean).filter(|d| !d.is_empty()) {
        Some(d) => format!("Sign-in failed: {base} ({d})."),
        None => format!("Sign-in failed: {base}."),
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    port: u16,
    expected_state: &str,
) -> Option<std::result::Result<String, String>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(2).any(|w| w == b"\r\n") && buf.len() < MAX_REQUEST_BYTES {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n.min(MAX_REQUEST_BYTES - buf.len())]),
        }
    }
    let (status, page, outcome) = match parse_request(&buf) {
        Request::Callback(target) => {
            let outcome = reqwest::Url::parse(&format!("http://127.0.0.1:{port}{target}"))
                .ok()
                .and_then(|url| callback_outcome(&url, expected_state));
            match &outcome {
                Some(Ok(_)) => ("200 OK", PAGE_DONE, outcome),
                Some(Err(_)) => ("200 OK", PAGE_FAILED, outcome),
                None => ("400 Bad Request", PAGE_FAILED, None),
            }
        }
        Request::NotFound => ("404 Not Found", "", None),
        Request::Bad => ("400 Bad Request", "", None),
        Request::TooLarge => ("431 Request Header Fields Too Large", "", None),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n\
         Referrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
    outcome
}

/// The `state` parameter of an authorization URL (rmcp generates it; the callback must
/// echo it).
pub(crate) fn url_state(auth_url: &reqwest::Url) -> Option<String> {
    auth_url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
}

/// The browser may only be sent to `https`, or `http` on a loopback host (local servers).
pub(crate) fn browser_url_allowed(url: &reqwest::Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")),
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------
// Error classification

const MAX_MESSAGE: usize = 300;

/// Collapses whitespace, drops control characters and caps the length.
fn clean(s: &str) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = joined
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_MESSAGE)
        .collect();
    if joined.chars().count() > MAX_MESSAGE {
        out.push('…');
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthMode {
    None,
    User,
    ClientCredentials,
}

/// What the connection had when it failed, to word the message.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AuthContext {
    pub mode: AuthMode,
    pub has_headers: bool,
    /// OAuth tokens were stored when connecting (a 401 then means they expired).
    pub had_tokens: bool,
}

impl AuthContext {
    pub(crate) fn new(auth: &McpAuth, has_headers: bool, had_tokens: bool) -> Self {
        let mode = match auth {
            McpAuth::None => AuthMode::None,
            McpAuth::OauthUser { .. } => AuthMode::User,
            McpAuth::OauthClientCredentials { .. } => AuthMode::ClientCredentials,
        };
        Self {
            mode,
            has_headers,
            had_tokens,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Browser sign-in is needed; `reason` explains why when it isn't simply the first.
    NeedsAuth(Option<String>),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Classified {
    pub failure: Failure,
    /// The server's `WWW-Authenticate` challenge, to seed the next sign-in's discovery.
    /// Kept in memory only — never shown or logged.
    pub challenge: Option<String>,
}

impl Classified {
    pub(crate) fn failed(msg: impl Into<String>) -> Self {
        Self {
            failure: Failure::Failed(msg.into()),
            challenge: None,
        }
    }
}

enum Cause<'a> {
    Unauthorized(Option<String>),
    Forbidden,
    Scope(Option<String>),
    Dropped,
    Auth(&'a AuthError),
    Other(&'a (dyn StdError + 'static)),
}

/// Walks an error chain for the auth-relevant cause, else returns the innermost error.
/// `StreamableHttpError::Client` doesn't link its inner error as a `source`, so the walk
/// steps into it explicitly.
fn find_cause<'a>(root: &'a (dyn StdError + 'static)) -> Cause<'a> {
    let mut cur = root;
    loop {
        let mut next = cur.source();
        if let Some(e) = cur.downcast_ref::<StreamableHttpError<reqwest::Error>>() {
            match e {
                StreamableHttpError::AuthRequired(a) => {
                    return Cause::Unauthorized(Some(a.www_authenticate_header.clone()))
                }
                StreamableHttpError::InsufficientScope(s) => {
                    return Cause::Scope(s.required_scope.clone())
                }
                StreamableHttpError::Auth(a) => return Cause::Auth(a),
                StreamableHttpError::Client(r) => next = Some(r),
                StreamableHttpError::UnexpectedServerResponse(m) if m.starts_with("HTTP 401") => {
                    return Cause::Unauthorized(None)
                }
                StreamableHttpError::UnexpectedServerResponse(m) if m.starts_with("HTTP 403") => {
                    return Cause::Forbidden
                }
                StreamableHttpError::TransportChannelClosed => return Cause::Dropped,
                _ => {}
            }
        } else if let Some(a) = cur.downcast_ref::<AuthRequiredError>() {
            return Cause::Unauthorized(Some(a.www_authenticate_header.clone()));
        } else if let Some(s) = cur.downcast_ref::<InsufficientScopeError>() {
            return Cause::Scope(s.required_scope.clone());
        } else if let Some(a) = cur.downcast_ref::<AuthError>() {
            return Cause::Auth(a);
        } else if let Some(r) = cur.downcast_ref::<reqwest::Error>() {
            match r.status().map(|s| s.as_u16()) {
                Some(401) => return Cause::Unauthorized(None),
                Some(403) => return Cause::Forbidden,
                _ => {}
            }
        }
        match next {
            Some(n) => cur = n,
            None => return Cause::Other(cur),
        }
    }
}

fn unauthorized(ctx: AuthContext, challenge: Option<String>) -> Classified {
    let failure = match ctx.mode {
        AuthMode::None if ctx.has_headers => Failure::Failed(
            "The server rejected the credentials in this connection's headers.".into(),
        ),
        AuthMode::None => Failure::Failed(
            "This server requires authentication. Add an Authorization header, or choose an \
             OAuth method under Authentication."
                .into(),
        ),
        AuthMode::User => Failure::NeedsAuth(
            ctx.had_tokens
                .then(|| "Your sign-in expired — sign in again.".to_owned()),
        ),
        AuthMode::ClientCredentials => Failure::Failed(
            "The server rejected the client credentials. Check the client ID, secret and scopes."
                .into(),
        ),
    };
    Classified { failure, challenge }
}

fn scope_message(ctx: AuthContext, scope: Option<&str>) -> String {
    let then = match ctx.mode {
        AuthMode::User => "then sign in again",
        _ => "then reconnect",
    };
    match scope.map(clean).filter(|s| !s.is_empty()) {
        Some(s) => format!("The server needs the `{s}` scope. Add it under Scopes, {then}."),
        None => format!(
            "The server needs a scope this connection wasn't granted. Add it under Scopes, {then}."
        ),
    }
}

/// One-line message for an rmcp OAuth error.
pub(crate) fn classify_auth(e: &AuthError, ctx: AuthContext) -> Classified {
    let msg = match e {
        AuthError::AuthorizationRequired | AuthError::TokenRefreshRejected(_) => {
            return unauthorized(ctx, None)
        }
        AuthError::NoAuthorizationSupport => {
            "The server doesn't advertise OAuth support.".to_owned()
        }
        AuthError::MetadataError(m) => {
            format!("Couldn't read the server's OAuth metadata: {}", clean(m))
        }
        AuthError::RegistrationFailed(m) if m.contains("not supported") => {
            "The authorization server doesn't support automatic client registration — enter a \
             Client ID under Authentication."
                .to_owned()
        }
        AuthError::RegistrationFailed(m) => format!("Client registration failed: {}", clean(m)),
        AuthError::PkceUnsupported => {
            "The authorization server doesn't support PKCE (S256), which sign-in requires."
                .to_owned()
        }
        // Servers like GitHub answer a rejected exchange with 200 and an error
        // object; rmcp then reports only the deserialize failure ("missing field
        // `access_token`"), not the server's own error.
        AuthError::TokenExchangeFailed(m) if m.contains("missing field") => {
            "The authorization server rejected the token exchange — check the Client ID, \
             and add a Client secret if the server requires one."
                .to_owned()
        }
        AuthError::TokenExchangeFailed(m) => {
            format!(
                "The authorization server didn't issue a token: {}",
                clean(m)
            )
        }
        AuthError::TokenRefreshFailed(m) => format!("Couldn't refresh the sign-in: {}", clean(m)),
        AuthError::ClientCredentialsError(m)
            if ["invalid_client", "unauthorized_client", "401"]
                .iter()
                .any(|k| m.contains(k)) =>
        {
            return unauthorized(
                AuthContext {
                    mode: AuthMode::ClientCredentials,
                    ..ctx
                },
                None,
            )
        }
        AuthError::ClientCredentialsError(m) => format!("Client credentials failed: {}", clean(m)),
        AuthError::CredentialStoreError(_) => {
            "Couldn't access the saved sign-in in the system credential store.".to_owned()
        }
        AuthError::AuthorizationServerMismatch { .. }
        | AuthError::AuthorizationServerMissingIssuer { .. } => {
            "The authorization server's identity (issuer) didn't match its metadata.".to_owned()
        }
        AuthError::HttpError(r) => format!(
            "Couldn't reach the authorization server: {}",
            clean(&innermost(r).to_string())
        ),
        AuthError::InsufficientScope { required_scope, .. } => {
            scope_message(ctx, Some(required_scope))
        }
        AuthError::InvalidScope(s) => format!("The authorization server rejected `{}`.", clean(s)),
        other => format!("OAuth error: {}", clean(&other.to_string())),
    };
    Classified::failed(msg)
}

fn innermost<'a>(e: &'a (dyn StdError + 'static)) -> &'a (dyn StdError + 'static) {
    let mut cur = e;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur
}

fn classify_root(root: &(dyn StdError + 'static), ctx: AuthContext) -> Classified {
    match find_cause(root) {
        Cause::Unauthorized(challenge) => unauthorized(ctx, challenge),
        Cause::Forbidden => Classified::failed(match ctx.mode {
            AuthMode::None => "The server refused access (HTTP 403).",
            _ => "The server refused access (HTTP 403) — the account may lack permission.",
        }),
        Cause::Scope(scope) => Classified::failed(scope_message(ctx, scope.as_deref())),
        // rmcp swallows the HTTP error that killed the worker — name the likely cause.
        Cause::Dropped => Classified::failed(
            "The server closed the connection during setup — check the URL is a \
             Streamable HTTP MCP endpoint.",
        ),
        Cause::Auth(a) => classify_auth(a, ctx),
        Cause::Other(e) => Classified::failed(clean(&e.to_string())),
    }
}

/// The error chain for the debug log, without the `WWW-Authenticate` values carried by
/// the auth-required/insufficient-scope errors.
fn log_chain(e: &(dyn StdError + 'static)) -> String {
    let mut parts = Vec::new();
    let mut cur = Some(e);
    while let Some(e) = cur {
        parts.push(if e.is::<AuthRequiredError>() {
            "authorization required".to_owned()
        } else if e.is::<InsufficientScopeError>() {
            "insufficient scope".to_owned()
        } else {
            e.to_string()
        });
        cur = e.source();
    }
    parts.join(": ")
}

/// Classifies a failed MCP handshake. The chain goes to the debug log; neither it nor the
/// message includes tokens, secrets or the raw `WWW-Authenticate` value.
pub(crate) fn classify_init(e: &ClientInitializeError, ctx: AuthContext) -> Classified {
    tracing::debug!(error = %log_chain(e), "MCP initialize failed");
    match e {
        ClientInitializeError::TransportError { error, .. } => {
            classify_root(error.error.as_ref(), ctx)
        }
        ClientInitializeError::LegacyFallbackFailed { fallback, .. } => {
            classify_init(fallback, ctx)
        }
        other => Classified::failed(clean(&other.to_string())),
    }
}

/// Classifies a failed request on an established session (e.g. `tools/list`).
pub(crate) fn classify_service(e: &ServiceError, ctx: AuthContext) -> Classified {
    tracing::debug!(error = %log_chain(e), "MCP request failed");
    match e {
        ServiceError::TransportSend(d) => classify_root(d.error.as_ref(), ctx),
        other => Classified::failed(clean(&other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecretStore;

    #[test]
    fn token_blob_round_trips_in_chunks_and_clears() {
        let s = MemorySecretStore::default();
        assert!(read_tokens(&s, "srv").unwrap().is_none());
        let big: String = "é".repeat(3 * CHUNK_BYTES);
        write_tokens(&s, "srv", &big).unwrap();
        assert!(chunk_count(&s, "srv").unwrap() > 3);
        assert_eq!(
            read_tokens(&s, "srv").unwrap().as_deref(),
            Some(big.as_str())
        );
        // A smaller blob drops the stale trailing chunks.
        write_tokens(&s, "srv", "small").unwrap();
        assert_eq!(chunk_count(&s, "srv").unwrap(), 1);
        assert!(s.get(&chunk_ref("srv", 1).unwrap()).unwrap().is_none());
        assert_eq!(read_tokens(&s, "srv").unwrap().as_deref(), Some("small"));
        clear_tokens(&s, "srv").unwrap();
        assert!(read_tokens(&s, "srv").unwrap().is_none());
        assert!(s.get(&chunk_ref("srv", 0).unwrap()).unwrap().is_none());
        // A missing chunk reads as no tokens rather than a corrupt blob.
        write_tokens(&s, "srv", &big).unwrap();
        s.delete(&chunk_ref("srv", 1).unwrap()).unwrap();
        assert!(read_tokens(&s, "srv").unwrap().is_none());
        assert!(write_tokens(&s, "srv", &"x".repeat(CHUNK_BYTES * (MAX_CHUNKS + 1))).is_err());
    }

    #[tokio::test]
    async fn credential_store_round_trip() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
        let store = KeychainCredentialStore::new(Arc::clone(&secrets), "srv", Arc::default());
        assert!(store.load().await.unwrap().is_none());
        let creds = StoredCredentials::new("client-1".into(), None, vec!["a".into()], Some(1));
        store.save(creds).await.unwrap();
        let loaded = store.load().await.unwrap().unwrap();
        assert_eq!(loaded.client_id, "client-1");
        assert_eq!(loaded.granted_scopes, vec!["a"]);
        let _guard = store.acquire_refresh_guard().await.unwrap();
        store.clear().await.unwrap();
        assert!(store.load().await.unwrap().is_none());
    }

    #[test]
    fn request_line_parsing() {
        assert!(matches!(
            parse_request(b"GET /callback?code=1&state=s HTTP/1.1\r\nHost: x\r\n\r\n"),
            Request::Callback(t) if t == "/callback?code=1&state=s"
        ));
        assert!(matches!(
            parse_request(b"GET /favicon.ico HTTP/1.1\r\n"),
            Request::NotFound
        ));
        assert!(matches!(
            parse_request(b"POST /callback HTTP/1.1\r\n"),
            Request::NotFound
        ));
        assert!(matches!(parse_request(b"garbage\r\n"), Request::Bad));
        assert!(matches!(
            parse_request(b"GET /callback HTTP/1.1"),
            Request::Bad
        ));
        assert!(matches!(
            parse_request(&vec![b'a'; MAX_REQUEST_BYTES]),
            Request::TooLarge
        ));
    }

    #[test]
    fn callback_requires_our_state() {
        let url =
            |q: &str| reqwest::Url::parse(&format!("http://127.0.0.1:1/callback?{q}")).unwrap();
        assert!(matches!(
            callback_outcome(&url("code=c&state=s"), "s"),
            Some(Ok(_))
        ));
        assert!(callback_outcome(&url("code=c&state=other"), "s").is_none());
        assert!(callback_outcome(&url("code=c"), "s").is_none());
        assert!(callback_outcome(&url("state=s"), "s").is_none());
        let Some(Err(msg)) = callback_outcome(
            &url("error=access_denied&error_description=User%20said%20no&state=s"),
            "s",
        ) else {
            panic!("expected an error outcome");
        };
        assert_eq!(msg, "Sign-in failed: access was denied (User said no).");
        let odd = authorization_error("<script>", Some(&"x".repeat(1000)));
        assert!(!odd.contains('<') && odd.chars().count() < 400);
    }

    #[tokio::test]
    async fn loopback_ignores_strays_and_returns_the_callback() {
        let cb = LoopbackCallback::bind(None).await.unwrap();
        let base = cb.redirect_uri();
        assert!(base.starts_with("http://127.0.0.1:") && base.ends_with("/callback"));
        let port = cb.port;
        let cancel = CancellationToken::new();
        let waiter = tokio::spawn(async move { cb.wait("st", &cancel).await });
        let send = |req: String| async move {
            let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            s.write_all(req.as_bytes()).await.unwrap();
            let mut out = String::new();
            // An oversized request is closed with unread input, which may reset the socket.
            let _ = s.read_to_string(&mut out).await;
            out
        };
        // An idle connection (browser preconnect) must not hold up the others.
        let _idle = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        assert!(send("GET /nope HTTP/1.1\r\n\r\n".into())
            .await
            .starts_with("HTTP/1.1 404"));
        assert!(
            send("GET /callback?code=x&state=forged HTTP/1.1\r\n\r\n".into())
                .await
                .starts_with("HTTP/1.1 400")
        );
        let big = send(format!(
            "GET /{} HTTP/1.1\r\n",
            "a".repeat(MAX_REQUEST_BYTES)
        ))
        .await;
        assert!(big.is_empty() || big.starts_with("HTTP/1.1 431"));
        let ok = send("GET /callback?code=abc&state=st HTTP/1.1\r\n\r\n".into()).await;
        assert!(ok.starts_with("HTTP/1.1 200") && ok.contains("Sign-in complete"));
        assert!(!ok.contains("abc"), "no reflected input");
        let url = waiter.await.unwrap().unwrap();
        assert_eq!(
            url,
            format!("http://127.0.0.1:{port}/callback?code=abc&state=st")
        );
    }

    #[tokio::test]
    async fn loopback_cancels() {
        let cb = LoopbackCallback::bind(None).await.unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(cb.wait("s", &cancel).await.unwrap_err(), "cancelled");
    }

    #[test]
    fn browser_url_rules() {
        let ok = |u: &str| browser_url_allowed(&reqwest::Url::parse(u).unwrap());
        assert!(ok("https://auth.example.com/authorize"));
        assert!(ok("http://127.0.0.1:9/authorize") && ok("http://localhost/a"));
        assert!(!ok("http://auth.example.com/authorize"));
        assert!(!ok("file:///etc/passwd") && !ok("javascript:alert(1)"));
    }

    fn ctx(mode: AuthMode, has_headers: bool, had_tokens: bool) -> AuthContext {
        AuthContext {
            mode,
            has_headers,
            had_tokens,
        }
    }

    fn http_err(e: StreamableHttpError<reqwest::Error>) -> ClientInitializeError {
        ClientInitializeError::TransportError {
            error: rmcp::transport::DynamicTransportError::from_parts(
                "WorkerTransport<StreamableHttpClientWorker<reqwest::Client>>",
                std::any::TypeId::of::<()>(),
                Box::new(e),
            ),
            context: "send initialize request".into(),
        }
    }

    fn auth_required() -> ClientInitializeError {
        http_err(StreamableHttpError::AuthRequired(AuthRequiredError::new(
            "Bearer resource_metadata=\"https://x/.well-known/oauth-protected-resource\"".into(),
        )))
    }

    #[test]
    fn classifies_unauthorized_per_mode() {
        let c = classify_init(&auth_required(), ctx(AuthMode::None, false, false));
        let Failure::Failed(m) = &c.failure else {
            panic!()
        };
        assert!(m.starts_with("This server requires authentication"));
        assert!(!m.contains("Transport [") && !m.contains("resource_metadata"));
        let ClientInitializeError::TransportError { error, .. } = auth_required() else {
            unreachable!()
        };
        assert!(!log_chain(error.error.as_ref()).contains("resource_metadata"));
        assert!(c
            .challenge
            .as_deref()
            .unwrap()
            .contains("resource_metadata"));

        let c = classify_init(&auth_required(), ctx(AuthMode::None, true, false));
        assert!(matches!(c.failure, Failure::Failed(m) if m.contains("headers")));
        let c = classify_init(&auth_required(), ctx(AuthMode::User, false, false));
        assert_eq!(c.failure, Failure::NeedsAuth(None));
        let c = classify_init(&auth_required(), ctx(AuthMode::User, false, true));
        assert!(matches!(c.failure, Failure::NeedsAuth(Some(m)) if m.contains("expired")));
        let c = classify_init(
            &auth_required(),
            ctx(AuthMode::ClientCredentials, false, false),
        );
        assert!(matches!(c.failure, Failure::Failed(m) if m.contains("client credentials")));
        // 401 without a challenge header.
        let e = http_err(StreamableHttpError::UnexpectedServerResponse(
            "HTTP 401 Unauthorized: nope".into(),
        ));
        let c = classify_init(&e, ctx(AuthMode::None, false, false));
        assert!(matches!(c.failure, Failure::Failed(m) if m.starts_with("This server requires")));
    }

    #[test]
    fn classifies_scope_auth_errors_and_generic_failures() {
        let e = http_err(StreamableHttpError::InsufficientScope(
            InsufficientScopeError::new(
                "Bearer error=\"insufficient_scope\"".into(),
                Some("repo".into()),
            ),
        ));
        let c = classify_init(&e, ctx(AuthMode::User, false, true));
        assert!(
            matches!(&c.failure, Failure::Failed(m) if m.contains("`repo`") && m.contains("Scopes"))
        );

        let dcr = AuthError::RegistrationFailed(
            "Dynamic registration failed: Registration failed: Dynamic client registration not supported".into(),
        );
        let c = classify_auth(&dcr, ctx(AuthMode::User, false, false));
        assert!(matches!(&c.failure, Failure::Failed(m) if m.contains("enter a Client ID")));
        let c = classify_auth(
            &AuthError::PkceUnsupported,
            ctx(AuthMode::User, false, false),
        );
        assert!(matches!(&c.failure, Failure::Failed(m) if m.contains("PKCE")));
        let c = classify_auth(
            &AuthError::TokenRefreshRejected("invalid_grant".into()),
            ctx(AuthMode::User, false, true),
        );
        assert!(matches!(c.failure, Failure::NeedsAuth(Some(_))));
        let c = classify_auth(
            &AuthError::TokenExchangeFailed(
                "error decoding response body: missing field `access_token` at line 1 column 266"
                    .into(),
            ),
            ctx(AuthMode::User, false, false),
        );
        assert!(matches!(&c.failure, Failure::Failed(m) if m.contains("Client secret")));
        let c = classify_auth(
            &AuthError::TokenExchangeFailed("HTTP 500".into()),
            ctx(AuthMode::User, false, false),
        );
        assert!(matches!(&c.failure, Failure::Failed(m) if m.contains("didn't issue a token")));
        let c = classify_auth(
            &AuthError::ClientCredentialsError("Token exchange failed: invalid_client".into()),
            ctx(AuthMode::ClientCredentials, false, false),
        );
        assert!(
            matches!(&c.failure, Failure::Failed(m) if m.starts_with("The server rejected the client"))
        );
        let c = classify_init(
            &http_err(StreamableHttpError::Auth(AuthError::NoAuthorizationSupport)),
            ctx(AuthMode::User, false, false),
        );
        assert!(matches!(&c.failure, Failure::Failed(m) if m.contains("doesn't advertise OAuth")));

        let long = "x".repeat(2_000);
        let e = http_err(StreamableHttpError::UnexpectedServerResponse(
            format!("HTTP 500 Internal Server Error: {long}").into(),
        ));
        let c = classify_init(&e, ctx(AuthMode::None, false, false));
        let Failure::Failed(m) = c.failure else {
            panic!()
        };
        assert!(m.chars().count() <= MAX_MESSAGE + 1 && !m.contains("Transport ["));
        let c = classify_init(
            &ClientInitializeError::ConnectionClosed("initialize".into()),
            ctx(AuthMode::None, false, false),
        );
        assert!(matches!(c.failure, Failure::Failed(m) if m.contains("connection closed")));
        // A server that kills the handshake mid-way (e.g. rejects the session it just
        // issued) — rmcp drops the HTTP error, so name the likely cause instead of
        // showing the raw variant.
        let e = http_err(StreamableHttpError::<reqwest::Error>::TransportChannelClosed);
        let c = classify_init(&e, ctx(AuthMode::None, false, false));
        assert!(matches!(c.failure, Failure::Failed(m) if m.contains("closed the connection")));
    }
}
