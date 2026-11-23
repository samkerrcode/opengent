//! `run`: one non-interactive run through `launcher::launch`. The reply goes to stdout,
//! notices and tool activity to stderr; `--json` prints every `RunEvent` as a JSON line.
//!
//! Stored permissions apply exactly as in the app; the CLI adds no permission model of its
//! own. Tools set to Allow run without a prompt (verified against the pin the app stored);
//! Ask prompts on the terminal when someone can answer, and is denied otherwise. The
//! "Always allow" choice is app-only: it would persist policy and pins.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{IsTerminal, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_core::agent::launcher::{self, RunParams, RunServices, MAX_PROMPT_CHARS};
use agent_core::agent::{RunCoordinator, RunSink};
use agent_core::agents::{Agent, ModelRef};
use agent_core::config::{product, resolve_data_root, DataRoot};
use agent_core::domain::{
    ApprovalDecision, ChatId, RunEvent, RunEventPayload, ToolCallInfo, ToolCallStatus, ToolPolicy,
};
use agent_core::mcp::{
    ChatToolOverrides, McpManager, McpServerConfig, McpServerStatus, McpStore, PermissionScope,
    ScopePolicies,
};
use agent_core::providers::registry::{ProviderConfig, ProviderStore};
use agent_core::settings::SettingsStore;
use agent_core::storage::agents as agents_repo;
use agent_core::storage::chats as chats_repo;
use agent_core::storage::{AttachmentStore, ChatSummary, Db, MessageStatus, ProcessLock};
use tokio::sync::oneshot;

use crate::args::{self, Arg, Args};
use crate::schedule::find_agent;
use crate::term::{self, note};
use crate::{
    http_client, secret_store, CliError, Result, EXIT_CANCELLED, EXIT_FAILED, EXIT_INTERRUPTED,
};

/// How long the selected MCP servers get to connect before the run starts without them.
const CONNECT_WAIT: Duration = Duration::from_secs(30);
/// After the first Ctrl-C, how long to wait for the cancellation to be stored.
const CANCEL_WAIT: Duration = Duration::from_secs(10);
/// Events buffered for the writer thread before the run waits for it.
const OUTPUT_QUEUE: usize = 256;

#[derive(Debug, Default)]
pub struct RunOptions {
    model: Option<String>,
    agent: Option<String>,
    chat: Option<String>,
    mcp: Vec<String>,
    tools: Vec<String>,
    no_mcp: bool,
    json: bool,
    prompt: String,
}

impl RunOptions {
    fn selecting(&self) -> bool {
        self.no_mcp || !self.mcp.is_empty() || !self.tools.is_empty()
    }
}

pub fn parse(args: &mut Args) -> Result<RunOptions> {
    let mut o = RunOptions::default();
    let mut words = Vec::new();
    while let Some(a) = args.next_arg() {
        match a {
            Arg::Flag(f, v) => match f.as_str() {
                "model" => o.model = Some(args.value(&f, v)?),
                "agent" => o.agent = Some(args.value(&f, v)?),
                "chat" => o.chat = Some(args.value(&f, v)?),
                "mcp" => o.mcp.push(args.value(&f, v)?),
                "tool" => o.tools.push(args.value(&f, v)?),
                "no-mcp" => {
                    args::switch(&f, v)?;
                    o.no_mcp = true;
                }
                "json" => {
                    args::switch(&f, v)?;
                    o.json = true;
                }
                _ => return Err(args::unknown(&f)),
            },
            Arg::Positional(p) => words.push(p),
        }
    }
    o.prompt = words.join(" ");
    if o.prompt.trim().is_empty() {
        if std::io::stdin().is_terminal() {
            return Err(CliError::Usage("give a prompt to run".into()));
        }
        // Bounded: a prompt of the allowed length is at most four bytes per character.
        let limit = MAX_PROMPT_CHARS * 4;
        let mut buf = Vec::new();
        std::io::stdin()
            .take(limit as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(|e| CliError::Failed(format!("couldn't read the prompt from stdin: {e}")))?;
        if buf.len() > limit {
            return Err(CliError::Usage(format!(
                "the prompt on stdin is too long (the limit is {MAX_PROMPT_CHARS} characters)"
            )));
        }
        o.prompt = String::from_utf8(buf)
            .map_err(|_| CliError::Usage("the prompt on stdin isn't valid UTF-8".into()))?;
        if o.prompt.trim().is_empty() {
            return Err(CliError::Usage("the prompt on stdin is empty".into()));
        }
    }
    if o.no_mcp && (!o.mcp.is_empty() || !o.tools.is_empty()) {
        return Err(CliError::Usage(
            "--no-mcp can't be combined with --mcp or --tool".into(),
        ));
    }
    Ok(o)
}

pub fn run(opts: RunOptions) -> Result<()> {
    let interactive = !opts.json && term::interactive();
    let root = resolve_data_root(None)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| CliError::Failed(e.to_string()))?;
    let handle = rt.handle().clone();
    let result = rt.block_on(execute(opts, root, interactive, handle));
    // MCP sessions were cancelled; give their teardown (child process kill) a moment.
    rt.shutdown_timeout(Duration::from_secs(1));
    result
}

/// The process's services, assembled like the app's `AppState` but with read-only stores.
struct Session {
    services: RunServices,
    _lock: ProcessLock,
}

fn open(root: &DataRoot, handle: tokio::runtime::Handle) -> Result<Session> {
    let secrets = secret_store();
    let providers = ProviderStore::load_read_only(root, secrets.clone(), http_client()?)?;
    let settings = SettingsStore::load_read_only(root)?;
    let (db, lock) = Db::open_root(root)?;
    let mcp = McpManager::new(
        McpStore::load_read_only(root, secrets)?,
        http_client()?,
        handle.clone(),
        || {},
    );
    Ok(Session {
        services: RunServices {
            db,
            providers: Arc::new(providers),
            attachments: Arc::new(AttachmentStore::new(root.attachments_dir())),
            mcp,
            coordinator: RunCoordinator::new(1, handle),
            settings: Arc::new(settings),
        },
        _lock: lock,
    })
}

async fn execute(
    opts: RunOptions,
    root: DataRoot,
    interactive: bool,
    handle: tokio::runtime::Handle,
) -> Result<()> {
    let session = open(&root, handle)?;
    let s = &session.services;
    let result = execute_in(&opts, s, interactive).await;
    s.mcp.shutdown();
    result
}

/// The signals that ask the run to stop: Ctrl-C, and what a supervisor or a closing
/// terminal sends (SIGTERM, SIGHUP; Ctrl-Break on Windows). Listening starts when this is
/// created, so a signal that arrives while the run isn't waiting for one is kept for the
/// next `recv` — without it the process would die with a reply still marked as streaming
/// and its MCP server processes left behind.
struct Interrupt {
    #[cfg(unix)]
    signals: [tokio::signal::unix::Signal; 3],
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
}

impl Interrupt {
    #[cfg(unix)]
    fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        Ok(Self {
            signals: [
                signal(SignalKind::interrupt())?,
                signal(SignalKind::terminate())?,
                signal(SignalKind::hangup())?,
            ],
        })
    }

    #[cfg(windows)]
    fn new() -> std::io::Result<Self> {
        Ok(Self {
            ctrl_c: tokio::signal::windows::ctrl_c()?,
            ctrl_break: tokio::signal::windows::ctrl_break()?,
        })
    }

    #[cfg(unix)]
    async fn recv(&mut self) {
        let [a, b, c] = &mut self.signals;
        tokio::select! {
            _ = a.recv() => {}
            _ = b.recv() => {}
            _ = c.recv() => {}
        }
    }

    #[cfg(windows)]
    async fn recv(&mut self) {
        tokio::select! {
            _ = self.ctrl_c.recv() => {}
            _ = self.ctrl_break.recv() => {}
        }
    }
}

async fn execute_in(opts: &RunOptions, s: &RunServices, interactive: bool) -> Result<()> {
    let mut interrupt = Interrupt::new()
        .map_err(|e| CliError::Failed(format!("couldn't listen for Ctrl-C: {e}")))?;
    let chat = match &opts.chat {
        Some(raw) => {
            let id: ChatId = raw
                .trim()
                .parse()
                .map_err(|_| CliError::Usage(format!("`{raw}` is not a chat id")))?;
            Some(s.db.call(move |c| chats_repo::get_summary(c, id)).await?)
        }
        None => None,
    };
    let agents = s.db.call(|c| agents_repo::list_agents(c)).await?;
    let agent = resolve_agent(&agents, opts.agent.as_deref(), chat.as_ref())?;
    if agent.is_some() && opts.selecting() {
        return Err(CliError::Usage(
            "agent runs use the agent's own servers and tools; --mcp, --tool and --no-mcp \
             don't apply"
                .into(),
        ));
    }
    if agent.is_some() && opts.model.is_some() {
        return Err(CliError::Usage(
            "agent runs use the agent's own models; drop --model".into(),
        ));
    }

    let model = match &agent {
        Some(a) => a.default_model.clone(),
        None => resolve_model(&s.providers, opts.model.as_deref(), chat.as_ref())?,
    };
    let caps = s
        .providers
        .capabilities_for(&model.provider_id, &model.model)
        .map_err(|e| match (&agent, e) {
            (Some(a), agent_core::Error::NotFound(_)) => CliError::Usage(format!(
                "the model provider for agent \"{}\" was removed — edit the agent in the app",
                a.name
            )),
            (_, e) => e.into(),
        })?;
    if !caps.tools && (!opts.mcp.is_empty() || !opts.tools.is_empty()) {
        return Err(CliError::Usage(format!(
            "`{}` can't call tools, so --mcp and --tool don't apply",
            model.model
        )));
    }

    // Which servers the run's scope uses; only those are connected.
    let configs = s.mcp.store().list();
    let enabled = |id: &str| configs.iter().any(|c| c.id == id && c.enabled);
    let base = match &chat {
        Some(c) if agent.is_none() => {
            let id = c.id;
            s.db.call(move |c| chats_repo::get_tool_overrides(c, id))
                .await?
        }
        _ => ChatToolOverrides::default(),
    };
    let selection = if agent.is_none() && opts.selecting() {
        Some(Selection::resolve(&configs, opts)?)
    } else {
        None
    };
    let connect: Vec<String> = match (&agent, &selection) {
        _ if !caps.tools => Vec::new(),
        (Some(a), _) => a
            .servers
            .iter()
            .map(|a| a.server_id.clone())
            .filter(|id| enabled(id))
            .collect(),
        (None, Some(sel)) => sel.servers.keys().cloned().collect(),
        (None, None) => {
            let scope = ScopePolicies::Chat(base.clone());
            configs
                .iter()
                .filter(|c| c.enabled && scope.allows_server(&c.id))
                .map(|c| c.id.clone())
                .collect()
        }
    };
    // Nothing is stored yet, so stopping here only has to end the sessions (`execute`).
    tokio::select! {
        () = connect_servers(&s.mcp, &connect) => {}
        () = interrupt.recv() => {
            note("cancelled");
            return Err(CliError::Exit(EXIT_CANCELLED));
        }
    }
    let overrides = selection
        .map(|sel| sel.apply(base, &configs, &s.mcp))
        .transpose()?;

    // Everything is validated; now the chat can be created.
    let (chat_id, created) = match &chat {
        Some(c) => (c.id, false),
        None => {
            let agent_id = agent.as_ref().map(|a| a.id);
            let c =
                s.db.call(move |c| chats_repo::create_chat_with(c, "New chat", agent_id, None))
                    .await?;
            note(&format!(
                "chat {id} — continue it with `{bin} run --chat {id} …`",
                id = c.id,
                bin = product().slug
            ));
            (c.id, true)
        }
    };
    let discard = |db: &Db| {
        if created {
            db.spawn_job(move |c| {
                if chats_repo::get_summary(c, chat_id)?.message_count == 0 {
                    chats_repo::delete_chat(c, chat_id)?;
                }
                Ok(())
            });
        }
    };
    if let Some(o) = overrides {
        // Same validation as the app's Tools menu commands; a database row, so the app
        // shows what this run used and a later `--chat` run keeps it.
        let known: Vec<String> = configs.iter().map(|c| c.id.clone()).collect();
        let stored =
            s.db.call(move |c| {
                let o = o.validated(|id| known.iter().any(|k| k == id))?;
                chats_repo::set_tool_overrides(c, chat_id, &o)
            })
            .await;
        if let Err(e) = stored {
            discard(&s.db);
            return Err(e.into());
        }
    }

    let scope = match &agent {
        Some(a) => PermissionScope::Agent(a.id),
        None => PermissionScope::Chat(chat_id),
    };
    let (sink, terminal, writer) = Output::start(Writer::new(
        opts.json,
        interactive,
        s.coordinator.clone(),
        s.mcp.clone(),
        scope,
    ));
    let launched = launcher::launch(
        s,
        RunParams {
            chat_id,
            model: agent.is_none().then_some(model),
            prompt: opts.prompt.clone(),
            attachment_ids: Vec::new(),
            replace_from: None,
            // Nobody can answer the agent: it must finish on its own.
            unattended: agent.is_some() && !interactive,
        },
        sink,
        |_| {},
    )
    .await;
    let started = match launched {
        Ok(started) => started,
        Err(e) => {
            discard(&s.db);
            return Err(e.into());
        }
    };

    let mut terminal = terminal;
    let interrupted = tokio::select! {
        _ = &mut terminal => false,
        () = interrupt.recv() => true,
    };
    if interrupted {
        s.coordinator.cancel(started.run_id);
        note("cancelling… press Ctrl-C again to quit now");
        tokio::select! {
            _ = &mut terminal => {}
            _ = tokio::time::sleep(CANCEL_WAIT) => note("the run didn't stop in time"),
            () = interrupt.recv() => std::process::exit(EXIT_INTERRUPTED.into()),
        }
    }
    // The terminal event is sent after the final write was queued on the database worker,
    // so this read (queued behind it) sees the stored outcome — fire-and-forget writes only
    // log failures, so the exit code comes from what was actually stored.
    let id = started.assistant_message.id.clone();
    let status =
        s.db.call(move |c| chats_repo::message_status(c, &id))
            .await?;
    writer.finish(Duration::from_secs(2));
    match status {
        Some(MessageStatus::Complete) => Ok(()),
        Some(MessageStatus::Cancelled) => Err(CliError::Exit(EXIT_CANCELLED)),
        _ => Err(CliError::Exit(EXIT_FAILED)),
    }
}

fn resolve_agent(
    agents: &[Agent],
    name: Option<&str>,
    chat: Option<&ChatSummary>,
) -> Result<Option<Agent>> {
    let named = name.map(|n| find_agent(agents, n)).transpose()?;
    let of_chat = chat
        .and_then(|c| c.agent_id)
        .map(|id| {
            agents
                .iter()
                .find(|a| a.id == id)
                .cloned()
                .ok_or_else(|| CliError::Failed("the chat's agent no longer exists".into()))
        })
        .transpose()?;
    match (named, of_chat, chat) {
        (Some(a), Some(b), _) if a.id != b.id => Err(CliError::Usage(format!(
            "that chat belongs to agent \"{}\"",
            b.name
        ))),
        (Some(_), None, Some(_)) => Err(CliError::Usage(
            "that chat has no agent; drop --agent to continue it".into(),
        )),
        (a, b, _) => Ok(a.or(b)),
    }
}

/// The longest key (name, case-insensitive, or id) that `spec` equals or that is followed by
/// `/` in it, with the rest after the `/`. Several items sharing that key is an error.
fn longest_prefix<'a, T>(
    items: &'a [T],
    keys: impl Fn(&T) -> [&str; 2],
    spec: &str,
    what: &str,
) -> Result<Option<(&'a T, String)>> {
    let mut best: Vec<(&T, usize)> = Vec::new();
    for item in items {
        for key in keys(item) {
            let len = key.len();
            let hit = !key.is_empty()
                && spec.len() >= len
                && spec.is_char_boundary(len)
                && spec[..len].eq_ignore_ascii_case(key)
                && (spec.len() == len || spec[len..].starts_with('/'));
            if !hit {
                continue;
            }
            match best.first().map(|(_, l)| *l) {
                Some(l) if l > len => {}
                Some(l) if l == len => {
                    if !best.iter().any(|(b, _)| std::ptr::eq(*b, item)) {
                        best.push((item, len));
                    }
                }
                _ => best = vec![(item, len)],
            }
        }
    }
    match best.as_slice() {
        [] => Ok(None),
        [(item, len)] => Ok(Some((
            *item,
            spec[*len..].trim_start_matches('/').to_owned(),
        ))),
        _ => Err(CliError::Usage(format!(
            "several {what}s match `{spec}`; use the id"
        ))),
    }
}

fn resolve_model(
    providers: &ProviderStore,
    spec: Option<&str>,
    chat: Option<&ChatSummary>,
) -> Result<ModelRef> {
    let bin = product().slug;
    if let Some(spec) = spec {
        let list = providers.list();
        let (p, model) = longest_prefix(
            &list,
            |p: &ProviderConfig| [p.id.as_str(), p.name.as_str()],
            spec.trim(),
            "provider",
        )?
        .ok_or_else(|| {
            CliError::Usage(format!("no provider matches `{spec}` — see `{bin} models`"))
        })?;
        let model = Some(model.trim().to_owned())
            .filter(|m| !m.is_empty())
            .or_else(|| p.default_model.clone())
            .ok_or_else(|| {
                CliError::Usage(format!(
                    "\"{}\" has no default model; use `--model \"{}/<model>\"`",
                    p.name, p.name
                ))
            })?;
        return Ok(ModelRef {
            provider_id: p.id.clone(),
            model,
        });
    }
    if let Some((provider_id, model)) =
        chat.and_then(|c| Some((c.provider_id.clone()?, c.model.clone()?)))
    {
        return Ok(ModelRef { provider_id, model });
    }
    if let Some(a) = providers.active() {
        return Ok(ModelRef {
            provider_id: a.provider_id,
            model: a.model,
        });
    }
    Err(CliError::Usage(format!(
        "choose a model with --model \"<provider>/<model>\" (see `{bin} models`)"
    )))
}

/// `--mcp` / `--tool` / `--no-mcp`: the command-line counterpart of a chat's Tools menu.
/// It only selects — servers on or off, tools disabled — and never sets Allow or Ask.
struct Selection {
    /// Server id → `None` for the whole server, or the only tools it may use.
    servers: BTreeMap<String, Option<BTreeSet<String>>>,
}

fn find_server<'a>(configs: &'a [McpServerConfig], spec: &str) -> Result<&'a McpServerConfig> {
    let spec = spec.trim();
    let mut hits = configs
        .iter()
        .filter(|c| c.id == spec || c.name.eq_ignore_ascii_case(spec));
    match (hits.next(), hits.next()) {
        (Some(c), None) => Ok(c),
        (None, _) => Err(CliError::Usage(format!(
            "no MCP connection named `{spec}` — see `{} mcp list`",
            product().slug
        ))),
        _ => Err(CliError::Usage(format!(
            "several MCP connections match `{spec}`; use the id"
        ))),
    }
}

fn require_enabled(c: &McpServerConfig) -> Result<()> {
    if c.enabled {
        return Ok(());
    }
    Err(CliError::Usage(format!(
        "`{}` is turned off in Connections — turn it on in the app first",
        c.name
    )))
}

impl Selection {
    fn resolve(configs: &[McpServerConfig], opts: &RunOptions) -> Result<Self> {
        let mut servers: BTreeMap<String, Option<BTreeSet<String>>> = BTreeMap::new();
        for spec in &opts.mcp {
            let c = find_server(configs, spec)?;
            require_enabled(c)?;
            servers.entry(c.id.clone()).or_insert(None);
        }
        for spec in &opts.tools {
            let (c, tool) = longest_prefix(
                configs,
                |c: &McpServerConfig| [c.id.as_str(), c.name.as_str()],
                spec.trim(),
                "MCP connection",
            )?
            .filter(|(_, t)| !t.trim().is_empty())
            .ok_or_else(|| {
                CliError::Usage(format!(
                    "`--tool {spec}` needs <server>/<tool> with a configured server"
                ))
            })?;
            require_enabled(c)?;
            let tools = servers.entry(c.id.clone()).or_insert(None);
            tools
                .get_or_insert_with(BTreeSet::new)
                .insert(tool.trim().to_owned());
        }
        Ok(Self { servers })
    }

    /// The chat's overrides with this on/off selection: unselected servers off; for a
    /// server limited with `--tool`, its other tools disabled and a selected tool the chat
    /// had disabled back on its connection permission. Other Allow/Ask choices are kept.
    fn apply(
        &self,
        mut o: ChatToolOverrides,
        configs: &[McpServerConfig],
        mcp: &McpManager,
    ) -> Result<ChatToolOverrides> {
        let views = mcp.list_views();
        for c in configs {
            let Some(selected) = self.servers.get(&c.id) else {
                o.set_server_off(&c.id, true);
                continue;
            };
            o.set_server_off(&c.id, false);
            let Some(wanted) = selected else { continue };
            let view = views
                .iter()
                .find(|v| v.config.id == c.id)
                .filter(|v| !v.tools_cached)
                .ok_or_else(|| {
                    CliError::Failed(format!(
                        "couldn't connect to `{}`, so its tools can't be checked",
                        c.name
                    ))
                })?;
            for w in wanted {
                if !view.tools.iter().any(|t| t.name == *w) {
                    return Err(CliError::Usage(format!("`{}` has no tool `{w}`", c.name)));
                }
                if mcp.store().policy_for(&c.id, w) == ToolPolicy::Block {
                    return Err(CliError::Usage(format!(
                        "`{}/{w}` is disabled in Connections — enable it in the app first",
                        c.name
                    )));
                }
            }
            for t in &view.tools {
                let chat_policy = o
                    .servers
                    .get(&c.id)
                    .and_then(|s| s.policies.get(&t.name))
                    .copied();
                if wanted.contains(&t.name) {
                    if chat_policy == Some(ToolPolicy::Block) {
                        o.set_tool(&c.id, &t.name, None);
                    }
                } else {
                    o.set_tool(&c.id, &t.name, Some(ToolPolicy::Block));
                }
            }
        }
        Ok(o)
    }
}

/// Connects the run's servers concurrently, waiting at most `CONNECT_WAIT`; a server that
/// isn't ready by then is left out of this run.
async fn connect_servers(mcp: &Arc<McpManager>, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    let mut set = tokio::task::JoinSet::new();
    for id in ids {
        let (m, id) = (Arc::clone(mcp), id.clone());
        set.spawn(async move { m.connect(&id).await });
    }
    let _ = tokio::time::timeout(CONNECT_WAIT, async {
        while set.join_next().await.is_some() {}
    })
    .await;
    for v in mcp.list_views() {
        if !ids.contains(&v.config.id) {
            continue;
        }
        let why = match &v.status {
            McpServerStatus::Connected { .. } => continue,
            McpServerStatus::Failed { error } => error.clone(),
            McpServerStatus::NeedsAuth { .. } => "sign in to it in the app".into(),
            McpServerStatus::Connecting => format!("not ready after {}s", CONNECT_WAIT.as_secs()),
            s => format!("{s:?}"),
        };
        note(&format!(
            "warning: `{}` isn't connected ({why}); its tools are unavailable",
            v.config.name
        ));
    }
}

/// What the run task and the writer thread share.
#[derive(Default)]
struct Shared {
    /// stdout was closed (e.g. `| head`): the sink reports a disconnect, cancelling the run.
    broken: AtomicBool,
    terminal: Mutex<Option<oneshot::Sender<()>>>,
}

/// `RunSink::send` runs on the run task, so it only queues the event: a bounded channel to
/// the writer thread. When the queue is full the run waits (backpressure) without stalling
/// the runtime's other tasks.
struct CliSink {
    tx: SyncSender<RunEvent>,
    shared: Arc<Shared>,
}

impl RunSink for CliSink {
    fn send(&self, event: RunEvent) -> bool {
        let terminal = event.payload.is_terminal();
        let queued = match self.tx.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(event)) => {
                tokio::task::block_in_place(|| self.tx.send(event).is_ok())
            }
            Err(TrySendError::Disconnected(_)) => false,
        };
        if terminal {
            if let Some(done) = self.shared.terminal.lock().unwrap().take() {
                let _ = done.send(());
            }
        }
        queued && !self.shared.broken.load(Ordering::Relaxed)
    }
}

struct Output {
    finished: Receiver<()>,
}

impl Output {
    fn start(writer: Writer) -> (CliSink, oneshot::Receiver<()>, Output) {
        let (tx, rx) = mpsc::sync_channel(OUTPUT_QUEUE);
        let (done_tx, done_rx) = oneshot::channel();
        let shared = Arc::new(Shared {
            broken: AtomicBool::new(false),
            terminal: Mutex::new(Some(done_tx)),
        });
        let (fin_tx, fin_rx) = mpsc::sync_channel(1);
        let thread_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("output".into())
            .spawn(move || {
                writer.drain(rx, &thread_shared);
                let _ = fin_tx.send(());
            })
            .expect("failed to spawn the output thread");
        (CliSink { tx, shared }, done_rx, Output { finished: fin_rx })
    }

    /// Waits (bounded) for queued output to be written. The writer may be blocked on an
    /// approval prompt of a run that was cancelled meanwhile; the process exits anyway.
    fn finish(self, wait: Duration) {
        let _ = self.finished.recv_timeout(wait);
    }
}

struct Writer {
    json: bool,
    interactive: bool,
    stdout_tty: bool,
    coordinator: Arc<RunCoordinator>,
    mcp: Arc<McpManager>,
    scope: PermissionScope,
    calls: HashMap<String, ToolCallInfo>,
    /// `(server name, tool)` the scope stores as Allow, computed at the first prompt: those
    /// ask only because their definition changed or was never pinned in the app.
    stored_allow: Option<HashSet<(String, String)>>,
    wrote_text: bool,
    ends_with_newline: bool,
}

impl Writer {
    fn new(
        json: bool,
        interactive: bool,
        coordinator: Arc<RunCoordinator>,
        mcp: Arc<McpManager>,
        scope: PermissionScope,
    ) -> Self {
        Self {
            json,
            interactive,
            stdout_tty: std::io::stdout().is_terminal(),
            coordinator,
            mcp,
            scope,
            calls: HashMap::new(),
            stored_allow: None,
            wrote_text: false,
            ends_with_newline: true,
        }
    }

    fn drain(mut self, rx: Receiver<RunEvent>, shared: &Shared) {
        for event in rx {
            if self.json {
                let line = serde_json::to_string(&event).unwrap_or_default() + "\n";
                self.out(&line, shared);
            }
            self.handle(event, shared);
        }
    }

    fn out(&mut self, s: &str, shared: &Shared) {
        if shared.broken.load(Ordering::Relaxed) {
            return;
        }
        let mut stdout = std::io::stdout().lock();
        if stdout
            .write_all(s.as_bytes())
            .and_then(|()| stdout.flush())
            .is_err()
        {
            shared.broken.store(true, Ordering::Relaxed);
        }
    }

    fn handle(&mut self, event: RunEvent, shared: &Shared) {
        let text_mode = !self.json;
        match event.payload {
            RunEventPayload::TextDelta { text } if text_mode => {
                let text = term::for_stream(&text, self.stdout_tty, true);
                if !text.is_empty() {
                    self.wrote_text = true;
                    self.ends_with_newline = text.ends_with('\n');
                    self.out(&text, shared);
                }
            }
            RunEventPayload::Notice { message } if text_mode => note(&format!("note: {message}")),
            RunEventPayload::ModelSwitched { model, reason, .. } if text_mode => {
                note(&format!("note: switched to {model}: {reason}"))
            }
            RunEventPayload::ToolCall { call } => {
                if text_mode {
                    note(&format!(
                        "[tool] {}",
                        term::tool_label(call.server.as_deref(), &call.tool)
                    ));
                }
                self.calls.insert(call.id.clone(), call);
            }
            RunEventPayload::ToolStatus {
                call_id,
                status: ToolCallStatus::PendingApproval,
            } => self.approve(event.run_id, &call_id),
            RunEventPayload::ToolResult {
                call_id, status, ..
            } if text_mode && status != ToolCallStatus::Completed => {
                let label = self
                    .calls
                    .get(&call_id)
                    .map(|c| term::tool_label(c.server.as_deref(), &c.tool))
                    .unwrap_or_default();
                let status = serde_json::to_value(status).unwrap_or_default();
                note(&format!(
                    "[tool] {label}: {}",
                    status.as_str().unwrap_or_default().replace('_', " ")
                ));
            }
            RunEventPayload::Completed { .. } | RunEventPayload::Cancelled
                if text_mode && self.wrote_text && !self.ends_with_newline && self.stdout_tty =>
            {
                self.out("\n", shared);
                if matches!(event.payload, RunEventPayload::Cancelled) {
                    note("cancelled");
                }
            }
            RunEventPayload::Cancelled if text_mode => note("cancelled"),
            RunEventPayload::Failed { message } if text_mode => {
                if self.wrote_text && !self.ends_with_newline && self.stdout_tty {
                    self.out("\n", shared);
                }
                note(&format!("error: {message}"));
            }
            _ => {}
        }
    }

    fn stored_allow(&mut self, server: &str, tool: &str) -> bool {
        let (mcp, scope) = (&self.mcp, &self.scope);
        self.stored_allow
            .get_or_insert_with(|| {
                mcp.list_scoped(scope)
                    .into_iter()
                    .flat_map(|s| {
                        let name = mcp
                            .store()
                            .get(&s.server_id)
                            .map(|c| c.name)
                            .unwrap_or_default();
                        s.tools
                            .into_iter()
                            .filter(|t| t.in_scope && t.policy == ToolPolicy::AlwaysAllow)
                            .map(move |t| (name.clone(), t.name))
                    })
                    .collect()
            })
            .contains(&(server.to_owned(), tool.to_owned()))
    }

    fn approve(&mut self, run_id: agent_core::domain::RunId, call_id: &str) {
        // A call this process never saw announced can't be shown, so it can't be approved.
        let Some(call) = self.calls.get(call_id).cloned() else {
            note("denied a tool call that was never announced");
            self.coordinator
                .resolve_approval(run_id, call_id, ApprovalDecision::Deny);
            return;
        };
        let (server, tool, arguments) = (call.server, call.tool, call.arguments);
        let label = term::tool_label(server.as_deref(), &tool);
        if server
            .as_deref()
            .is_some_and(|s| self.stored_allow(s, &tool))
        {
            note(&format!(
                "note: {label} is set to Allow, but its definition changed or was never \
                 reviewed in the app — review it under Connections to let it run without a prompt"
            ));
        }
        let decision = if self.interactive {
            let mut err = std::io::stderr().lock();
            let _ = write!(
                err,
                "{}",
                term::approval_prompt(server.as_deref(), &tool, &arguments)
            );
            let _ = err.flush();
            drop(err);
            let mut answer = String::new();
            match std::io::stdin().read_line(&mut answer) {
                Ok(n) if n > 0 && matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") => {
                    ApprovalDecision::AllowOnce
                }
                _ => ApprovalDecision::Deny,
            }
        } else {
            note(&format!(
                "denied {label}: it needs approval and nobody can answer here (run it in a \
                 terminal without --json to be asked, or set it to Allow in the app)"
            ));
            ApprovalDecision::Deny
        };
        if !self.coordinator.resolve_approval(run_id, call_id, decision) && self.interactive {
            note("the approval was no longer pending");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Item(&'static str, &'static str);

    #[test]
    fn longest_prefix_prefers_the_longest_key_and_keeps_slashes_in_the_rest() {
        fn keys(i: &Item) -> [&str; 2] {
            [i.0, i.1]
        }
        let items = [
            Item("1", "Local"),
            Item("2", "Local/LAN"),
            Item("3", "OpenAI"),
        ];
        let hit = |spec| {
            longest_prefix(&items, keys, spec, "provider")
                .unwrap()
                .map(|(i, rest)| (i.0, rest))
        };
        assert_eq!(hit("Local/LAN/llama3/8b"), Some(("2", "llama3/8b".into())));
        assert_eq!(hit("local/qwen"), Some(("1", "qwen".into())));
        assert_eq!(hit("openai"), Some(("3", String::new())));
        assert_eq!(hit("3/gpt-5"), Some(("3", "gpt-5".into())));
        assert_eq!(hit("OpenAIx/gpt"), None);
        let dupes = [Item("1", "Same"), Item("2", "same")];
        assert!(longest_prefix(&dupes, keys, "same/m", "provider").is_err());
    }
}
