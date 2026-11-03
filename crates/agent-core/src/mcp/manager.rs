//! Connection manager: owns live MCP sessions, tool catalogs and dispatch. Implements the
//! agent loop's `ToolHost` seam so policy checks and calls happen here in Rust.
//!
//! Status changes are pushed to the host through an `on_change` callback (the Tauri layer
//! emits an app event; the UI refetches `list_mcp_servers`).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{NotificationContext, Peer, RunningService};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use super::config::{KnownTool, McpServerConfig, McpStore, McpTransport};
use super::{client_config, StdioServerSpec};
use crate::agent::context::{estimate_tokens, tools_prompt, ServerBrief};
use crate::agent::tools::{
    resolve_on_demand, RunTools, ToolEntry, ToolFilter, ToolFuture, ToolHost, ToolNamer, ToolOutput,
};
use crate::domain::{Message, ToolAccess, ToolPolicy};
use crate::providers::ToolSpec;

/// Wait for the MCP handshake + initial `tools/list` before marking a server connected.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a connected session is checked for unexpected transport loss.
const WATCHDOG_POLL: Duration = Duration::from_secs(2);
/// Keep the last N stderr lines of a stdio server for diagnostics.
const STDERR_LINES: usize = 40;
/// Max chars kept per stderr line; longer lines are truncated (and never grow the
/// read buffer unboundedly).
const MAX_STDERR_LINE: usize = 2_048;
/// Max tools offered per server; a misbehaving server can't flood the catalog.
const MAX_TOOLS_PER_SERVER: usize = 500;
/// Disabled tools named in the system prompt per server; beyond this they are
/// counted as "+N more".
const MAX_DISABLED_LISTED: usize = 16;
/// Bounds on untrusted server-provided metadata sent to the model.
const MAX_TOOL_DESC_CHARS: usize = 1_024;
const MAX_TOOL_SCHEMA_CHARS: usize = 16_384;

type Service = RunningService<RoleClient, AppClientHandler>;

/// Truncates, redacts known secret values, logs, and appends a stderr line to the bounded
/// tail the UI reads for diagnostics.
fn push_stderr_line(line: &str, tail: &Mutex<VecDeque<String>>, secrets: &[String], server: &str) {
    let mut line: String = line.chars().take(MAX_STDERR_LINE).collect();
    for s in secrets {
        if line.contains(s.as_str()) {
            line = line.replace(s.as_str(), "[redacted]");
        }
    }
    tracing::debug!(server = %server, "mcp stderr: {line}");
    let mut tail = tail.lock().expect("stderr lock");
    if tail.len() >= STDERR_LINES {
        tail.pop_front();
    }
    tail.push_back(line);
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum McpServerStatus {
    Disabled,
    Idle,
    Connecting,
    Connected {
        tool_count: u32,
    },
    /// Connection died or the handshake failed; `error` holds the reason.
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ToolInfo {
    /// Original tool name on the server.
    pub name: String,
    /// Namespaced name exposed to the model (`server__tool`).
    pub exposed: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub policy: ToolPolicy,
    /// From MCP annotation hints (untrusted metadata, shown for information only).
    pub access: ToolAccess,
    /// An `always_allow` grant exists but the tool's definition changed since it was
    /// granted; it currently behaves as `ask` until re-approved.
    #[serde(default)]
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpServerView {
    #[serde(flatten)]
    #[ts(flatten)]
    pub config: McpServerConfig,
    pub status: McpServerStatus,
    pub server_info: Option<String>,
    pub tools: Vec<ToolInfo>,
    /// `tools` is the last list seen (server disabled, offline or failed), not live.
    pub tools_cached: bool,
    /// Recent stderr lines for stdio servers that failed to start.
    pub stderr_tail: Vec<String>,
}

struct Live {
    service: Service,
    tools: Mutex<Vec<rmcp::model::Tool>>,
    server_info: String,
    instructions: Option<String>,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

#[derive(Default)]
struct Inner {
    status: HashMap<String, McpServerStatus>,
    live: HashMap<String, Arc<Live>>,
    /// Bumped on every connect/disconnect so a slow handshake finishing late can't install
    /// a session for a server that was disabled, deleted or reconnected meanwhile.
    epochs: HashMap<String, u64>,
}

/// sha256 over the fields that define what a tool does. `AlwaysAllow` grants record the
/// signature at grant time and are honored only while it matches — a server that changes
/// the tool's schema or description behind the grant falls back to `Ask`.
pub(crate) fn tool_signature(t: &rmcp::model::Tool) -> String {
    let mut h = Sha256::new();
    h.update(t.name.as_bytes());
    h.update(b"\0");
    if let Some(d) = t.description.as_deref() {
        h.update(d.as_bytes());
    }
    h.update(b"\0");
    h.update(
        serde_json::to_string(&t.input_schema)
            .unwrap_or_default()
            .as_bytes(),
    );
    h.update(b"\0");
    if let Some(ann) = &t.annotations {
        h.update(serde_json::to_string(ann).unwrap_or_default().as_bytes());
    }
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(h.finalize())
}

/// The cacheable summary of a live tool definition.
fn known_tool(t: &rmcp::model::Tool) -> KnownTool {
    let ann = t.annotations.as_ref();
    KnownTool {
        name: t.name.to_string(),
        title: t
            .title
            .clone()
            .or_else(|| ann.and_then(|a| a.title.clone())),
        description: t.description.as_deref().map(str::to_owned),
        access: ToolAccess::from_hints(
            ann.and_then(|a| a.read_only_hint),
            ann.and_then(|a| a.destructive_hint),
        ),
        signature: tool_signature(t),
    }
}

/// Client-side MCP handler: declares our identity/capabilities and forwards
/// `tools/list_changed` notifications to a channel the manager drains.
struct AppClientHandler {
    info: ClientConfig,
    tool_changed: mpsc::UnboundedSender<Peer<RoleClient>>,
}

impl ClientHandler for AppClientHandler {
    fn get_info(&self) -> ClientConfig {
        self.info.clone()
    }

    async fn on_tool_list_changed(&self, context: NotificationContext<RoleClient>) {
        let _ = self.tool_changed.send(context.peer.clone());
    }
}

pub struct McpManager {
    store: McpStore,
    inner: Mutex<Inner>,
    http: reqwest::Client,
    /// Runtime handle for spawning work — `tokio::spawn` alone panics outside runtime
    /// context (setup hook, sync commands run on the main thread).
    handle: tokio::runtime::Handle,
    on_change: Box<dyn Fn() + Send + Sync>,
}

impl McpManager {
    pub fn new(
        store: McpStore,
        http: reqwest::Client,
        handle: tokio::runtime::Handle,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            inner: Mutex::new(Inner::default()),
            http,
            handle,
            on_change: Box::new(on_change),
        })
    }

    fn notify(&self) {
        (self.on_change)();
    }

    pub fn store(&self) -> &McpStore {
        &self.store
    }

    fn set_status(&self, id: &str, status: McpServerStatus) {
        self.inner
            .lock()
            .expect("mcp lock")
            .status
            .insert(id.to_owned(), status);
        self.notify();
    }

    /// Starts a new connect generation for `id`: any earlier in-flight `establish` for this
    /// server becomes stale and must not install its session when it finishes.
    fn next_epoch(&self, id: &str) -> u64 {
        let mut inner = self.inner.lock().expect("mcp lock");
        let e = inner.epochs.entry(id.to_owned()).or_insert(0);
        *e += 1;
        *e
    }

    fn is_current(&self, id: &str, epoch: u64) -> bool {
        self.inner.lock().expect("mcp lock").epochs.get(id).copied() == Some(epoch)
    }

    /// Definition signature of the live tool, if the server is connected.
    fn current_signature(&self, server_id: &str, tool: &str) -> Option<String> {
        let live = self
            .inner
            .lock()
            .expect("mcp lock")
            .live
            .get(server_id)
            .cloned()?;
        let tools = live.tools.lock().expect("tools lock");
        tools.iter().find(|t| t.name == tool).map(tool_signature)
    }

    /// Persists a tool policy plus the definition signature it was granted against — live
    /// when connected, else the last listed one — so an `AlwaysAllow` grant stays
    /// verifiable even when made while the server is offline.
    pub fn set_policy(&self, server_id: &str, tool: &str, policy: ToolPolicy) {
        let sig = self
            .current_signature(server_id, tool)
            .or_else(|| self.store.cached_signature(server_id, tool));
        if let Err(e) = self.store.set_tool_policy(server_id, tool, policy, sig) {
            tracing::warn!(error = %e, "failed to persist tool policy");
        }
        self.notify();
    }

    /// Caches the listed tools (and pins newly seen default-allowed ones).
    fn record_catalog(&self, server_id: &str, tools: &[rmcp::model::Tool]) {
        if let Err(e) = self
            .store
            .set_catalog(server_id, tools.iter().map(known_tool).collect())
        {
            tracing::warn!(error = %e, "failed to persist tool catalog");
        }
    }

    /// Enabled servers whose first connection attempt hasn't settled (connected or failed).
    /// The scheduler waits for this to reach zero at startup so catch-up runs see tools.
    pub fn pending_connections(&self) -> usize {
        let inner = self.inner.lock().expect("mcp lock");
        self.store
            .list()
            .iter()
            .filter(|c| c.enabled)
            .filter(|c| {
                !matches!(
                    inner.status.get(&c.id),
                    Some(McpServerStatus::Connected { .. } | McpServerStatus::Failed { .. })
                )
            })
            .count()
    }

    /// Cancels every session; called at app exit. Process-group kill-on-drop guarantees
    /// child processes die even while the runtime is tearing down.
    pub fn shutdown(&self) {
        let lives: Vec<Arc<Live>> = {
            let mut inner = self.inner.lock().expect("mcp lock");
            inner.epochs.clear();
            inner.status.clear();
            inner.live.drain().map(|(_, l)| l).collect()
        };
        for live in lives {
            live.service.cancellation_token().cancel();
        }
    }

    /// Snapshot for the UI: every configured server with live status, tools and diagnostics.
    pub fn list_views(&self) -> Vec<McpServerView> {
        let inner = self.inner.lock().expect("mcp lock");
        self.store
            .list()
            .into_iter()
            .map(|config| {
                let live = inner.live.get(&config.id);
                let known: Vec<KnownTool> = match live {
                    Some(l) => l
                        .tools
                        .lock()
                        .expect("tools lock")
                        .iter()
                        .map(known_tool)
                        .collect(),
                    None => self.store.catalog(&config.id),
                };
                let mut namer = ToolNamer::new();
                let tools = known
                    .into_iter()
                    .map(|t| {
                        let policy = self.store.policy_for(&config.id, &t.name);
                        ToolInfo {
                            exposed: namer.name(&config.name, &t.name),
                            policy,
                            access: t.access,
                            changed: policy == ToolPolicy::AlwaysAllow
                                && self.store.grant_stale(&config.id, &t.name, &t.signature),
                            name: t.name,
                            title: t.title,
                            description: t.description,
                        }
                    })
                    .collect();
                McpServerView {
                    tools_cached: live.is_none(),
                    status: if config.enabled {
                        inner
                            .status
                            .get(&config.id)
                            .cloned()
                            .unwrap_or(McpServerStatus::Idle)
                    } else {
                        McpServerStatus::Disabled
                    },
                    server_info: live.map(|l| l.server_info.clone()),
                    tools,
                    stderr_tail: live
                        .map(|l| {
                            l.stderr
                                .lock()
                                .expect("stderr lock")
                                .iter()
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default(),
                    config,
                }
            })
            .collect()
    }

    /// Connects all enabled servers that aren't already connected, in the background.
    /// Called once at startup.
    pub fn autoconnect(self: &Arc<Self>) {
        // The first login-shell PATH probe can block ~2s; warm the cache off the async
        // request path so the first connect doesn't stall a runtime worker.
        self.handle.spawn(async {
            let _ = tokio::task::spawn_blocking(super::stdio::prewarm).await;
        });
        let ids: Vec<String> = {
            let inner = self.inner.lock().expect("mcp lock");
            self.store
                .list()
                .into_iter()
                .filter(|s| s.enabled && !inner.live.contains_key(&s.id))
                .map(|s| s.id)
                .collect()
        };
        for id in ids {
            let this = Arc::clone(self);
            self.handle.spawn(async move { this.connect(&id).await });
        }
    }

    /// Disconnects and drops the session, if any. Also invalidates an in-flight connect:
    /// when it finishes it will see a stale epoch and drop the session instead of
    /// installing it.
    pub fn disconnect(&self, id: &str) {
        let live = {
            let mut inner = self.inner.lock().expect("mcp lock");
            *inner.epochs.entry(id.to_owned()).or_insert(0) += 1;
            inner.status.remove(id);
            inner.live.remove(id)
        };
        if let Some(live) = live {
            // Cancel in the background; dropping the Arc closes the transport for real
            // once in-flight calls finish holding it.
            self.handle.spawn(async move {
                live.service.cancellation_token().cancel();
            });
        }
        if self.store.get(id).is_ok() {
            self.set_status(id, McpServerStatus::Idle);
        } else {
            self.notify();
        }
    }

    /// (Re)connects a single server: handshake, tool listing, catalog install. Errors are
    /// recorded as `Failed` status rather than returned — this usually runs on background
    /// tasks and the UI reads the status. A connect that finishes after the server was
    /// disabled, deleted or reconnected drops its session instead of installing it.
    pub async fn connect(self: &Arc<Self>, id: &str) {
        let config = match self.store.get(id) {
            Ok(c) if c.enabled => c,
            _ => return,
        };
        self.disconnect(id);
        let epoch = self.next_epoch(id);
        self.set_status(id, McpServerStatus::Connecting);

        match self.establish(&config, epoch).await {
            Ok((live, tool_changed_rx)) => {
                if !self.is_current(id, epoch)
                    || !self.store.get(id).map(|c| c.enabled).unwrap_or(false)
                {
                    // Superseded while establishing; dropping `live` cancels the service
                    // and kills the child process.
                    return;
                }
                let live = Arc::new(live);
                let count = {
                    let tools = live.tools.lock().expect("tools lock");
                    self.record_catalog(id, &tools);
                    tools.len() as u32
                };
                self.inner
                    .lock()
                    .expect("mcp lock")
                    .live
                    .insert(id.to_owned(), Arc::clone(&live));
                self.set_status(id, McpServerStatus::Connected { tool_count: count });
                self.spawn_watchdog(id.to_owned(), Arc::clone(&live));
                self.spawn_tool_change_listener(
                    id.to_owned(),
                    Arc::downgrade(&live),
                    tool_changed_rx,
                );
            }
            Err(error) => {
                if self.is_current(id, epoch) {
                    self.set_status(id, McpServerStatus::Failed { error });
                }
            }
        }
    }

    /// Marks the server `Failed` if its transport dies (server exit, network drop). Bound
    /// to a specific `Live` so a watchdog can't outlive and flag a newer session.
    fn spawn_watchdog(self: &Arc<Self>, id: String, live: Arc<Live>) {
        let this = Arc::clone(self);
        self.handle.spawn(async move {
            loop {
                tokio::time::sleep(WATCHDOG_POLL).await;
                let current = this.inner.lock().expect("mcp lock").live.get(&id).cloned();
                match current {
                    Some(cur) if Arc::ptr_eq(&cur, &live) => {}
                    _ => return, // removed or replaced — this session is no longer ours
                }
                if live.service.is_closed() || live.service.is_transport_closed() {
                    let removed = {
                        let mut inner = this.inner.lock().expect("mcp lock");
                        match inner.live.get(&id) {
                            Some(cur) if Arc::ptr_eq(cur, &live) => {
                                inner.live.remove(&id).is_some()
                            }
                            _ => false,
                        }
                    };
                    if removed {
                        this.set_status(
                            &id,
                            McpServerStatus::Failed {
                                error:
                                    "connection lost — the server exited or closed the transport"
                                        .into(),
                            },
                        );
                    }
                    return;
                }
            }
        });
    }

    /// Applies `tools/list_changed` notifications to the live catalog, coalescing bursts
    /// into a single refetch. Bound to the session via `Weak<Live>`.
    fn spawn_tool_change_listener(
        self: &Arc<Self>,
        id: String,
        live: Weak<Live>,
        mut rx: mpsc::UnboundedReceiver<Peer<RoleClient>>,
    ) {
        let this = Arc::clone(self);
        self.handle.spawn(async move {
            while let Some(peer) = rx.recv().await {
                while rx.try_recv().is_ok() {}
                let Some(live) = live.upgrade() else { return };
                let still_current = this
                    .inner
                    .lock()
                    .expect("mcp lock")
                    .live
                    .get(&id)
                    .is_some_and(|cur| Arc::ptr_eq(cur, &live));
                if !still_current {
                    return;
                }
                let tools = tokio::time::timeout(CONNECT_TIMEOUT, peer.list_all_tools())
                    .await
                    .ok()
                    .and_then(std::result::Result::ok);
                if let Some(tools) = tools {
                    let still_current = this
                        .inner
                        .lock()
                        .expect("mcp lock")
                        .live
                        .get(&id)
                        .is_some_and(|cur| Arc::ptr_eq(cur, &live));
                    if !still_current {
                        return;
                    }
                    let count = {
                        let mut t = live.tools.lock().expect("tools lock");
                        *t = tools;
                        this.record_catalog(&id, &t);
                        t.len() as u32
                    };
                    this.set_status(&id, McpServerStatus::Connected { tool_count: count });
                }
            }
        });
    }

    /// Performs transport setup + handshake + initial tool list for one server.
    /// `epoch` ties this attempt to a connect generation; if the server was disconnected
    /// meanwhile the partially-established session is abandoned.
    async fn establish(
        &self,
        config: &McpServerConfig,
        epoch: u64,
    ) -> std::result::Result<(Live, mpsc::UnboundedReceiver<Peer<RoleClient>>), String> {
        let (tool_changed_tx, tool_changed_rx) = mpsc::unbounded_channel::<Peer<RoleClient>>();
        let stderr = Arc::new(Mutex::new(VecDeque::<String>::new()));
        let handler = AppClientHandler {
            info: client_config(),
            tool_changed: tool_changed_tx,
        };

        let service = match config.transport {
            McpTransport::Stdio => {
                let env =
                    super::stdio::build_env(&self.store, config).map_err(|e| e.to_string())?;
                // Known secret values, scrubbed from stderr before it reaches the log or UI.
                let secrets = self.store.secret_values(config);
                let spec = StdioServerSpec {
                    command: super::stdio::expand_tilde(&config.command),
                    // `~` in paths can't be expanded by the child (no shell runs).
                    args: config
                        .args
                        .iter()
                        .map(|a| super::stdio::expand_tilde(a))
                        .collect(),
                    cwd: config
                        .cwd
                        .as_deref()
                        .map(super::stdio::expand_tilde)
                        .map(std::path::PathBuf::from),
                    env,
                };
                // Windows: `Command::new("npx")` won't find `npx.cmd` via PATHEXT.
                #[cfg(windows)]
                let mut cmd = rmcp::transport::which_command(&spec.command)
                    .unwrap_or_else(|_| tokio::process::Command::new(&spec.command));
                #[cfg(not(windows))]
                let mut cmd = tokio::process::Command::new(&spec.command);
                cmd.args(&spec.args).env_clear().envs(&spec.env);
                if let Some(cwd) = &spec.cwd {
                    cmd.current_dir(cwd);
                }
                // Wrap so kill()/drop kills the whole process tree (e.g. `npx` → `node`),
                // not just the direct child.
                let mut cmd = process_wrap::tokio::CommandWrap::from(cmd);
                #[cfg(unix)]
                cmd.wrap(process_wrap::tokio::ProcessGroup::leader());
                #[cfg(windows)]
                cmd.wrap(process_wrap::tokio::JobObject);
                cmd.wrap(process_wrap::tokio::KillOnDrop);
                let (transport, stderr_stream) = TokioChildProcess::builder(cmd)
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| format!("failed to launch `{}`: {e}", spec.command))?;
                // Drain stderr into a bounded, redacted tail for diagnostics + the log.
                if let Some(mut err) = stderr_stream {
                    let tail_buf = stderr.clone();
                    let server = config.name.clone();
                    self.handle.spawn(async move {
                        use tokio::io::AsyncReadExt;
                        let mut buf = [0u8; 8192];
                        let mut pending = String::new();
                        loop {
                            let n = match err.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => n,
                            };
                            pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                            while let Some(pos) = pending.find('\n') {
                                let mut line: String = pending.drain(..=pos).collect();
                                line.pop();
                                push_stderr_line(&line, &tail_buf, &secrets, &server);
                            }
                            if pending.chars().count() > MAX_STDERR_LINE {
                                let line: String = pending.chars().take(MAX_STDERR_LINE).collect();
                                pending.clear();
                                push_stderr_line(&line, &tail_buf, &secrets, &server);
                            }
                        }
                        if !pending.trim().is_empty() {
                            push_stderr_line(&pending, &tail_buf, &secrets, &server);
                        }
                    });
                }
                tokio::time::timeout(CONNECT_TIMEOUT, handler.serve(transport))
                    .await
                    .map_err(|_| {
                        let tail = stderr.lock().expect("stderr lock");
                        if tail.is_empty() {
                            format!("handshake timed out after {CONNECT_TIMEOUT:?}")
                        } else {
                            format!(
                                "handshake timed out after {CONNECT_TIMEOUT:?}: {}",
                                tail.iter().cloned().collect::<Vec<_>>().join(" | ")
                            )
                        }
                    })?
                    .map_err(|e| format!("handshake failed: {e}"))?
            }
            McpTransport::StreamableHttp => {
                let (_, headers) = self
                    .store
                    .resolved_entries(config)
                    .map_err(|e| e.to_string())?;
                let mut cfg = StreamableHttpClientTransportConfig::with_uri(config.url.clone());
                let mut map = HashMap::new();
                for (k, v) in headers {
                    let name = k
                        .parse::<reqwest::header::HeaderName>()
                        .map_err(|_| format!("invalid header name `{k}`"))?;
                    let value = v
                        .parse::<reqwest::header::HeaderValue>()
                        .map_err(|_| format!("invalid header value for `{k}`"))?;
                    map.insert(name, value);
                }
                if !map.is_empty() {
                    cfg = cfg.custom_headers(map);
                }
                let transport = StreamableHttpClientTransport::with_client(self.http.clone(), cfg);
                tokio::time::timeout(CONNECT_TIMEOUT, handler.serve(transport))
                    .await
                    .map_err(|_| format!("handshake timed out after {CONNECT_TIMEOUT:?}"))?
                    .map_err(|e| format!("handshake failed: {e}"))?
            }
        };

        if !self.is_current(&config.id, epoch) {
            return Err("connect superseded".into());
        }
        let tools = tokio::time::timeout(CONNECT_TIMEOUT, service.list_all_tools())
            .await
            .map_err(|_| "tools/list timed out".to_owned())?
            .map_err(|e| format!("tools/list failed: {e}"))?;
        if !self.is_current(&config.id, epoch) {
            return Err("connect superseded".into());
        }
        let info = service.peer_info();
        let server_info = info
            .as_ref()
            .and_then(|i| i.server_info.as_ref())
            .map(|s| format!("{} {}", s.name, s.version))
            .unwrap_or_else(|| config.name.clone());
        let instructions = info.as_ref().and_then(|i| i.instructions.clone());

        Ok((
            Live {
                service,
                tools: Mutex::new(tools),
                server_info,
                instructions,
                stderr,
            },
            tool_changed_rx,
        ))
    }

    /// The run-facing snapshot: a catalog of non-blocked tools from connected, enabled
    /// servers plus the system-prompt addition and the estimated definition cost.
    /// `history` restores the on-demand loaded set. `filter` (agents) restricts the catalog
    /// to some servers/tools. Returns `None` when nothing is callable.
    pub fn prepare_tools(
        self: &Arc<Self>,
        history: &[Message],
        context_window: u32,
        filter: Option<&ToolFilter>,
    ) -> Option<(RunTools, String, u32)> {
        if filter.is_some_and(ToolFilter::is_empty) {
            return None;
        }
        let settings = self.store.settings();
        let mut catalog = Vec::new();
        let mut briefs = Vec::new();
        let mut all_disabled = Vec::new();
        let mut namer = ToolNamer::new();
        {
            let inner = self.inner.lock().expect("mcp lock");
            for config in self.store.list() {
                if !config.enabled {
                    continue;
                }
                let Some(live) = inner.live.get(&config.id) else {
                    continue;
                };
                let in_filter = |tool: &str| filter.is_none_or(|f| f.allows_tool(&config.id, tool));
                if filter.is_some_and(|f| !f.allows_server(&config.id)) {
                    // Still feed the namer so exposed names match unfiltered runs.
                    for t in live.tools.lock().expect("tools lock").iter() {
                        namer.name(&config.name, &t.name);
                    }
                    continue;
                }
                let mut count = 0usize;
                let mut skipped = 0usize;
                let mut disabled = Vec::new();
                let mut disabled_extra = 0usize;
                for t in live.tools.lock().expect("tools lock").iter() {
                    // Name every tool in list order so exposed names stay stable when tools
                    // are disabled or skipped.
                    let exposed = namer.name(&config.name, &t.name);
                    if !in_filter(&t.name) {
                        continue;
                    }
                    if self.store.policy_for(&config.id, &t.name) == ToolPolicy::Block {
                        // Named but never defined: the model can explain it's disabled.
                        if disabled.len() < MAX_DISABLED_LISTED {
                            disabled.push(exposed.clone());
                        } else {
                            disabled_extra += 1;
                        }
                        all_disabled.push(exposed);
                        continue;
                    }
                    if count >= MAX_TOOLS_PER_SERVER {
                        continue;
                    }
                    let schema_json = serde_json::to_string(&t.input_schema).unwrap_or_default();
                    if schema_json.len() > MAX_TOOL_SCHEMA_CHARS {
                        skipped += 1;
                        continue;
                    }
                    // Server-provided descriptions are untrusted content; bound them.
                    let description: String = t
                        .description
                        .as_deref()
                        .or(t.title.as_deref())
                        .unwrap_or("")
                        .chars()
                        .take(MAX_TOOL_DESC_CHARS)
                        .collect();
                    let def_tokens = estimate_tokens(&exposed)
                        + estimate_tokens(&description)
                        + estimate_tokens(&schema_json)
                        + 8;
                    count += 1;
                    catalog.push(ToolEntry {
                        spec: ToolSpec {
                            name: exposed,
                            description,
                            parameters: Value::Object((*t.input_schema).clone()),
                        },
                        server_id: config.id.clone(),
                        server_name: config.name.clone(),
                        tool: t.name.to_string(),
                        def_tokens,
                    });
                }
                if skipped > 0 {
                    tracing::warn!(server = %config.name, skipped, "skipped tools with oversized schemas");
                }
                if count == 0 && disabled.is_empty() && disabled_extra == 0 && filter.is_some() {
                    continue;
                }
                briefs.push(ServerBrief {
                    name: config.name.clone(),
                    tool_count: count,
                    instructions: live.instructions.clone(),
                    disabled,
                    disabled_extra,
                });
            }
        }
        if catalog.is_empty() {
            return None;
        }
        let on_demand = resolve_on_demand(&settings, &catalog, context_window);
        let tools = RunTools::new(
            Arc::clone(self) as Arc<dyn ToolHost>,
            catalog,
            on_demand,
            settings.search_limit as usize,
            history,
            context_window,
        )
        .with_disabled(all_disabled);
        let def_tokens = tools.definition_tokens();
        Some((tools, tools_prompt(&briefs, on_demand), def_tokens))
    }
}

/// `McpManager` implements `ToolHost`; runs hold it as `Arc<dyn ToolHost>`.
impl ToolHost for McpManager {
    fn policy(&self, server_id: &str, tool: &str) -> ToolPolicy {
        let sig = self.current_signature(server_id, tool);
        self.store.effective_policy(server_id, tool, sig.as_deref())
    }

    fn set_policy(&self, server_id: &str, tool: &str, policy: ToolPolicy) {
        McpManager::set_policy(self, server_id, tool, policy)
    }

    /// `call_tool` abandons its request on drop, which makes rmcp send
    /// `notifications/cancelled` — so the coordinator's timeout/cancel path (which drops
    /// this future) already notifies the server; `_cancel` only short-circuits locally.
    fn call(
        &self,
        server_id: &str,
        tool: &str,
        arguments: Value,
        _cancel: CancellationToken,
    ) -> ToolFuture {
        let live = self
            .inner
            .lock()
            .expect("mcp lock")
            .live
            .get(server_id)
            .cloned();
        let tool = tool.to_owned();
        let timeout = self
            .store
            .get(server_id)
            .map(|c| Duration::from_secs(c.call_timeout_secs.max(1)))
            .unwrap_or_else(|_| Duration::from_secs(120));
        Box::pin(async move {
            let Some(live) = live else {
                return ToolOutput::text(
                    "the server is not connected (reconnect it in Connections)",
                    true,
                );
            };
            let mut params = CallToolRequestParams::new(tool.clone());
            if let Some(args) = arguments.as_object().cloned() {
                params = params.with_arguments(args);
            }
            match tokio::time::timeout(timeout, live.service.call_tool(params)).await {
                Err(_) => {
                    ToolOutput::text(format!("tool `{tool}` timed out after {timeout:?}"), true)
                }
                Ok(Err(e)) => ToolOutput::text(
                    if live.service.is_transport_closed() {
                        format!("tool `{tool}` failed: the server disconnected")
                    } else {
                        format!("tool `{tool}` failed: {e}")
                    },
                    true,
                ),
                Ok(Ok(result)) => {
                    let content = serde_json::to_value(&result.content)
                        .unwrap_or_else(|_| Value::Array(Vec::new()));
                    let is_error = result.is_error.unwrap_or(false);
                    if content.as_array().is_some_and(|a| !a.is_empty()) {
                        ToolOutput { content, is_error }
                    } else if let Some(structured) = &result.structured_content {
                        ToolOutput::text(structured.to_string(), is_error)
                    } else {
                        ToolOutput::text("(tool returned no content)", is_error)
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolve_data_root;
    use crate::secrets::MemorySecretStore;

    fn manager() -> Arc<McpManager> {
        let tmp = std::env::temp_dir().join(format!("opengent-mgr-{}", uuid::Uuid::new_v4()));
        let root = resolve_data_root(Some(&tmp)).unwrap();
        let store = McpStore::load(&root, Arc::new(MemorySecretStore::default())).unwrap();
        McpManager::new(
            store,
            crate::http::client().unwrap(),
            tokio::runtime::Handle::current(),
            || {},
        )
    }

    #[tokio::test]
    async fn disconnect_invalidates_an_in_flight_connect() {
        let m = manager();
        let epoch = m.next_epoch("s");
        assert!(m.is_current("s", epoch));
        m.disconnect("s");
        assert!(
            !m.is_current("s", epoch),
            "a superseded connect must fail the currency check"
        );
        assert!(!m.is_current("never-connected", u64::MAX));
    }

    #[test]
    fn stderr_tail_redacts_secrets_and_caps_lines() {
        let tail = Mutex::new(VecDeque::new());
        let secrets = vec!["sekret-token".to_string()];
        push_stderr_line("boom sekret-token happened", &tail, &secrets, "s");
        let long = "x".repeat(MAX_STDERR_LINE + 64);
        push_stderr_line(&long, &tail, &secrets, "s");
        let t = tail.lock().unwrap();
        assert!(t[0].contains("[redacted]") && !t[0].contains("sekret-token"));
        assert_eq!(t[1].chars().count(), MAX_STDERR_LINE);
    }
}
