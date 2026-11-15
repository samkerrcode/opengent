//! MCP OAuth end to end against an in-process mock: a Streamable HTTP MCP endpoint that
//! demands a bearer token, protected-resource + authorization-server metadata, Dynamic
//! Client Registration, an auto-approving `/authorize`, and `/token` (authorization code +
//! PKCE, refresh, client credentials).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_core::config::resolve_data_root;
use agent_core::domain::ToolPolicy;
use agent_core::mcp::{
    KeyValueInput, McpAuthInput, McpManager, McpServerInput, McpServerStatus, McpStore,
    McpTransport,
};
use agent_core::secrets::{MemorySecretStore, SecretStore};
use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Default)]
struct State {
    expires_in: u64,
    reject_refresh: bool,
    no_dcr: bool,
    next: u64,
    valid_tokens: Vec<String>,
    refresh_tokens: HashMap<String, String>,
    /// code -> (client_id, code_challenge, redirect_uri)
    codes: HashMap<String, (String, String, String)>,
    grants: Vec<String>,
    mcp_requests: usize,
}

const CC_ID: &str = "machine";
const CC_SECRET: &str = "machine-secret";
/// Spec revisions this mock knows — the released versions github-mcp-server accepts.
const KNOWN_PROTOCOL_VERSIONS: &[&str] = &["2025-03-26", "2025-06-18", "2025-11-25"];

struct Mock {
    origin: String,
    state: Arc<Mutex<State>>,
}

impl Mock {
    async fn start(expires_in: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let state = Arc::new(Mutex::new(State {
            expires_in,
            ..Default::default()
        }));
        let (o, s) = (origin.clone(), Arc::clone(&state));
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let (o, s) = (o.clone(), Arc::clone(&s));
                tokio::spawn(async move { serve(sock, &o, &s).await });
            }
        });
        Self { origin, state }
    }

    fn url(&self) -> String {
        format!("{}/mcp", self.origin)
    }

    fn grants(&self) -> Vec<String> {
        self.state.lock().unwrap().grants.clone()
    }
}

struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: String,
}

fn pairs(s: &str) -> HashMap<String, String> {
    reqwest::Url::parse(&format!("http://x/?{s}"))
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

async fn read_req(sock: &mut TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let target = first.next()?.to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + 4 + len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[head_end + 4..]).into_owned();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Some(Req {
        method,
        path: path.to_owned(),
        query: pairs(query),
        headers,
        body,
    })
}

async fn reply(sock: &mut TcpStream, status: &str, extra: &[(&str, String)], body: &str) {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = sock.write_all(head.as_bytes()).await;
    let _ = sock.write_all(body.as_bytes()).await;
    let _ = sock.shutdown().await;
}

fn json() -> (&'static str, String) {
    ("Content-Type", "application/json".into())
}

fn issue(s: &mut State, with_refresh: bool) -> serde_json::Value {
    s.next += 1;
    let access = format!("at-{}", s.next);
    s.valid_tokens.push(access.clone());
    let mut t = serde_json::json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": s.expires_in,
    });
    if with_refresh {
        let rt = format!("rt-{}", s.next);
        s.refresh_tokens.insert(rt.clone(), access);
        t["refresh_token"] = rt.into();
    }
    t
}

async fn serve(mut sock: TcpStream, origin: &str, state: &Mutex<State>) {
    let Some(req) = read_req(&mut sock).await else {
        return;
    };
    let p = req.path.as_str();
    if p.starts_with("/.well-known/oauth-protected-resource") {
        let body = serde_json::json!({
            "resource": format!("{origin}/mcp"),
            "authorization_servers": [origin],
        });
        return reply(&mut sock, "200 OK", &[json()], &body.to_string()).await;
    }
    if p.starts_with("/.well-known/oauth-authorization-server") {
        let no_dcr = state.lock().unwrap().no_dcr;
        let mut body = serde_json::json!({
            "issuer": origin,
            "authorization_endpoint": format!("{origin}/authorize"),
            "token_endpoint": format!("{origin}/token"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token", "client_credentials"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none", "client_secret_post", "client_secret_basic"],
        });
        if !no_dcr {
            body["registration_endpoint"] = format!("{origin}/register").into();
        }
        return reply(&mut sock, "200 OK", &[json()], &body.to_string()).await;
    }
    match (req.method.as_str(), p) {
        ("POST", "/register") => {
            let r: serde_json::Value = serde_json::from_str(&req.body).unwrap();
            let id = {
                let mut s = state.lock().unwrap();
                s.next += 1;
                format!("dcr-{}", s.next)
            };
            let body = serde_json::json!({"client_id": id, "redirect_uris": r["redirect_uris"]});
            reply(&mut sock, "201 Created", &[json()], &body.to_string()).await
        }
        ("GET", "/authorize") => {
            let q = &req.query;
            assert_eq!(
                q.get("code_challenge_method").map(String::as_str),
                Some("S256")
            );
            let code = {
                let mut s = state.lock().unwrap();
                s.next += 1;
                let code = format!("code-{}", s.next);
                s.codes.insert(
                    code.clone(),
                    (
                        q["client_id"].clone(),
                        q["code_challenge"].clone(),
                        q["redirect_uri"].clone(),
                    ),
                );
                code
            };
            let mut loc = reqwest::Url::parse(&q["redirect_uri"]).unwrap();
            loc.query_pairs_mut()
                .append_pair("code", &code)
                .append_pair("state", &q["state"]);
            reply(&mut sock, "302 Found", &[("Location", loc.to_string())], "").await
        }
        ("POST", "/token") => {
            let f = pairs(&req.body);
            let grant = f.get("grant_type").cloned().unwrap_or_default();
            let ok = {
                let mut s = state.lock().unwrap();
                s.grants.push(grant.clone());
                match grant.as_str() {
                    "authorization_code" => s
                        .codes
                        .remove(&f["code"])
                        .filter(|(cid, challenge, redirect)| {
                            let verifier = f.get("code_verifier").cloned().unwrap_or_default();
                            let hashed = base64::engine::general_purpose::URL_SAFE_NO_PAD
                                .encode(Sha256::digest(verifier.as_bytes()));
                            f.get("client_id") == Some(cid)
                                && &hashed == challenge
                                && f.get("redirect_uri") == Some(redirect)
                        })
                        .map(|_| issue(&mut s, true)),
                    "refresh_token" if !s.reject_refresh => {
                        let rt = f.get("refresh_token").cloned().unwrap_or_default();
                        s.refresh_tokens.remove(&rt).map(|_| issue(&mut s, true))
                    }
                    "client_credentials" => (f.get("client_id").map(String::as_str) == Some(CC_ID)
                        && f.get("client_secret").map(String::as_str) == Some(CC_SECRET)
                        && f.contains_key("resource"))
                    .then(|| issue(&mut s, false)),
                    _ => None,
                }
            };
            match ok {
                Some(t) => reply(&mut sock, "200 OK", &[json()], &t.to_string()).await,
                None => {
                    let e = if grant == "client_credentials" {
                        "invalid_client"
                    } else {
                        "invalid_grant"
                    };
                    reply(
                        &mut sock,
                        "400 Bad Request",
                        &[json()],
                        &format!("{{\"error\":\"{e}\"}}"),
                    )
                    .await
                }
            }
        }
        (_, "/mcp") => {
            let token = req
                .headers
                .get("authorization")
                .and_then(|h| h.strip_prefix("Bearer "))
                .map(str::to_owned);
            let authorized = {
                let mut s = state.lock().unwrap();
                s.mcp_requests += 1;
                token.is_some_and(|t| s.valid_tokens.contains(&t))
            };
            if !authorized {
                let challenge = format!(
                    "Bearer resource_metadata=\"{origin}/.well-known/oauth-protected-resource/mcp\""
                );
                return reply(
                    &mut sock,
                    "401 Unauthorized",
                    &[("WWW-Authenticate", challenge)],
                    "",
                )
                .await;
            }
            if req.method != "POST" {
                return reply(&mut sock, "405 Method Not Allowed", &[], "").await;
            }
            // github-mcp-server quirk: a session created by an `initialize` that
            // requested a spec revision the server doesn't know rejects every
            // subsequent request with `400 invalid session`.
            if req
                .headers
                .get("mcp-session-id")
                .is_some_and(|s| s.starts_with("dead-"))
            {
                return reply(&mut sock, "400 Bad Request", &[], "invalid session").await;
            }
            let msg: serde_json::Value = serde_json::from_str(&req.body).unwrap();
            let Some(id) = msg.get("id").cloned() else {
                return reply(&mut sock, "202 Accepted", &[], "").await;
            };
            let mut session = req
                .headers
                .get("mcp-session-id")
                .cloned()
                .unwrap_or_else(|| "sess-1".into());
            let out = match msg["method"].as_str().unwrap_or("") {
                "initialize" => {
                    let requested = msg["params"]["protocolVersion"].as_str().unwrap_or("");
                    let negotiated = if KNOWN_PROTOCOL_VERSIONS.contains(&requested) {
                        requested
                    } else {
                        session = format!("dead-{requested}");
                        KNOWN_PROTOCOL_VERSIONS[KNOWN_PROTOCOL_VERSIONS.len() - 1]
                    };
                    serde_json::json!({"result": {
                        "protocolVersion": negotiated,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "mock-oauth", "version": "1"},
                    }})
                }
                "tools/list" => serde_json::json!({"result": {"tools": [
                    {"name": "echo", "inputSchema": {"type": "object"}},
                ]}}),
                "tools/call" => {
                    serde_json::json!({"result": {"content": [{"type": "text", "text": "hi"}]}})
                }
                m => {
                    serde_json::json!({"error": {"code": -32601, "message": format!("unsupported {m}")}})
                }
            };
            let mut body = serde_json::json!({"jsonrpc": "2.0", "id": id});
            body.as_object_mut()
                .unwrap()
                .extend(out.as_object().unwrap().clone());
            reply(
                &mut sock,
                "200 OK",
                &[json(), ("Mcp-Session-Id", session)],
                &body.to_string(),
            )
            .await
        }
        _ => reply(&mut sock, "404 Not Found", &[], "").await,
    }
}

struct Harness {
    manager: Arc<McpManager>,
    secrets: Arc<dyn SecretStore>,
    root: std::path::PathBuf,
}

fn harness_with(secrets: Arc<dyn SecretStore>, root: std::path::PathBuf) -> Harness {
    agent_core::http::init();
    let data = resolve_data_root(Some(&root)).unwrap();
    let store = McpStore::load(&data, Arc::clone(&secrets)).unwrap();
    let manager = McpManager::new(
        store,
        agent_core::http::client().unwrap(),
        tokio::runtime::Handle::current(),
        || {},
    );
    Harness {
        manager,
        secrets,
        root,
    }
}

fn harness() -> Harness {
    let root = std::env::temp_dir().join(format!("opengent-oauth-{}", uuid::Uuid::new_v4()));
    harness_with(Arc::new(MemorySecretStore::default()), root)
}

impl Harness {
    fn add(&self, url: &str, auth: McpAuthInput) -> String {
        self.manager
            .store()
            .save(McpServerInput {
                id: None,
                name: "remote".into(),
                transport: McpTransport::StreamableHttp,
                command: String::new(),
                env: Vec::new(),
                cwd: None,
                url: url.into(),
                headers: Vec::new(),
                auth,
                default_policy: ToolPolicy::Ask,
                call_timeout_secs: 30,
            })
            .unwrap()
            .id
    }

    fn status(&self, id: &str) -> McpServerStatus {
        self.view(id).status
    }

    fn view(&self, id: &str) -> agent_core::mcp::McpServerView {
        self.manager
            .list_views()
            .into_iter()
            .find(|v| v.config.id == id)
            .unwrap()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn user_auth(client_id: &str) -> McpAuthInput {
    McpAuthInput::OauthUser {
        client_id: client_id.into(),
        client_secret: String::new(),
        scopes: String::new(),
        redirect_port: None,
    }
}

/// Plays the browser: follows `/authorize`'s redirect to the loopback callback.
fn browser() -> impl FnOnce(&str) -> Result<(), String> + Send {
    |url: &str| {
        let url = url.to_owned();
        tokio::spawn(async move {
            let http = agent_core::http::client().unwrap();
            let res = http.get(&url).send().await.unwrap();
            assert_eq!(res.status(), 302, "authorize should redirect");
            let location = res.headers()["location"].to_str().unwrap().to_owned();
            assert!(location.starts_with("http://127.0.0.1:"));
            let page = http.get(&location).send().await.unwrap();
            assert_eq!(page.status(), 200);
        });
        Ok(())
    }
}

#[tokio::test]
async fn unauthenticated_401_is_a_short_actionable_message() {
    let mock = Mock::start(3600).await;
    let h = harness();
    let id = h.add(&mock.url(), McpAuthInput::None);
    h.manager.connect(&id).await;
    let McpServerStatus::Failed { error } = h.status(&id) else {
        panic!("expected failure, got {:?}", h.status(&id));
    };
    assert!(
        error.starts_with("This server requires authentication"),
        "{error}"
    );
    assert!(!error.contains("Transport [") && !error.contains("resource_metadata"));
}

#[tokio::test]
async fn static_authorization_header_connects() {
    let mock = Mock::start(3600).await;
    mock.state
        .lock()
        .unwrap()
        .valid_tokens
        .push("pat-token".into());
    let h = harness();
    let id = h
        .manager
        .store()
        .save(McpServerInput {
            id: None,
            name: "pat".into(),
            transport: McpTransport::StreamableHttp,
            command: String::new(),
            env: Vec::new(),
            cwd: None,
            url: mock.url(),
            headers: vec![KeyValueInput {
                name: "Authorization".into(),
                value: "Bearer pat-token".into(),
                secret: true,
            }],
            auth: McpAuthInput::None,
            default_policy: ToolPolicy::Ask,
            call_timeout_secs: 30,
        })
        .unwrap()
        .id;
    h.manager.connect(&id).await;
    assert_eq!(h.status(&id), McpServerStatus::Connected { tool_count: 1 });
    h.manager.shutdown();
}

#[tokio::test]
async fn browser_sign_in_connects_persists_and_signs_out() {
    let mock = Mock::start(3600).await;
    let h = harness();
    let id = h.add(&mock.url(), user_auth(""));
    h.manager.connect(&id).await;
    assert_eq!(h.status(&id), McpServerStatus::NeedsAuth { reason: None });
    assert_eq!(
        mock.state.lock().unwrap().mcp_requests,
        0,
        "no request without a sign-in"
    );

    h.manager.sign_in(&id, browser()).await.unwrap();
    assert_eq!(h.status(&id), McpServerStatus::Connected { tool_count: 1 });
    assert!(h.view(&id).signed_in);
    assert_eq!(mock.grants(), vec!["authorization_code"]);

    // A restart reconnects from the stored tokens without a browser.
    let h2 = harness_with(Arc::clone(&h.secrets), h.root.clone());
    h2.manager.connect(&id).await;
    assert_eq!(h2.status(&id), McpServerStatus::Connected { tool_count: 1 });
    h2.manager.shutdown();

    h.manager.sign_out(&id).await.unwrap();
    assert_eq!(h.status(&id), McpServerStatus::NeedsAuth { reason: None });
    assert!(!h.view(&id).signed_in);
    assert!(!h.manager.store().has_oauth_tokens(&id));
}

#[tokio::test]
async fn sign_in_with_a_preregistered_client_and_cancel() {
    let mock = Mock::start(3600).await;
    mock.state.lock().unwrap().no_dcr = true;
    let h = harness();
    let id = h.add(&mock.url(), user_auth(""));
    let err = h.manager.sign_in(&id, browser()).await.unwrap_err();
    assert!(err.contains("enter a Client ID"), "{err}");
    assert!(matches!(
        h.status(&id),
        McpServerStatus::NeedsAuth { reason: Some(_) }
    ));

    let id2 = {
        let mut i = h.manager.store().get(&id).unwrap();
        i.name = "x".into();
        h.manager
            .store()
            .save(McpServerInput {
                id: Some(i.id.clone()),
                name: i.name,
                transport: McpTransport::StreamableHttp,
                command: String::new(),
                env: Vec::new(),
                cwd: None,
                url: i.url,
                headers: Vec::new(),
                auth: user_auth("preregistered"),
                default_policy: ToolPolicy::Ask,
                call_timeout_secs: 30,
            })
            .unwrap()
            .id
    };
    h.manager.sign_in(&id2, browser()).await.unwrap();
    assert_eq!(h.status(&id2), McpServerStatus::Connected { tool_count: 1 });

    // Cancel while waiting for the browser.
    let m = Arc::clone(&h.manager);
    let id3 = id2.clone();
    let task = tokio::spawn(async move { m.sign_in(&id3, |_| Ok(())).await });
    for _ in 0..100 {
        if h.status(&id2) == McpServerStatus::Authorizing {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(h.status(&id2), McpServerStatus::Authorizing);
    assert_eq!(
        h.manager.pending_connections(),
        0,
        "a sign-in never blocks the scheduler"
    );
    h.manager.cancel_sign_in(&id2);
    assert!(task.await.unwrap().is_err());
    assert_eq!(h.status(&id2), McpServerStatus::NeedsAuth { reason: None });
}

#[tokio::test]
async fn expired_tokens_refresh_and_a_rejected_refresh_needs_sign_in() {
    // Tokens "expire" immediately, so every use refreshes.
    let mock = Mock::start(1).await;
    let h = harness();
    let id = h.add(&mock.url(), user_auth(""));
    h.manager.sign_in(&id, browser()).await.unwrap();
    assert_eq!(h.status(&id), McpServerStatus::Connected { tool_count: 1 });
    assert!(
        mock.grants().iter().any(|g| g == "refresh_token"),
        "{:?}",
        mock.grants()
    );

    mock.state.lock().unwrap().reject_refresh = true;
    h.manager.connect(&id).await;
    let McpServerStatus::NeedsAuth {
        reason: Some(reason),
    } = h.status(&id)
    else {
        panic!("expected sign-in required, got {:?}", h.status(&id));
    };
    assert!(reason.contains("expired"), "{reason}");
    assert!(!h.view(&id).signed_in);
}

#[tokio::test]
async fn client_credentials_connect_renew_and_reject() {
    // 66 s tokens renew 60 s before expiry, i.e. after ~6 s.
    let mock = Mock::start(66).await;
    let h = harness();
    let id = h.add(
        &mock.url(),
        McpAuthInput::OauthClientCredentials {
            client_id: CC_ID.into(),
            client_secret: CC_SECRET.into(),
            scopes: "mcp".into(),
        },
    );
    h.manager.connect(&id).await;
    assert_eq!(h.status(&id), McpServerStatus::Connected { tool_count: 1 });
    assert_eq!(mock.grants(), vec!["client_credentials"]);
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert_eq!(
        mock.grants(),
        vec!["client_credentials", "client_credentials"]
    );
    assert_eq!(h.status(&id), McpServerStatus::Connected { tool_count: 1 });

    let bad = h.add_named(&mock.url(), "bad", CC_ID, "wrong");
    h.manager.connect(&bad).await;
    let McpServerStatus::Failed { error } = h.status(&bad) else {
        panic!("expected failure, got {:?}", h.status(&bad));
    };
    assert!(
        error.starts_with("The server rejected the client credentials"),
        "{error}"
    );
    h.manager.shutdown();
}

impl Harness {
    fn add_named(&self, url: &str, name: &str, client_id: &str, secret: &str) -> String {
        self.manager
            .store()
            .save(McpServerInput {
                id: None,
                name: name.into(),
                transport: McpTransport::StreamableHttp,
                command: String::new(),
                env: Vec::new(),
                cwd: None,
                url: url.into(),
                headers: Vec::new(),
                auth: McpAuthInput::OauthClientCredentials {
                    client_id: client_id.into(),
                    client_secret: secret.into(),
                    scopes: String::new(),
                },
                default_policy: ToolPolicy::Ask,
                call_timeout_secs: 30,
            })
            .unwrap()
            .id
    }
}
