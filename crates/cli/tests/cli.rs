//! Runs the real `opengent` binary against a temp data root. The "app" side — saving
//! providers, connecting MCP servers, granting Allow, creating agents — is done in this test
//! process with agent-core's normal (writable) stores, exactly as the desktop app does it;
//! the CLI process then only reads. Model and MCP servers are in-process mocks on loopback:
//! an OpenAI-compatible endpoint that calls a tool when the prompt says `call <tool>`, and a
//! Streamable HTTP MCP server.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use agent_core::agents::{AgentInput, AgentServerAccess, ModelRef};
use agent_core::config::{data_dir_env_var, product, resolve_data_root, DataRoot};
use agent_core::domain::{ChatId, RunEvent, RunEventPayload, ToolCallStatus, ToolPolicy};
use agent_core::mcp::{
    ChatToolOverrides, KeyValueInput, McpManager, McpServerInput, McpStore, McpTransport,
    PermissionScope, ScopePolicies,
};
use agent_core::providers::capabilities::ImageSupport;
use agent_core::providers::registry::{ProviderInput, ProviderKind, ProviderStore, TlsMode};
use agent_core::secrets::{MemorySecretStore, SecretStore};
use agent_core::settings::SettingsStore;
use agent_core::storage::agents as agents_repo;
use agent_core::storage::chats as chats_repo;
use agent_core::storage::{Db, MessageStatus};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const BIN: &str = env!("CARGO_BIN_EXE_opengent");

struct Root {
    path: PathBuf,
    root: DataRoot,
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn root() -> Root {
    let path =
        std::env::temp_dir().join(format!("{}-cli-{}", product().slug, uuid::Uuid::new_v4()));
    Root {
        root: resolve_data_root(Some(&path)).unwrap(),
        path,
    }
}

impl Root {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env(data_dir_env_var(), &self.path)
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// Runs the CLI off the test's runtime (the mocks live on it).
    async fn run_async(&self, args: &[&str]) -> Output {
        let mut c = self.cmd(args);
        tokio::task::spawn_blocking(move || c.output().unwrap())
            .await
            .unwrap()
    }

    fn db(&self) -> Db {
        Db::open_root(&self.root).unwrap().0
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The chat id the CLI announced on stderr.
fn chat_of(o: &Output) -> ChatId {
    let err = stderr(o);
    let id = err
        .split_whitespace()
        .skip_while(|w| *w != "chat")
        .nth(1)
        .unwrap_or_else(|| panic!("no chat id in {err}"));
    id.parse().unwrap()
}

fn events(o: &Output) -> Vec<RunEvent> {
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

fn tool_statuses(events: &[RunEvent]) -> Vec<ToolCallStatus> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            RunEventPayload::ToolResult { status, .. } => Some(*status),
            _ => None,
        })
        .collect()
}

fn reply_status(db: &Db, chat: ChatId) -> MessageStatus {
    db.call_blocking(move |c| Ok(chats_repo::list_messages(c, chat)?.last().unwrap().status))
        .unwrap()
}

#[test]
fn binary_name_matches_product_slug() {
    let stem = Path::new(BIN)
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(stem, product().slug);
}

#[test]
fn run_completes_and_the_reply_is_stored_complete() {
    let r = root();
    let o = r.run(&[
        "run",
        "--model",
        "simulated/simulated-1",
        "hello from the cli",
    ]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("hello from the cli"));
    let chat = chat_of(&o);
    assert_eq!(reply_status(&r.db(), chat), MessageStatus::Complete);

    // Continuing the chat uses its stored model.
    let id = chat.to_string();
    let o = r.run(&["run", "--chat", &id, "again"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let db = r.db();
    let n = db
        .call_blocking(move |c| chats_repo::list_messages(c, chat))
        .unwrap();
    assert_eq!(n.len(), 4);

    assert_eq!(code(&r.run(&["run", "no model given"])), 2, "usage error");
    assert_eq!(code(&r.run(&["run", "--bogus", "x"])), 2);
}

#[test]
fn json_emits_ordered_run_events() {
    let r = root();
    let o = r.run(&["run", "--json", "--model", "simulated", "ordered"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let events = events(&o);
    assert!(events.len() > 3);
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e.seq, i as u64, "contiguous, in order");
    }
    assert!(matches!(
        events.last().unwrap().payload,
        RunEventPayload::Completed { .. }
    ));
}

#[test]
fn closing_stdout_cancels_the_run_and_stores_it_cancelled() {
    use std::io::BufRead;
    let r = root();
    let mut child = r
        .cmd(&[
            "run",
            "--json",
            "--model",
            "simulated",
            "a long enough reply",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    out.read_line(&mut first).unwrap();
    drop(out);
    let o = child.wait_with_output().unwrap();
    assert_eq!(code(&o), 3, "cancelled: {}", stderr(&o));
    assert_eq!(reply_status(&r.db(), chat_of(&o)), MessageStatus::Cancelled);
}

#[test]
fn four_parallel_runs_on_different_chats_all_complete() {
    let r = root();
    // Create the database first so the four start against the same schema.
    drop(r.db());
    let children: Vec<_> = (0..4)
        .map(|i| {
            r.cmd(&["run", "--model", "simulated", &format!("parallel {i}")])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let db_outputs: Vec<Output> = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect();
    let db = r.db();
    for o in &db_outputs {
        assert_eq!(code(o), 0, "{}", stderr(o));
        assert_eq!(reply_status(&db, chat_of(o)), MessageStatus::Complete);
    }
}

#[test]
fn listing_needs_no_database_and_prints_no_secrets() {
    let r = root();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
    McpStore::load(&r.root, secrets)
        .unwrap()
        .save(McpServerInput {
            id: None,
            name: "remote".into(),
            transport: McpTransport::StreamableHttp,
            command: String::new(),
            env: Vec::new(),
            cwd: None,
            url: "http://127.0.0.1:9/mcp".into(),
            headers: vec![KeyValueInput {
                name: "X-Api-Key".into(),
                value: "sekret-value-123".into(),
                secret: true,
            }],
            auth: Default::default(),
            default_policy: ToolPolicy::Ask,
            call_timeout_secs: 60,
        })
        .unwrap();
    // Absent database.
    for args in [
        &["models"][..],
        &["models", "--json"],
        &["mcp", "list", "--tools"],
        &["mcp", "list", "--json"],
    ] {
        let o = r.run(args);
        assert_eq!(code(&o), 0, "{args:?}: {}", stderr(&o));
        assert!(!String::from_utf8_lossy(&o.stdout).contains("sekret"));
    }
    assert!(
        !r.root.database_path().exists(),
        "listing opened the database"
    );

    // Locked database: another connection holds an exclusive transaction.
    let db = r.db();
    let (hold, release) = std::sync::mpsc::channel::<()>();
    let (locked_tx, locked) = std::sync::mpsc::channel::<()>();
    db.spawn_job(move |c| {
        c.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let _ = locked_tx.send(());
        let _ = release.recv();
        c.execute_batch("COMMIT").unwrap();
        Ok(())
    });
    locked.recv().unwrap();
    for args in [&["models"][..], &["mcp", "list"]] {
        let o = r.run(args);
        assert_eq!(code(&o), 0, "{args:?}: {}", stderr(&o));
    }
    let o = r.run(&["mcp", "list"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("remote"));
    drop(hold);
}

#[test]
fn schedules_are_added_listed_and_removed_where_the_app_fires_them() {
    use agent_core::agents::Recurrence;
    let r = root();
    let db = r.db();
    db.call_blocking(|c| {
        agents_repo::save_agent(
            c,
            AgentInput {
                id: None,
                name: "Scout".into(),
                description: String::new(),
                default_model: ModelRef {
                    provider_id: "simulated".into(),
                    model: "simulated-1".into(),
                },
                fallback_model: None,
                system_prompt: String::new(),
                servers: Vec::new(),
            },
        )
    })
    .unwrap();
    let o = r.run(&[
        "schedule", "add", "--agent", "scout", "--every", "2h", "check", "releases",
    ]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stderr(&o).contains("while the desktop app is open"));
    let id = String::from_utf8_lossy(&o.stdout).trim().to_owned();

    let o = r.run(&["schedule", "list", "--json"]);
    let listed: Vec<agent_core::agents::AgentSchedule> = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id.to_string(), id);
    assert_eq!(listed[0].prompt, "check releases");
    assert_eq!(listed[0].recurrence, Recurrence::Interval { minutes: 120 });
    // The desktop scheduler's tick query picks it up once due.
    let due_at = listed[0].next_run_at.unwrap();
    let due = db
        .call_blocking(move |c| agents_repo::due_schedules(c, due_at))
        .unwrap();
    assert_eq!(due.len(), 1);

    for bad in [
        &["schedule", "add", "--agent", "scout", "x"][..],
        &["schedule", "add", "--agent", "nobody", "--every", "1h", "x"],
        &[
            "schedule",
            "add",
            "--agent",
            "scout",
            "--at",
            "2000-01-01 10:00",
            "x",
        ],
    ] {
        assert_eq!(code(&r.run(bad)), 2, "{bad:?}");
    }
    assert_eq!(code(&r.run(&["schedule", "remove", &id])), 0);
    assert_eq!(
        code(&r.run(&["schedule", "remove", &id])),
        2,
        "already gone"
    );
    let o = r.run(&["schedule", "list"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("No schedules"));
}

#[test]
fn help_follows_any_command_and_options_are_reported_as_typed() {
    let r = root();
    for args in [
        &["help"][..],
        &["run", "--help"],
        &["run", "--model", "simulated", "-h"],
        &["schedule", "--help"],
        &["schedule", "add", "-h"],
        &["mcp", "--help"],
    ] {
        let o = r.run(args);
        assert_eq!(code(&o), 0, "{args:?}: {}", stderr(&o));
        assert!(
            String::from_utf8_lossy(&o.stdout).contains("Usage:"),
            "{args:?}"
        );
    }
    assert!(
        !r.root.database_path().exists(),
        "asking for help ran something"
    );
    let o = r.run(&["run", "-x", "hi"]);
    assert_eq!(code(&o), 2);
    assert!(stderr(&o).contains("unknown option `-x`"), "{}", stderr(&o));
    // After `--` it is the prompt, not a request for help.
    let o = r.run(&["run", "--model", "simulated", "--", "--help"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("--help"));
}

/// `opengent models | head -1`: the reader is gone before anything is written.
#[test]
fn a_closed_stdout_is_not_a_crash() {
    let r = root();
    drop(r.db());
    for args in [
        &["help"][..],
        &["version"],
        &["models"],
        &["models", "--json"],
        &["mcp", "list"],
        &["schedule", "list"],
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let o = r
            .cmd(args)
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(code(&o), 0, "{args:?}: {}", stderr(&o));
        assert!(!stderr(&o).contains("panicked"), "{args:?}: {}", stderr(&o));
    }
}

#[test]
fn piped_listings_carry_no_hints() {
    let r = root();
    let o = r.run(&["models"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("simulated-1"), "{out}");
    assert!(!out.contains("Run a prompt"), "{out}");
    assert!(!stderr(&o).contains("Run a prompt"), "{}", stderr(&o));
}

#[test]
fn the_prompt_can_come_from_stdin() {
    use std::io::Write;
    let r = root();
    let piped = |input: &[u8]| {
        let mut child = r
            .cmd(&["run", "--model", "simulated"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    };
    let o = piped("summarize from stdin\n".as_bytes());
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("summarize from stdin"));
    for (input, why) in [
        (&b"  \n"[..], "empty"),
        (&b"caf\xe9"[..], "valid UTF-8"),
        (&vec![b'a'; 64 * 1024 * 4 + 1][..], "too long"),
    ] {
        let o = piped(input);
        assert_eq!(code(&o), 2, "{why}");
        assert!(stderr(&o).contains(why), "{}", stderr(&o));
    }
}

/// What `timeout`, a service manager or a CI runner sends: the run is cancelled and stored
/// as cancelled, not left streaming.
#[cfg(unix)]
#[test]
fn sigterm_cancels_the_run_and_stores_it_cancelled() {
    use std::io::{BufRead, Read};
    let r = root();
    let mut child = r
        .cmd(&[
            "run",
            "--json",
            "--model",
            "simulated",
            "a long enough reply",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    out.read_line(&mut first).unwrap();
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let mut rest = String::new();
    out.read_to_string(&mut rest).unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(code(&o), 3, "cancelled: {}", stderr(&o));
    assert!(
        rest.lines().last().unwrap_or("").contains("\"cancelled\""),
        "{rest}"
    );
    assert_eq!(reply_status(&r.db(), chat_of(&o)), MessageStatus::Cancelled);
}

// ---- mocks ----------------------------------------------------------------------------

struct Req {
    method: String,
    body: String,
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
    let method = head.split(' ').next()?.to_owned();
    let len: usize = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + 4 + len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(Req {
        method,
        body: String::from_utf8_lossy(&buf[head_end + 4..]).into_owned(),
    })
}

async fn reply(sock: &mut TcpStream, status: &str, kind: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nMcp-Session-Id: s1\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = sock.write_all(head.as_bytes()).await;
    let _ = sock.write_all(body.as_bytes()).await;
    let _ = sock.shutdown().await;
}

async fn serve<F>(handler: F) -> String
where
    F: Fn(Req) -> (String, String) + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let handler = Arc::clone(&handler);
            tokio::spawn(async move {
                let Some(req) = read_req(&mut sock).await else {
                    return;
                };
                let (status, body) = handler(req);
                let kind = if body.starts_with("data:") {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                reply(&mut sock, &status, kind, &body).await;
            });
        }
    });
    origin
}

#[derive(Default)]
struct McpState {
    tools: Vec<serde_json::Value>,
    calls: Vec<String>,
    requests: usize,
}

/// A Streamable HTTP MCP server answering with plain JSON.
async fn mcp_mock(tools: &[&str]) -> (String, Arc<Mutex<McpState>>) {
    let state = Arc::new(Mutex::new(McpState {
        tools: tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t, "description": format!("{t} tool"),
                    "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
                })
            })
            .collect(),
        ..Default::default()
    }));
    let s = Arc::clone(&state);
    let origin = serve(move |req| {
        s.lock().unwrap().requests += 1;
        if req.method != "POST" {
            return ("405 Method Not Allowed".into(), String::new());
        }
        let msg: serde_json::Value = serde_json::from_str(&req.body).unwrap_or_default();
        let Some(id) = msg.get("id").cloned() else {
            return ("202 Accepted".into(), String::new());
        };
        let result = match msg["method"].as_str().unwrap_or("") {
            "initialize" => serde_json::json!({
                "protocolVersion": msg["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "mock", "version": "1"},
            }),
            "tools/list" => serde_json::json!({"tools": s.lock().unwrap().tools}),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("").to_owned();
                s.lock().unwrap().calls.push(name.clone());
                serde_json::json!({"content": [{"type": "text", "text": format!("{name} ran")}]})
            }
            _ => serde_json::json!({}),
        };
        (
            "200 OK".into(),
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
        )
    })
    .await;
    (format!("{origin}/mcp"), state)
}

/// Tool names offered in each completion request the mock model received.
type Offered = Arc<Mutex<Vec<Vec<String>>>>;

/// OpenAI-compatible model: after a tool result it answers "done"; otherwise it calls the
/// tool named by `call <name>` in the prompt when that tool is offered.
async fn model_mock() -> (String, Offered) {
    let offered: Offered = Arc::default();
    let o = Arc::clone(&offered);
    let origin = serve(move |req| {
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap_or_default();
        let tools: Vec<String> = body["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
            .collect();
        o.lock().unwrap().push(tools.clone());
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let last = messages.last().cloned().unwrap_or_default();
        let text = |t: &str| {
            format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                serde_json::json!({"choices":[{"index":0,"delta":{"content":t},"finish_reason":null}]}),
                serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
            )
        };
        let sse = if last["role"] == "tool" {
            text("done")
        } else {
            let prompt = last["content"].to_string();
            match tools.iter().find(|t| prompt.contains(&format!("call {t}"))) {
                Some(t) => format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":t,"arguments":"{\"text\":\"hi\"}"}}]},"finish_reason":null}]}),
                    serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
                ),
                None => text("no tool"),
            }
        };
        ("200 OK".into(), sse)
    })
    .await;
    (format!("{origin}/v1"), offered)
}

/// The app's side of the data root.
struct App {
    r: Root,
    mcp: Arc<McpManager>,
    db: Db,
    provider: String,
    offered: Offered,
}

const CONFIG_FILES: [&str; 3] = ["providers.json", "mcp.json", "settings.json"];

impl App {
    async fn new() -> Self {
        let r = root();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
        let (base_url, offered) = model_mock().await;
        let provider = ProviderStore::load(
            &r.root,
            secrets.clone(),
            agent_core::http::client().unwrap(),
        )
        .unwrap()
        .save(ProviderInput {
            id: None,
            name: "Mock".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url,
            default_model: Some("mock-model".into()),
            api_key: None,
            include_usage: false,
            context_window: Some(32_000),
            model_context_windows: HashMap::new(),
            tls_mode: TlsMode::System,
            tls_ca_pem: None,
            image_support: ImageSupport::Never,
            hidden_models: Vec::new(),
        })
        .unwrap()
        .id;
        SettingsStore::load(&r.root)
            .unwrap()
            .update(&serde_json::json!({ "general": { "share_environment": false } }))
            .unwrap();
        let mcp = McpManager::new(
            McpStore::load(&r.root, secrets).unwrap(),
            agent_core::http::client().unwrap(),
            tokio::runtime::Handle::current(),
            || {},
        );
        let db = r.db();
        Self {
            r,
            mcp,
            db,
            provider,
            offered,
        }
    }

    /// Adds and connects a server, like Connections → Add.
    async fn server(&self, name: &str, url: &str, default: ToolPolicy) -> String {
        let id = self
            .mcp
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
                auth: Default::default(),
                default_policy: default,
                call_timeout_secs: 30,
            })
            .unwrap()
            .id;
        self.mcp.connect(&id).await;
        assert!(
            self.mcp
                .list_views()
                .iter()
                .any(|v| v.config.id == id && !v.tools_cached),
            "mock server didn't connect"
        );
        id
    }

    fn model(&self) -> String {
        format!("{}/mock-model", self.provider)
    }

    fn snapshot(&self) -> Vec<Option<Vec<u8>>> {
        CONFIG_FILES
            .iter()
            .map(|f| std::fs::read(self.r.path.join(f)).ok())
            .collect()
    }

    async fn cli(&self, args: &[&str]) -> Output {
        let before = self.snapshot();
        let o = self.r.run_async(args).await;
        assert_eq!(
            self.snapshot(),
            before,
            "config files changed: {}",
            stderr(&o)
        );
        o
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_tools_are_denied_without_a_terminal() {
    let app = App::new().await;
    let (url, state) = mcp_mock(&["echo"]).await;
    app.server("mock", &url, ToolPolicy::Ask).await;
    let model = app.model();
    let o = app
        .cli(&["run", "--json", "--model", &model, "call mock__echo"])
        .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(tool_statuses(&events(&o)), vec![ToolCallStatus::Denied]);
    assert!(stderr(&o).contains("denied mock/echo"), "{}", stderr(&o));
    assert!(state.lock().unwrap().calls.is_empty());

    // On a terminal the user is asked: `script` gives the CLI a pseudo-terminal and the
    // answers are typed into it.
    #[cfg(target_os = "macos")]
    for (answer, calls) in [("y\n", 1), ("n\n", 1)] {
        use std::io::Write;
        let mut c = Command::new("script");
        c.args([
            "-q",
            "/dev/null",
            BIN,
            "run",
            "--model",
            &model,
            "call mock__echo",
        ])
        .env(data_dir_env_var(), &app.r.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let typed = answer.to_owned();
        let o = tokio::task::spawn_blocking(move || {
            let mut child = c.spawn().unwrap();
            let mut stdin = child.stdin.take().unwrap();
            std::thread::sleep(std::time::Duration::from_secs(2));
            stdin.write_all(typed.as_bytes()).unwrap();
            child.wait_with_output().unwrap()
        })
        .await
        .unwrap();
        let shown = String::from_utf8_lossy(&o.stdout);
        assert!(shown.contains("Allow tool mock/echo?"), "{shown}");
        assert_eq!(
            state.lock().unwrap().calls.len(),
            calls,
            "{answer:?}: {shown}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn allowed_tools_run_without_a_prompt_at_every_level() {
    let app = App::new().await;
    let (url, state) = mcp_mock(&["echo", "other"]).await;
    let id = app.server("mock", &url, ToolPolicy::Ask).await;
    let model = app.model();

    // Connection-level Allow (pinned against the live definition, as the app does).
    app.mcp.set_policy(&id, "echo", ToolPolicy::AlwaysAllow);
    let o = app
        .cli(&["run", "--model", &model, "call mock__echo"])
        .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(state.lock().unwrap().calls, vec!["echo"]);

    // Chat override.
    let chat = app
        .db
        .call(|c| chats_repo::create_chat(c, "t"))
        .await
        .unwrap()
        .id;
    let mut o = ChatToolOverrides::default();
    o.set_tool(&id, "other", Some(ToolPolicy::AlwaysAllow));
    let stored = o.clone();
    app.db
        .call(move |c| chats_repo::set_tool_overrides(c, chat, &stored))
        .await
        .unwrap();
    app.mcp
        .set_scope(PermissionScope::Chat(chat), ScopePolicies::Chat(o));
    let chat_arg = chat.to_string();
    let out = app
        .cli(&[
            "run",
            "--json",
            "--chat",
            &chat_arg,
            "--model",
            &model,
            "call mock__other",
        ])
        .await;
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        tool_statuses(&events(&out)),
        vec![ToolCallStatus::Completed]
    );

    // Agent permissions.
    let server = id.clone();
    let provider = app.provider.clone();
    let agent = app
        .db
        .call(move |c| {
            let mut access = AgentServerAccess {
                server_id: server,
                tools: None,
                default_policy: Some(ToolPolicy::Ask),
                policies: Default::default(),
            };
            access
                .policies
                .insert("other".into(), ToolPolicy::AlwaysAllow);
            agents_repo::save_agent(
                c,
                AgentInput {
                    id: None,
                    name: "Runner".into(),
                    description: String::new(),
                    default_model: ModelRef {
                        provider_id: provider,
                        model: "mock-model".into(),
                    },
                    fallback_model: None,
                    system_prompt: String::new(),
                    servers: vec![access],
                },
            )
        })
        .await
        .unwrap();
    app.mcp
        .set_scope(PermissionScope::Agent(agent.id), agent.scope_policies());
    let out = app
        .cli(&["run", "--json", "--agent", "runner", "call mock__other"])
        .await;
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        tool_statuses(&events(&out)),
        vec![ToolCallStatus::Completed]
    );
    // The agent's own Ask still applies to its other tools.
    let out = app
        .cli(&["run", "--json", "--agent", "Runner", "call mock__echo"])
        .await;
    assert_eq!(tool_statuses(&events(&out)), vec![ToolCallStatus::Denied]);
    assert_eq!(state.lock().unwrap().calls, vec!["echo", "other", "other"]);

    // Agent runs follow the agent's configuration.
    let out = app
        .cli(&["run", "--agent", "Runner", "--mcp", "mock", "x"])
        .await;
    assert_eq!(code(&out), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_definition_asks_and_pins_nothing() {
    let app = App::new().await;
    let (url, state) = mcp_mock(&["echo"]).await;
    let id = app.server("mock", &url, ToolPolicy::Ask).await;
    app.mcp.set_policy(&id, "echo", ToolPolicy::AlwaysAllow);
    state.lock().unwrap().tools[0]["description"] = "now it deletes things".into();
    let model = app.model();
    let o = app
        .cli(&["run", "--json", "--model", &model, "call mock__echo"])
        .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(tool_statuses(&events(&o)), vec![ToolCallStatus::Denied]);
    assert!(stderr(&o).contains("set to Allow"), "{}", stderr(&o));
    assert!(state.lock().unwrap().calls.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn selection_limits_servers_and_tools_and_is_stored_on_the_chat() {
    let app = App::new().await;
    let (url, mock) = mcp_mock(&["echo", "add"]).await;
    let (other_url, other) = mcp_mock(&["ping"]).await;
    let id = app.server("mock", &url, ToolPolicy::AlwaysAllow).await;
    let other_id = app
        .server("other", &other_url, ToolPolicy::AlwaysAllow)
        .await;
    let other_requests = other.lock().unwrap().requests;
    let model = app.model();

    let o = app
        .cli(&[
            "run",
            "--json",
            "--model",
            &model,
            "--tool",
            "mock/echo",
            "call mock__add",
        ])
        .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let offered = app.offered.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        offered,
        vec!["mock__echo".to_string()],
        "only the selected tool"
    );
    assert!(
        tool_statuses(&events(&o)).is_empty(),
        "the model couldn't call add"
    );
    assert_eq!(
        other.lock().unwrap().requests,
        other_requests,
        "unselected: not contacted"
    );
    let chat = chat_of(&o);
    let overrides = app
        .db
        .call(move |c| chats_repo::get_tool_overrides(c, chat))
        .await
        .unwrap();
    assert!(overrides.servers[&other_id].off);
    assert_eq!(
        overrides.servers[&id].policies.get("add"),
        Some(&ToolPolicy::Block)
    );
    assert!(!overrides.servers[&id].policies.contains_key("echo"));

    // A later `--chat` run keeps the selection; `--mcp` replaces it.
    let chat_arg = chat.to_string();
    let o = app
        .cli(&[
            "run",
            "--json",
            "--chat",
            &chat_arg,
            "--mcp",
            "other",
            "call other__ping",
        ])
        .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(tool_statuses(&events(&o)), vec![ToolCallStatus::Completed]);
    assert_eq!(other.lock().unwrap().calls, vec!["ping"]);
    assert!(mock.lock().unwrap().calls.is_empty());
    let o = app
        .cli(&[
            "run",
            "--json",
            "--chat",
            &chat_arg,
            "--no-mcp",
            "call other__ping",
        ])
        .await;
    assert!(tool_statuses(&events(&o)).is_empty());
    assert!(app.offered.lock().unwrap().last().unwrap().is_empty());

    // Selection can't widen anything.
    app.mcp.set_policy(&id, "add", ToolPolicy::Block);
    for (args, why) in [
        (vec!["--tool", "mock/add"], "disabled in Connections"),
        (vec!["--tool", "mock/missing"], "has no tool"),
        (vec!["--mcp", "nope"], "no MCP connection"),
    ] {
        let mut full = vec!["run", "--model", model.as_str()];
        full.extend(args);
        full.push("x");
        let o = app.cli(&full).await;
        assert_eq!(code(&o), 2, "{full:?}");
        assert!(stderr(&o).contains(why), "{}", stderr(&o));
    }
    app.mcp.store().set_enabled(&other_id, false).unwrap();
    let o = app
        .cli(&["run", "--model", &model, "--mcp", "other", "x"])
        .await;
    assert_eq!(code(&o), 2);
    assert!(
        stderr(&o).contains("turned off in Connections"),
        "{}",
        stderr(&o)
    );
}

/// Ctrl-C while a server is still connecting: the process stops cleanly, before a chat or
/// a turn exists.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_while_connecting_stops_before_anything_is_stored() {
    let app = App::new().await;
    // A server that accepts the connection and never answers.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/mcp",
        listener.local_addr().unwrap().port()
    );
    let (accepted_tx, accepted) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _sock = listener.accept().await;
        let _ = accepted_tx.send(());
        std::future::pending::<()>().await;
    });
    app.mcp
        .store()
        .save(McpServerInput {
            id: None,
            name: "silent".into(),
            transport: McpTransport::StreamableHttp,
            command: String::new(),
            env: Vec::new(),
            cwd: None,
            url,
            headers: Vec::new(),
            auth: Default::default(),
            default_policy: ToolPolicy::Ask,
            call_timeout_secs: 30,
        })
        .unwrap();
    let model = app.model();
    let child = app
        .r
        .cmd(&["run", "--model", &model, "hi"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    accepted.await.unwrap();
    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let o = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert_eq!(code(&o), 3, "{}", stderr(&o));
    assert!(stderr(&o).contains("cancelled"), "{}", stderr(&o));
    let chats = app
        .db
        .call(|c| chats_repo::list_chats(c, 10))
        .await
        .unwrap();
    assert!(chats.is_empty(), "nothing was stored");
}
