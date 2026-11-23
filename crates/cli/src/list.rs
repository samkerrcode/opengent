//! `models` and `mcp list`. Neither opens the database, connects to a server or reads the
//! keychain: both only read `providers.json` / `mcp.json`, so they work while the database
//! is locked or before it exists.

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::sync::Arc;

use agent_core::config::resolve_data_root;
use agent_core::domain::ToolPolicy;
use agent_core::mcp::{McpStore, McpTransport};
use agent_core::providers::registry::ProviderStore;
use agent_core::secrets::MemorySecretStore;
use serde::Serialize;

use crate::args::{self, Arg, Args};
use crate::{http_client, term, CliError, Result};

fn json_only(args: &mut Args, extra: &mut dyn FnMut(&str) -> bool) -> Result<bool> {
    let mut json = false;
    while let Some(a) = args.next_arg() {
        match a {
            Arg::Flag(f, v) if f == "json" => {
                args::switch(&f, v)?;
                json = true;
            }
            Arg::Flag(f, v) if extra(&f) => args::switch(&f, v)?,
            Arg::Flag(f, _) => return Err(args::unknown(&f)),
            Arg::Positional(p) => {
                return Err(CliError::Usage(format!("unexpected argument `{p}`")))
            }
        }
    }
    Ok(json)
}

pub fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(|e| CliError::Failed(e.to_string()))?;
    term::out(&(text + "\n"))
}

#[derive(Serialize)]
struct ModelsEntry {
    id: String,
    name: String,
    kind: serde_json::Value,
    #[serde(skip)]
    label: &'static str,
    default_model: Option<String>,
    models: Vec<String>,
    /// The model last selected in the app, if it is on this provider; `run` uses it when
    /// no `--model` is given.
    active_model: Option<String>,
}

pub fn models(args: &mut Args) -> Result<()> {
    let json = json_only(args, &mut |_| false)?;
    let root = resolve_data_root(None)?;
    // An empty in-memory secret store: listing never needs a key.
    let store = ProviderStore::load_read_only(
        &root,
        Arc::new(MemorySecretStore::default()),
        http_client()?,
    )?;
    let active = store.active();
    let entries: Vec<ModelsEntry> = store
        .list()
        .into_iter()
        .map(|p| {
            let mut models: Vec<String> = p
                .models
                .iter()
                .filter(|m| !p.hidden_models.contains(*m))
                .cloned()
                .collect();
            if let Some(d) = p.default_model.as_ref().filter(|d| !models.contains(d)) {
                models.insert(0, d.clone());
            }
            ModelsEntry {
                kind: serde_json::to_value(p.kind).unwrap_or_default(),
                label: p.kind.label(),
                active_model: active
                    .as_ref()
                    .filter(|a| a.provider_id == p.id)
                    .map(|a| a.model.clone()),
                id: p.id,
                name: p.name,
                default_model: p.default_model,
                models,
            }
        })
        .collect();
    if json {
        return print_json(&entries);
    }
    let clean = |s: &str| term::sanitize(s, false);
    let mut text = String::new();
    for e in &entries {
        let _ = writeln!(
            text,
            "{}  ({}, id {})",
            clean(&e.name),
            e.label,
            clean(&e.id)
        );
        if e.models.is_empty() {
            let _ = writeln!(
                text,
                "  (no models listed — use `{}/<model>`)",
                clean(&e.name)
            );
        }
        for m in &e.models {
            let mut tags = Vec::new();
            if e.default_model.as_deref() == Some(m) {
                tags.push("default");
            }
            if e.active_model.as_deref() == Some(m) {
                tags.push("active");
            }
            let tags = if tags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", tags.join(", "))
            };
            let _ = writeln!(text, "  {}{tags}", clean(m));
        }
    }
    term::out(&text)?;
    // A hint for a person; kept out of stdout so piped output is only the list.
    if std::io::stdout().is_terminal() {
        term::note(&format!(
            "\nRun a prompt with: {} run --model \"<provider>/<model>\" \"…\"",
            agent_core::config::product().slug
        ));
    }
    Ok(())
}

pub fn policy_label(p: ToolPolicy) -> &'static str {
    match p {
        ToolPolicy::AlwaysAllow => "allow",
        ToolPolicy::Ask => "ask",
        ToolPolicy::Block => "disable",
    }
}

#[derive(Serialize)]
struct McpEntry {
    id: String,
    name: String,
    transport: &'static str,
    enabled: bool,
    default_permission: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<McpTool>>,
}

#[derive(Serialize)]
struct McpTool {
    name: String,
    /// Connection-level permission (per-tool setting, else the server default).
    permission: &'static str,
}

pub fn mcp(args: &mut Args) -> Result<()> {
    let mut tools = false;
    let json = json_only(args, &mut |f| {
        let hit = f == "tools";
        tools |= hit;
        hit
    })?;
    let root = resolve_data_root(None)?;
    let store = McpStore::load_read_only(&root, Arc::new(MemorySecretStore::default()))?;
    // Only non-secret fields are listed: secret env/header values are never in `mcp.json`,
    // and plain ones aren't printed either.
    let entries: Vec<McpEntry> = store
        .list()
        .into_iter()
        .map(|s| McpEntry {
            tools: tools.then(|| {
                store
                    .catalog(&s.id)
                    .into_iter()
                    .map(|t| McpTool {
                        permission: policy_label(store.policy_for(&s.id, &t.name)),
                        name: t.name,
                    })
                    .collect()
            }),
            transport: match s.transport {
                McpTransport::Stdio => "stdio",
                McpTransport::StreamableHttp => "http",
            },
            enabled: s.enabled,
            default_permission: policy_label(s.default_policy),
            id: s.id,
            name: s.name,
        })
        .collect();
    if json {
        return print_json(&entries);
    }
    if entries.is_empty() {
        return term::out("No MCP connections. Add them in the app under Connections.\n");
    }
    let clean = |s: &str| term::sanitize(s, false);
    let mut text = String::new();
    for e in &entries {
        let _ = writeln!(
            text,
            "{}  ({}, {}, default {}, id {})",
            clean(&e.name),
            e.transport,
            if e.enabled { "on" } else { "off" },
            e.default_permission,
            clean(&e.id)
        );
        match &e.tools {
            Some(t) if t.is_empty() => {
                text.push_str("  (no tools seen yet — connect it in the app)\n")
            }
            Some(t) => {
                for tool in t {
                    let _ = writeln!(text, "  {}  {}", clean(&tool.name), tool.permission);
                }
            }
            None => {}
        }
    }
    term::out(&text)
}
