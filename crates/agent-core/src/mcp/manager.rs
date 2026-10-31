//! Connection manager: owns live MCP sessions, tool catalogs and dispatch. Implements the
//! agent loop's `ToolHost` seam so policy checks and calls happen here in Rust.
//!
//! Status changes are pushed to the host through an `on_change` callback (the Tauri layer
//! emits an app event; the UI refetches `list_mcp_servers`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
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
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use super::config::{McpServerConfig, McpStore, McpTransport};
use super::{client_config, StdioServerSpec};
use crate::agent::context::{tools_prompt, ServerBrief};
use crate::agent::tools::{
    resolve_on_demand, RunTools, ToolEntry, ToolFuture, ToolHost, ToolNamer, ToolOutput,
};
use crate::domain::{Message, ToolPolicy};
use crate::providers::ToolSpec;

/// Wait for the MCP handshake + initial `tools/list` before marking a server connected.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a connected session is checked for unexpected transport loss.
const WATCHDOG_POLL: Duration = Duration::from_secs(2);
/// Keep the last N stderr lines of a stdio server for diagnostics.
const STDERR_LINES: usize = 40;

type Service = RunningService<RoleClient, AppClientHandler>;

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
    /// MCP annotation hints (untrusted metadata, shown for information only).
    pub read_only: bool,
    pub destructive: bool,
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
    /// Recent stderr lines for stdio servers that failed to start.
    pub stderr_tail: Vec<String>,
}

struct Live {
    service: Service,
    tools: Mutex<Vec<rmcp::model::Tool>>,
    server_info: String,
    instructions: Option<String>,
    stderr: Arc<Mutex<Vec<String>>>,
}

#[derive(Default)]
struct Inner {
    status: HashMap<String, McpServerStatus>,
    live: HashMap<String, Arc<Live>>,
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

    /// Snapshot for the UI: every configured server with live status, tools and diagnostics.
    pub fn list_views(&self) -> Vec<McpServerView> {
        let inner = self.inner.lock().expect("mcp lock");
        self.store
            .list()
            .into_iter()
            .map(|config| {
                let live = inner.live.get(&config.id);
                let tools = live
                    .map(|l| {
                        let mut namer = ToolNamer::new();
                        l.tools
                            .lock()
                            .expect("tools lock")
                            .iter()
                            .map(|t| {
                                let ann = t.annotations.as_ref();
                                ToolInfo {
                                    name: t.name.to_string(),
                                    exposed: namer.name(&config.name, &t.name),
                                    title: t
                                        .title
                                        .clone()
                                        .or_else(|| ann.and_then(|a| a.title.clone())),
                                    description: t.description.as_deref().map(str::to_owned),
                                    policy: self.store.policy_for(&config.id, &t.name),
                                    read_only: ann.and_then(|a| a.read_only_hint).unwrap_or(false),
                                    destructive: ann
                                        .and_then(|a| a.destructive_hint)
                                        .unwrap_or(true),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                McpServerView {
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
                        .map(|l| l.stderr.lock().expect("stderr lock").clone())
                        .unwrap_or_default(),
                    config,
                }
            })
            .collect()
    }

    /// Connects all enabled servers that aren't already connected, in the background.
    /// Called once at startup.
    pub fn autoconnect(self: &Arc<Self>) {
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

    /// Disconnects and drops the session, if any.
    pub fn disconnect(&self, id: &str) {
        let live = self.inner.lock().expect("mcp lock").live.remove(id);
        if let Some(live) = live {
            // Cancel in the background; dropping the Arc closes the transport for real
            // once in-flight calls finish holding it.
            self.handle.spawn(async move {
                live.service.cancellation_token().cancel();
            });
        }
        if self.store.get(id).is_ok() {
            self.set_status(id, McpServerStatus::Idle);
        }
    }

    /// (Re)connects a single server: handshake, tool listing, catalog install. Errors are
    /// recorded as `Failed` status rather than returned — this usually runs on background
    /// tasks and the UI reads the status.
    pub async fn connect(self: &Arc<Self>, id: &str) {
        let config = match self.store.get(id) {
            Ok(c) if c.enabled => c,
            _ => return,
        };
        self.disconnect(id);
        self.set_status(id, McpServerStatus::Connecting);

        match self.establish(&config).await {
            Ok((live, tool_changed_rx)) => {
                let count = live.tools.lock().expect("tools lock").len() as u32;
                self.inner
                    .lock()
                    .expect("mcp lock")
                    .live
                    .insert(id.to_owned(), Arc::new(live));
                self.set_status(id, McpServerStatus::Connected { tool_count: count });
                self.spawn_watchdog(id.to_owned());
                self.spawn_tool_change_listener(id.to_owned(), tool_changed_rx);
            }
            Err(error) => {
                self.set_status(id, McpServerStatus::Failed { error });
            }
        }
    }

    /// Marks the server `Failed` if its transport dies (server exit, network drop).
    fn spawn_watchdog(self: &Arc<Self>, id: String) {
        let this = Arc::clone(self);
        self.handle.spawn(async move {
            loop {
                tokio::time::sleep(WATCHDOG_POLL).await;
                let live = this.inner.lock().expect("mcp lock").live.get(&id).cloned();
                let Some(live) = live else { return };
                if live.service.is_closed() || live.service.is_transport_closed() {
                    if this
                        .inner
                        .lock()
                        .expect("mcp lock")
                        .live
                        .remove(&id)
                        .is_some()
                    {
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

    /// Applies `tools/list_changed` notifications to the live catalog.
    fn spawn_tool_change_listener(
        self: &Arc<Self>,
        id: String,
        mut rx: mpsc::UnboundedReceiver<Peer<RoleClient>>,
    ) {
        let this = Arc::clone(self);
        self.handle.spawn(async move {
            while let Some(peer) = rx.recv().await {
                let tools = tokio::time::timeout(CONNECT_TIMEOUT, peer.list_all_tools())
                    .await
                    .ok()
                    .and_then(std::result::Result::ok);
                if let Some(tools) = tools {
                    let count = {
                        let inner = this.inner.lock().expect("mcp lock");
                        match inner.live.get(&id) {
                            Some(l) => {
                                let mut t = l.tools.lock().expect("tools lock");
                                *t = tools;
                                t.len() as u32
                            }
                            None => return,
                        }
                    };
                    this.set_status(&id, McpServerStatus::Connected { tool_count: count });
                }
            }
        });
    }

    /// Performs transport setup + handshake + initial tool list for one server.
    async fn establish(
        &self,
        config: &McpServerConfig,
    ) -> std::result::Result<(Live, mpsc::UnboundedReceiver<Peer<RoleClient>>), String> {
        let (tool_changed_tx, tool_changed_rx) = mpsc::unbounded_channel::<Peer<RoleClient>>();
        let stderr = Arc::new(Mutex::new(Vec::<String>::new()));
        let handler = AppClientHandler {
            info: client_config(),
            tool_changed: tool_changed_tx,
        };

        let service = match config.transport {
            McpTransport::Stdio => {
                let env = super::stdio::build_env(&self.store, config);
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
                let mut cmd = tokio::process::Command::new(&spec.command);
                cmd.args(&spec.args).env_clear().envs(&spec.env);
                if let Some(cwd) = &spec.cwd {
                    cmd.current_dir(cwd);
                }
                let (transport, stderr_stream) = TokioChildProcess::builder(cmd)
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| format!("failed to launch `{}`: {e}", spec.command))?;
                // Drain stderr into a bounded tail for diagnostics + the log.
                if let Some(mut err) = stderr_stream {
                    let tail_buf = stderr.clone();
                    let server = config.name.clone();
                    self.handle.spawn(async move {
                        use tokio::io::AsyncBufReadExt;
                        let mut lines = tokio::io::BufReader::new(&mut err).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            tracing::debug!(server = %server, "mcp stderr: {line}");
                            let mut tail = tail_buf.lock().expect("stderr lock");
                            if tail.len() >= STDERR_LINES {
                                tail.remove(0);
                            }
                            tail.push(line);
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
                                tail.join(" | ")
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

        let tools = tokio::time::timeout(CONNECT_TIMEOUT, service.list_all_tools())
            .await
            .map_err(|_| "tools/list timed out".to_owned())?
            .map_err(|e| format!("tools/list failed: {e}"))?;
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
    /// servers plus the system-prompt addition. `history` restores the on-demand loaded set.
    /// Returns `None` when nothing is callable.
    pub fn prepare_tools(
        self: &Arc<Self>,
        history: &[Message],
        context_window: u32,
    ) -> Option<(RunTools, String)> {
        let settings = self.store.settings();
        let mut catalog = Vec::new();
        let mut briefs = Vec::new();
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
                let mut count = 0usize;
                for t in live.tools.lock().expect("tools lock").iter() {
                    if self.store.policy_for(&config.id, &t.name) == ToolPolicy::Block {
                        continue;
                    }
                    count += 1;
                    catalog.push(ToolEntry {
                        spec: ToolSpec {
                            name: namer.name(&config.name, &t.name),
                            description: t
                                .description
                                .as_deref()
                                .or(t.title.as_deref())
                                .unwrap_or("")
                                .to_owned(),
                            parameters: Value::Object((*t.input_schema).clone()),
                        },
                        server_id: config.id.clone(),
                        server_name: config.name.clone(),
                        tool: t.name.to_string(),
                    });
                }
                briefs.push(ServerBrief {
                    name: config.name.clone(),
                    tool_count: count,
                    instructions: live.instructions.clone(),
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
        );
        Some((tools, tools_prompt(&briefs, on_demand)))
    }
}

/// `McpManager` implements `ToolHost`; runs hold it as `Arc<dyn ToolHost>`.
impl ToolHost for McpManager {
    fn policy(&self, server_id: &str, tool: &str) -> ToolPolicy {
        self.store.policy_for(server_id, tool)
    }

    fn set_policy(&self, server_id: &str, tool: &str, policy: ToolPolicy) {
        if let Err(e) = self.store.set_tool_policy(server_id, tool, policy) {
            tracing::warn!(error = %e, "failed to persist tool policy");
        }
        self.notify();
    }

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
