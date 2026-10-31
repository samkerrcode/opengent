//! MCP server configuration. Non-secret fields persist as JSON under the data root
//! (`mcp.json`, same pattern as `providers.json`). Values marked `secret` (env vars,
//! HTTP headers) live only in the `SecretStore` under `mcp/<id>/…`; the config records
//! a `has_value` flag, never the value.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agent::tools::ToolSettings;
use crate::config::DataRoot;
use crate::domain::ToolPolicy;
use crate::providers::openai_compat::normalize_base_url;
use crate::secrets::{SecretRef, SecretStore};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum McpTransport {
    /// Spawn a local process, talk JSON-RPC over stdin/stdout.
    Stdio,
    /// Remote MCP endpoint speaking Streamable HTTP.
    StreamableHttp,
}

/// One env var or HTTP header. `secret` values are stored in the keychain; `has_value`
/// records whether one exists so the UI can show "(saved)" without ever seeing it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct KeyValueEntry {
    pub name: String,
    /// Plaintext value for non-secret entries; always "" for secret ones on disk.
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub has_value: bool,
}

/// Input for an env/header row from the UI. `secret` rows with an empty `value` keep the
/// stored secret; remove the row to delete it.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct KeyValueInput {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub transport: McpTransport,
    // stdio
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<KeyValueEntry>,
    #[serde(default)]
    pub cwd: Option<String>,
    // streamable http
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: Vec<KeyValueEntry>,
    /// Policy applied to tools without an explicit entry in `tool_policies`.
    #[serde(default)]
    pub default_policy: ToolPolicy,
    /// Per-tool overrides keyed by the server's own tool name.
    #[serde(default)]
    pub tool_policies: HashMap<String, ToolPolicy>,
    /// Per-call timeout for tool invocations on this server.
    #[serde(default = "default_call_timeout")]
    #[ts(type = "number")]
    pub call_timeout_secs: u64,
}

fn default_true() -> bool {
    true
}
fn default_call_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct McpServerInput {
    pub id: Option<String>,
    pub name: String,
    pub transport: McpTransport,
    /// Full command line (split shell-style into command + args on the Rust side).
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub env: Vec<KeyValueInput>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: Vec<KeyValueInput>,
    #[serde(default)]
    pub default_policy: ToolPolicy,
    #[serde(default = "default_call_timeout")]
    #[ts(type = "number")]
    pub call_timeout_secs: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    version: u16,
    #[serde(default)]
    servers: Vec<McpServerConfig>,
    #[serde(default)]
    settings: ToolSettings,
}

pub struct McpStore {
    path: PathBuf,
    state: Mutex<StoreFile>,
    secrets: Arc<dyn SecretStore>,
}

fn secret_ref(server_id: &str, scope: &str, name: &str) -> Result<SecretRef> {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    SecretRef::new(format!("mcp/{server_id}/{scope}/{clean}"))
}

/// Splits a command line into program + args honoring single/double quotes and backslash
/// escapes — enough for pasted `npx -y server` lines without a real shell.
pub fn split_command(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut escape = false;
    for c in line.chars() {
        match (escape, quote, c) {
            (true, _, c) => {
                cur.push(c);
                escape = false;
            }
            (_, Some('"'), '"') | (_, Some('\''), '\'') => quote = None,
            (_, None, '"') | (_, None, '\'') => quote = Some(c),
            (_, Some(_), c) => cur.push(c),
            (_, None, '\\') => escape = true,
            (_, None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (_, None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl McpStore {
    pub fn load(data_root: &DataRoot, secrets: Arc<dyn SecretStore>) -> Result<Self> {
        let path = data_root.path().join("mcp.json");
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<StoreFile>(&bytes)
                .map_err(|e| Error::Io(format!("mcp.json is corrupt: {e}")))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreFile {
                version: 1,
                ..Default::default()
            },
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
            secrets,
        })
    }

    fn persist(&self, state: &StoreFile) -> Result<()> {
        let persisted = StoreFile {
            version: 1,
            servers: state
                .servers
                .iter()
                .cloned()
                .map(|mut s| {
                    // Never write secret values to disk.
                    for e in s.env.iter_mut().chain(s.headers.iter_mut()) {
                        if e.secret {
                            e.value.clear();
                        }
                    }
                    s
                })
                .collect(),
            settings: state.settings.clone(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(&persisted).map_err(|e| Error::Internal(e.to_string()))?,
        )?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<McpServerConfig> {
        self.state.lock().unwrap().servers.clone()
    }

    pub fn get(&self, id: &str) -> Result<McpServerConfig> {
        self.state
            .lock()
            .unwrap()
            .servers
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("MCP server `{id}`")))
    }

    pub fn settings(&self) -> ToolSettings {
        self.state.lock().unwrap().settings.clone()
    }

    pub fn set_settings(&self, settings: ToolSettings) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.settings = settings;
        self.persist(&state)
    }

    /// Effective policy for a tool: explicit override → server default.
    pub fn policy_for(&self, server_id: &str, tool: &str) -> ToolPolicy {
        self.get(server_id)
            .map(|s| {
                s.tool_policies
                    .get(tool)
                    .copied()
                    .unwrap_or(s.default_policy)
            })
            .unwrap_or(ToolPolicy::Ask)
    }

    pub fn set_tool_policy(&self, server_id: &str, tool: &str, policy: ToolPolicy) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let s = state
            .servers
            .iter_mut()
            .find(|s| s.id == server_id)
            .ok_or_else(|| Error::NotFound(format!("MCP server `{server_id}`")))?;
        // Storing an explicit entry equal to the default is harmless and survives later
        // default changes; keep it simple and always record it.
        s.tool_policies.insert(tool.to_owned(), policy);
        self.persist(&state)
    }

    pub fn set_default_policy(&self, server_id: &str, policy: ToolPolicy) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let s = state
            .servers
            .iter_mut()
            .find(|s| s.id == server_id)
            .ok_or_else(|| Error::NotFound(format!("MCP server `{server_id}`")))?;
        s.default_policy = policy;
        self.persist(&state)
    }

    /// Validated upsert. `env`/`headers` inputs with `secret` + empty value keep the stored
    /// secret (mirrors provider API key semantics); a missing row deletes it.
    pub fn save(&self, input: McpServerInput) -> Result<McpServerConfig> {
        let name = input.name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(Error::InvalidInput("name must be 1–64 characters".into()));
        }
        if input.call_timeout_secs == 0 || input.call_timeout_secs > 3600 {
            return Err(Error::InvalidInput(
                "tool call timeout must be 1–3600 seconds".into(),
            ));
        }
        let (command, args) = match input.transport {
            McpTransport::Stdio => {
                let parts = split_command(&input.command);
                match parts.split_first() {
                    Some((cmd, args)) if !cmd.is_empty() => (cmd.clone(), args.to_vec()),
                    _ => {
                        return Err(Error::InvalidInput(
                            "command is required for stdio servers".into(),
                        ))
                    }
                }
            }
            McpTransport::StreamableHttp => {
                normalize_base_url(&input.url).map_err(|e| match e {
                    Error::InvalidInput(m) => Error::InvalidInput(format!("server URL: {m}")),
                    other => other,
                })?;
                (String::new(), Vec::new())
            }
        };
        for e in input.env.iter().chain(input.headers.iter()) {
            let n = e.name.trim();
            if n.is_empty() {
                return Err(Error::InvalidInput(
                    "environment/header names cannot be empty".into(),
                ));
            }
            if n.len() > 128 || e.value.chars().count() > 4096 {
                return Err(Error::InvalidInput(format!("invalid entry `{n}`")));
            }
        }

        let id = input.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut state = self.state.lock().unwrap();
        let existing = state.servers.iter().find(|s| s.id == id).cloned();

        let map_entries = |scope: &str,
                           inputs: Vec<KeyValueInput>,
                           old: &[KeyValueEntry]|
         -> Result<Vec<KeyValueEntry>> {
            let mut out = Vec::with_capacity(inputs.len());
            for e in inputs {
                let name = e.name.trim().to_owned();
                if e.secret {
                    let r = secret_ref(&id, scope, &name)?;
                    if !e.value.is_empty() {
                        self.secrets.set(&r, e.value.trim())?;
                    }
                    let kept =
                        e.value.is_empty() && old.iter().any(|o| o.name == name && o.has_value);
                    out.push(KeyValueEntry {
                        name,
                        value: String::new(),
                        secret: true,
                        has_value: !e.value.is_empty() || kept,
                    });
                } else {
                    out.push(KeyValueEntry {
                        name,
                        value: e.value,
                        secret: false,
                        has_value: false,
                    });
                }
            }
            Ok(out)
        };

        let env = match input.transport {
            McpTransport::Stdio => map_entries(
                "env",
                input.env,
                existing.as_ref().map(|s| s.env.as_slice()).unwrap_or(&[]),
            )?,
            McpTransport::StreamableHttp => Vec::new(),
        };
        let headers = match input.transport {
            McpTransport::StreamableHttp => map_entries(
                "header",
                input.headers,
                existing
                    .as_ref()
                    .map(|s| s.headers.as_slice())
                    .unwrap_or(&[]),
            )?,
            McpTransport::Stdio => Vec::new(),
        };

        let config = McpServerConfig {
            id: id.clone(),
            name: name.to_owned(),
            enabled: existing.as_ref().map(|s| s.enabled).unwrap_or(true),
            transport: input.transport,
            command,
            args,
            env,
            cwd: input
                .cwd
                .map(|c| c.trim().to_owned())
                .filter(|c| !c.is_empty()),
            url: match input.transport {
                McpTransport::StreamableHttp => normalize_base_url(&input.url)?,
                McpTransport::Stdio => String::new(),
            },
            headers,
            default_policy: input.default_policy,
            tool_policies: existing.map(|s| s.tool_policies).unwrap_or_default(),
            call_timeout_secs: input.call_timeout_secs,
        };
        match state.servers.iter_mut().find(|s| s.id == id) {
            Some(slot) => *slot = config.clone(),
            None => state.servers.push(config.clone()),
        }
        self.persist(&state)?;
        Ok(config)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let before = state.servers.len();
        let removed = state
            .servers
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("MCP server `{id}`")))?;
        state.servers.retain(|s| s.id != id);
        debug_assert!(state.servers.len() < before);
        for (scope, entries) in [("env", &removed.env), ("header", &removed.headers)] {
            for e in entries.iter().filter(|e| e.secret) {
                let _ = self.secrets.delete(&secret_ref(id, scope, &e.name)?);
            }
        }
        self.persist(&state)
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let s = state
            .servers
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| Error::NotFound(format!("MCP server `{id}`")))?;
        s.enabled = enabled;
        self.persist(&state)
    }

    /// Resolves the real env map and header map for a server, pulling secret values from the
    /// keychain. Only called at connect time inside Rust.
    pub fn resolved_entries(
        &self,
        config: &McpServerConfig,
    ) -> Result<(HashMap<String, String>, HashMap<String, String>)> {
        let mut env = HashMap::new();
        for e in &config.env {
            if e.secret {
                if e.has_value {
                    if let Some(v) = self.secrets.get(&secret_ref(&config.id, "env", &e.name)?)? {
                        env.insert(e.name.clone(), v);
                    }
                }
            } else {
                env.insert(e.name.clone(), e.value.clone());
            }
        }
        let mut headers = HashMap::new();
        for e in &config.headers {
            if e.secret {
                if e.has_value {
                    if let Some(v) = self
                        .secrets
                        .get(&secret_ref(&config.id, "header", &e.name)?)?
                    {
                        headers.insert(e.name.clone(), v);
                    }
                }
            } else {
                headers.insert(e.name.clone(), e.value.clone());
            }
        }
        Ok((env, headers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolve_data_root;
    use crate::secrets::MemorySecretStore;

    fn store() -> (McpStore, PathBuf) {
        let tmp = std::env::temp_dir().join(format!("opengent-mcp-{}", uuid::Uuid::new_v4()));
        let root = resolve_data_root(Some(&tmp)).unwrap();
        (
            McpStore::load(&root, Arc::new(MemorySecretStore::default())).unwrap(),
            tmp,
        )
    }

    fn input(name: &str) -> McpServerInput {
        McpServerInput {
            id: None,
            name: name.into(),
            transport: McpTransport::Stdio,
            command: "npx -y @scope/server --flag \"a b\"".into(),
            env: vec![
                KeyValueInput {
                    name: "PLAIN".into(),
                    value: "v1".into(),
                    secret: false,
                },
                KeyValueInput {
                    name: "TOKEN".into(),
                    value: "sekret".into(),
                    secret: true,
                },
            ],
            cwd: None,
            url: String::new(),
            headers: Vec::new(),
            default_policy: ToolPolicy::Ask,
            call_timeout_secs: 60,
        }
    }

    #[test]
    fn split_command_quotes_and_escapes() {
        assert_eq!(
            split_command("npx -y \"my server\" --x='a b' c\\ d"),
            vec!["npx", "-y", "my server", "--x=a b", "c d"]
        );
        assert!(split_command("  ").is_empty());
    }

    #[test]
    fn save_persists_secrets_out_of_band() {
        let (s, tmp) = store();
        let saved = s.save(input("files")).unwrap();
        assert_eq!(saved.command, "npx");
        assert_eq!(saved.args, vec!["-y", "@scope/server", "--flag", "a b"]);
        let raw = std::fs::read_to_string(tmp.join("mcp.json")).unwrap();
        assert!(!raw.contains("sekret"), "secret leaked into mcp.json");
        let (env, _) = s.resolved_entries(&s.get(&saved.id).unwrap()).unwrap();
        assert_eq!(env.get("TOKEN").map(String::as_str), Some("sekret"));
        assert_eq!(env.get("PLAIN").map(String::as_str), Some("v1"));

        // Editing with an empty secret value keeps the stored secret.
        let mut edit = input("files2");
        edit.id = Some(saved.id.clone());
        edit.env[1].value = String::new();
        let saved2 = s.save(edit).unwrap();
        assert!(saved2.env[1].has_value);
        let (env, _) = s.resolved_entries(&saved2).unwrap();
        assert_eq!(env.get("TOKEN").map(String::as_str), Some("sekret"));
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn policies_and_settings_round_trip() {
        let (s, tmp) = store();
        let saved = s.save(input("x")).unwrap();
        s.set_tool_policy(&saved.id, "danger", ToolPolicy::Block)
            .unwrap();
        s.set_default_policy(&saved.id, ToolPolicy::AlwaysAllow)
            .unwrap();
        assert_eq!(s.policy_for(&saved.id, "danger"), ToolPolicy::Block);
        assert_eq!(s.policy_for(&saved.id, "other"), ToolPolicy::AlwaysAllow);
        s.set_settings(ToolSettings {
            loading: crate::agent::tools::ToolLoading::OnDemand,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            s.settings().loading,
            crate::agent::tools::ToolLoading::OnDemand
        );
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn http_server_validates_url() {
        let (s, tmp) = store();
        let mut i = input("remote");
        i.transport = McpTransport::StreamableHttp;
        i.url = "http://localhost:8900/mcp".into();
        i.command = String::new();
        i.headers = vec![KeyValueInput {
            name: "Authorization".into(),
            value: "Bearer t".into(),
            secret: true,
        }];
        let c = s.save(i).unwrap();
        assert_eq!(c.url, "http://localhost:8900/mcp");
        let (_, headers) = s.resolved_entries(&c).unwrap();
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer t")
        );
        let mut bad = input("r2");
        bad.transport = McpTransport::StreamableHttp;
        bad.url = "http://example.com/mcp".into();
        assert!(s.save(bad).is_err(), "plain http off-loopback must fail");
        std::fs::remove_dir_all(tmp).unwrap();
    }
}
