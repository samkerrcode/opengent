//! MCP server configuration. Non-secret fields persist as JSON under the data root
//! (`mcp.json`, same pattern as `providers.json`). Values marked `secret` (env vars,
//! HTTP headers) live only in the `SecretStore` under `mcp/<id>/…`; the config records
//! a `has_value` flag, never the value.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::server_meta::{InstructionsStatus, ServerMeta};
use crate::agent::tools::ToolSettings;
use crate::config::{write_private, DataRoot};
use crate::domain::{ToolAccess, ToolPolicy};
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

/// How a remote (Streamable HTTP) server authenticates us, besides any custom headers.
/// Client secrets and tokens live in the keychain under `mcp/<id>/oauth/…`; only
/// `has_client_secret` is recorded here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum McpAuth {
    /// No OAuth: unauthenticated, or credentials in custom headers.
    #[default]
    None,
    /// Authorization code + PKCE in the system browser with a loopback redirect. Without
    /// a client ID the client registers itself (Dynamic Client Registration).
    OauthUser {
        #[serde(default)]
        client_id: Option<String>,
        #[serde(default)]
        has_client_secret: bool,
        #[serde(default)]
        scopes: Vec<String>,
        /// Fixed loopback callback port for authorization servers that require an exact
        /// redirect URI; `None` picks a free port per sign-in.
        #[serde(default)]
        redirect_port: Option<u16>,
    },
    /// Machine-to-machine: the client ID + secret are exchanged for a token directly.
    OauthClientCredentials {
        client_id: String,
        #[serde(default)]
        has_client_secret: bool,
        #[serde(default)]
        scopes: Vec<String>,
    },
}

impl McpAuth {
    pub fn is_oauth(&self) -> bool {
        !matches!(self, McpAuth::None)
    }

    pub fn client_id(&self) -> Option<&str> {
        match self {
            McpAuth::None => None,
            McpAuth::OauthUser { client_id, .. } => client_id.as_deref(),
            McpAuth::OauthClientCredentials { client_id, .. } => Some(client_id),
        }
    }

    fn has_client_secret(&self) -> bool {
        matches!(
            self,
            McpAuth::OauthUser {
                has_client_secret: true,
                ..
            } | McpAuth::OauthClientCredentials {
                has_client_secret: true,
                ..
            }
        )
    }

    pub fn scopes(&self) -> &[String] {
        match self {
            McpAuth::None => &[],
            McpAuth::OauthUser { scopes, .. } | McpAuth::OauthClientCredentials { scopes, .. } => {
                scopes
            }
        }
    }

    fn kind(&self) -> u8 {
        match self {
            McpAuth::None => 0,
            McpAuth::OauthUser { .. } => 1,
            McpAuth::OauthClientCredentials { .. } => 2,
        }
    }
}

/// Authentication settings from the UI. `client_secret` empty keeps the stored secret
/// (when the client ID is unchanged); `scopes` is space-separated.
#[derive(Debug, Clone, Default, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum McpAuthInput {
    #[default]
    None,
    OauthUser {
        #[serde(default)]
        client_id: String,
        #[serde(default)]
        client_secret: String,
        #[serde(default)]
        scopes: String,
        #[serde(default)]
        redirect_port: Option<u16>,
    },
    OauthClientCredentials {
        #[serde(default)]
        client_id: String,
        #[serde(default)]
        client_secret: String,
        #[serde(default)]
        scopes: String,
    },
}

const MAX_SCOPES: usize = 32;
const MAX_SCOPE_LEN: usize = 128;
const MAX_CLIENT_ID_LEN: usize = 512;
const MAX_CLIENT_SECRET_LEN: usize = 1024;

/// RFC 6749 §3.3 scope tokens: printable ASCII except space, `"` and `\`.
fn parse_scopes(raw: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for s in raw.split_whitespace() {
        if s.len() > MAX_SCOPE_LEN
            || !s
                .chars()
                .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
        {
            return Err(Error::InvalidInput(format!(
                "invalid scope `{}`",
                s.chars().take(40).collect::<String>()
            )));
        }
        if !out.iter().any(|o| o == s) {
            out.push(s.to_owned());
        }
    }
    if out.len() > MAX_SCOPES {
        return Err(Error::InvalidInput(format!("at most {MAX_SCOPES} scopes")));
    }
    Ok(out)
}

fn parse_client_id(raw: &str) -> Result<Option<String>> {
    let id = raw.trim();
    if id.len() > MAX_CLIENT_ID_LEN || id.chars().any(char::is_control) {
        return Err(Error::InvalidInput("invalid client ID".into()));
    }
    Ok((!id.is_empty()).then(|| id.to_owned()))
}

/// Validates the auth input against the stored auth. Returns the auth to store and a new
/// client secret to write, if one was entered. A blank secret keeps the stored one only
/// while the client ID is unchanged — a secret belongs to its client.
fn resolve_auth(input: McpAuthInput, old: Option<&McpAuth>) -> Result<(McpAuth, Option<String>)> {
    let secret_input = |client_id: Option<&str>, secret: &str| -> Result<(bool, Option<String>)> {
        let secret = secret.trim();
        if secret.len() > MAX_CLIENT_SECRET_LEN || secret.chars().any(char::is_control) {
            return Err(Error::InvalidInput("invalid client secret".into()));
        }
        if !secret.is_empty() {
            return Ok((true, Some(secret.to_owned())));
        }
        let kept = client_id.is_some()
            && old.is_some_and(|o| o.has_client_secret() && o.client_id() == client_id);
        Ok((kept, None))
    };
    Ok(match input {
        McpAuthInput::None => (McpAuth::None, None),
        McpAuthInput::OauthUser {
            client_id,
            client_secret,
            scopes,
            redirect_port,
        } => {
            let client_id = parse_client_id(&client_id)?;
            if client_id.is_none() && !client_secret.trim().is_empty() {
                return Err(Error::InvalidInput(
                    "a client secret needs a client ID".into(),
                ));
            }
            if redirect_port.is_some_and(|p| p < 1024) {
                return Err(Error::InvalidInput(
                    "callback port must be 1024–65535".into(),
                ));
            }
            let (has_client_secret, secret) = secret_input(client_id.as_deref(), &client_secret)?;
            (
                McpAuth::OauthUser {
                    client_id,
                    has_client_secret,
                    scopes: parse_scopes(&scopes)?,
                    redirect_port,
                },
                secret,
            )
        }
        McpAuthInput::OauthClientCredentials {
            client_id,
            client_secret,
            scopes,
        } => {
            let Some(client_id) = parse_client_id(&client_id)? else {
                return Err(Error::InvalidInput(
                    "client credentials need a client ID".into(),
                ));
            };
            let (has_client_secret, secret) = secret_input(Some(&client_id), &client_secret)?;
            if !has_client_secret {
                return Err(Error::InvalidInput(
                    "client credentials need a client secret".into(),
                ));
            }
            (
                McpAuth::OauthClientCredentials {
                    client_id,
                    has_client_secret,
                    scopes: parse_scopes(&scopes)?,
                },
                secret,
            )
        }
    })
}

/// Whether `input` would change the stored auth (a new secret counts as a change).
fn auth_changed(input: &McpAuthInput, old: &McpAuth) -> bool {
    let norm = |s: &str| s.trim().to_owned();
    match (input, old) {
        (McpAuthInput::None, McpAuth::None) => false,
        (
            McpAuthInput::OauthUser {
                client_id,
                client_secret,
                scopes,
                redirect_port,
            },
            McpAuth::OauthUser {
                client_id: old_id,
                scopes: old_scopes,
                redirect_port: old_port,
                ..
            },
        ) => {
            old_id.as_deref().unwrap_or("") != norm(client_id)
                || !client_secret.trim().is_empty()
                || parse_scopes(scopes).ok().as_ref() != Some(old_scopes)
                || redirect_port != old_port
        }
        (
            McpAuthInput::OauthClientCredentials {
                client_id,
                client_secret,
                scopes,
            },
            McpAuth::OauthClientCredentials {
                client_id: old_id,
                scopes: old_scopes,
                ..
            },
        ) => {
            *old_id != norm(client_id)
                || !client_secret.trim().is_empty()
                || parse_scopes(scopes).ok().as_ref() != Some(old_scopes)
        }
        _ => true,
    }
}

pub(crate) const CLIENT_SECRET: &str = "client_secret";

/// Keychain key for a server's OAuth material (`mcp/<id>/oauth/<name>`).
pub(crate) fn oauth_ref(server_id: &str, name: &str) -> Result<SecretRef> {
    secret_ref(server_id, "oauth", name)
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
    #[serde(default)]
    pub auth: McpAuth,
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
    pub auth: McpAuthInput,
    #[serde(default)]
    pub default_policy: ToolPolicy,
    #[serde(default = "default_call_timeout")]
    #[ts(type = "number")]
    pub call_timeout_secs: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(serialize_with = "current_version")]
    version: u16,
    #[serde(default, serialize_with = "servers_without_secrets")]
    servers: Vec<McpServerConfig>,
    #[serde(default)]
    settings: ToolSettings,
    /// Definition signatures (`server_id -> tool -> sha256`) captured when a tool was
    /// granted `AlwaysAllow`. A server that later changes the tool's description or schema
    /// loses the grant and falls back to `Ask` until re-approved.
    #[serde(default)]
    signatures: HashMap<String, HashMap<String, String>>,
    /// Last tool list seen per server (`server_id -> tools`), so permissions can be edited
    /// and grants pinned while a server is disabled or offline. Display/pinning only —
    /// dispatch always requires the live tool.
    #[serde(default)]
    catalog: HashMap<String, Vec<KnownTool>>,
    /// Pins for chat/agent-level `AlwaysAllow` grants: `scope key -> server -> tool -> sig`.
    /// Kept apart from the global pins so re-approving in one scope can't re-validate a
    /// stale grant in another.
    #[serde(default)]
    scoped_signatures: HashMap<String, HashMap<String, HashMap<String, String>>>,
    /// Handshake metadata (title, icon, instructions) last seen per server, for display
    /// while it is offline. Untrusted; bounded by `ServerMeta::from_handshake` and again
    /// on load.
    #[serde(default)]
    server_meta: HashMap<String, ServerMeta>,
    /// Signature of the instructions version accepted per server (pinned on first connect
    /// or when the user turns them back on). Changed instructions are left out of the
    /// system prompt until accepted again.
    #[serde(default)]
    instruction_pins: HashMap<String, String>,
    /// Servers whose instructions the user excluded from the system prompt.
    #[serde(default)]
    instructions_off: HashSet<String>,
}

fn current_version<S: serde::Serializer>(_: &u16, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_u16(1)
}

fn servers_without_secrets<S: serde::Serializer>(
    servers: &[McpServerConfig],
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    s.collect_seq(servers.iter().map(|srv| {
        let mut srv = srv.clone();
        // Never write secret values to disk.
        for e in srv.env.iter_mut().chain(srv.headers.iter_mut()) {
            if e.secret {
                e.value.clear();
            }
        }
        srv
    }))
}

/// Bounds on cached server-provided metadata (untrusted, persisted to disk).
const MAX_CACHED_TOOLS: usize = 500;
const MAX_CACHED_NAME: usize = 128;
const MAX_CACHED_TITLE: usize = 128;
const MAX_CACHED_DESC: usize = 256;

/// A tool as last listed by its server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownTool {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub access: ToolAccess,
    /// Definition signature (see `manager::tool_signature`).
    pub signature: String,
}

impl KnownTool {
    fn bounded(mut self) -> Self {
        let cap = |s: &str, n: usize| s.chars().take(n).collect::<String>();
        self.title = self.title.map(|t| cap(&t, MAX_CACHED_TITLE));
        self.description = self.description.map(|d| cap(&d, MAX_CACHED_DESC));
        self
    }
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

/// Whether env/header inputs differ from the stored entries. A secret row with an empty
/// value keeps the stored secret, so it only counts as a change if it wasn't secret before.
fn entries_changed(inputs: &[KeyValueInput], old: &[KeyValueEntry]) -> bool {
    inputs.len() != old.len()
        || inputs.iter().any(|e| {
            let name = e.name.trim();
            match old.iter().find(|o| o.name == name) {
                None => true,
                Some(o) if e.secret => !o.secret || !e.value.is_empty(),
                Some(o) => o.secret || o.value != e.value,
            }
        })
}

impl McpStore {
    pub fn load(data_root: &DataRoot, secrets: Arc<dyn SecretStore>) -> Result<Self> {
        let path = data_root.path().join("mcp.json");
        let mut state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<StoreFile>(&bytes)
                .map_err(|e| Error::Io(format!("mcp.json is corrupt: {e}")))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreFile {
                version: 1,
                ..Default::default()
            },
            Err(e) => return Err(e.into()),
        };
        state.server_meta = std::mem::take(&mut state.server_meta)
            .into_iter()
            .map(|(id, m)| (id, m.bounded()))
            .collect();
        Ok(Self {
            path,
            state: Mutex::new(state),
            secrets,
        })
    }

    fn persist(&self, state: &StoreFile) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(state).map_err(|e| Error::Internal(e.to_string()))?;
        write_private(&tmp, &bytes)?;
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

    /// Stored policy for a tool: explicit override → server default. Does not check the
    /// definition signature; use `effective_policy` at dispatch.
    pub fn policy_for(&self, server_id: &str, tool: &str) -> ToolPolicy {
        let state = self.state.lock().unwrap();
        state
            .servers
            .iter()
            .find(|s| s.id == server_id)
            .map(|s| {
                s.tool_policies
                    .get(tool)
                    .copied()
                    .unwrap_or(s.default_policy)
            })
            .unwrap_or(ToolPolicy::Ask)
    }

    /// Dispatch-time policy. `AlwaysAllow` (explicit or via the server default) is honored
    /// only while the tool's definition signature matches the pinned one; a changed tool
    /// falls back to `Ask`. Grants are normally pinned when made (from the live or cached
    /// definition) or when a default-allowed tool is first listed (`set_catalog`); only a
    /// tool with no pin at all — a config predating signatures or the catalog cache — is
    /// pinned on first use. A tool with no live definition can't be verified and gets `Ask`.
    pub fn effective_policy(
        &self,
        server_id: &str,
        tool: &str,
        current_sig: Option<&str>,
    ) -> ToolPolicy {
        let raw = self.policy_for(server_id, tool);
        if raw != ToolPolicy::AlwaysAllow {
            return raw;
        }
        self.verify_global_allow(server_id, tool, current_sig)
    }

    /// Verifies a connection-level `AlwaysAllow` against the global pin (see
    /// `effective_policy`); returns `AlwaysAllow` or `Ask`.
    pub fn verify_global_allow(
        &self,
        server_id: &str,
        tool: &str,
        current_sig: Option<&str>,
    ) -> ToolPolicy {
        let Some(current) = current_sig else {
            return ToolPolicy::Ask;
        };
        let mut state = self.state.lock().unwrap();
        match state.signatures.get(server_id).and_then(|m| m.get(tool)) {
            Some(pinned) if pinned == current => ToolPolicy::AlwaysAllow,
            Some(_) => ToolPolicy::Ask,
            None => {
                state
                    .signatures
                    .entry(server_id.to_owned())
                    .or_default()
                    .insert(tool.to_owned(), current.to_owned());
                if let Err(e) = self.persist(&state) {
                    tracing::warn!(error = %e, "failed to persist tool signature");
                }
                ToolPolicy::AlwaysAllow
            }
        }
    }

    /// Verifies a scope-level `AlwaysAllow` (chat/agent) against the scope's own pin. Scoped
    /// grants are pinned when made (`sync_scope_pins`, `pin_scoped`), never on first use,
    /// so a missing or mismatched pin means `Ask`.
    pub fn verify_scoped_allow(
        &self,
        scope_key: &str,
        server_id: &str,
        tool: &str,
        current_sig: Option<&str>,
    ) -> ToolPolicy {
        let state = self.state.lock().unwrap();
        let pinned = state
            .scoped_signatures
            .get(scope_key)
            .and_then(|m| m.get(server_id))
            .and_then(|m| m.get(tool));
        match (pinned, current_sig) {
            (Some(p), Some(c)) if p == c => ToolPolicy::AlwaysAllow,
            _ => ToolPolicy::Ask,
        }
    }

    /// Whether a scope's pin for the tool no longer matches `current_sig` (or is missing).
    pub fn scoped_grant_stale(
        &self,
        scope_key: &str,
        server_id: &str,
        tool: &str,
        current_sig: &str,
    ) -> bool {
        let state = self.state.lock().unwrap();
        state
            .scoped_signatures
            .get(scope_key)
            .and_then(|m| m.get(server_id))
            .and_then(|m| m.get(tool))
            .is_none_or(|s| s != current_sig)
    }

    /// Pins (or, without a signature, unpins) a scope grant the user just approved against
    /// the definition they saw — the approval card's "Always allow".
    pub fn pin_scoped(&self, scope_key: &str, server_id: &str, tool: &str, sig: Option<String>) {
        let mut state = self.state.lock().unwrap();
        let pins = state
            .scoped_signatures
            .entry(scope_key.to_owned())
            .or_default()
            .entry(server_id.to_owned())
            .or_default();
        match sig {
            Some(s) => pins.insert(tool.to_owned(), s),
            None => pins.remove(tool),
        };
        if let Err(e) = self.persist(&state) {
            tracing::warn!(error = %e, "failed to persist scoped tool signature");
        }
    }

    /// Reconciles a scope's pins with its current `AlwaysAllow` grants (`server, tool,
    /// current signature`). Existing pins are kept (a stale grant stays stale); a new grant
    /// copies the **global** pin when there is one — so copying a connection's Allow can't
    /// launder a changed tool into a fresh pin — else pins the current definition. Pins of
    /// tools that are no longer allowed are dropped.
    pub fn sync_scope_pins(&self, scope_key: &str, grants: Vec<(String, String, Option<String>)>) {
        let mut state = self.state.lock().unwrap();
        let old = state
            .scoped_signatures
            .get(scope_key)
            .cloned()
            .unwrap_or_default();
        let mut next: HashMap<String, HashMap<String, String>> = HashMap::new();
        for (server, tool, current) in grants {
            let pin = old
                .get(&server)
                .and_then(|m| m.get(&tool))
                .or_else(|| state.signatures.get(&server).and_then(|m| m.get(&tool)))
                .cloned()
                .or(current);
            if let Some(pin) = pin {
                next.entry(server).or_default().insert(tool, pin);
            }
        }
        if next == old {
            return;
        }
        if next.is_empty() {
            state.scoped_signatures.remove(scope_key);
        } else {
            state.scoped_signatures.insert(scope_key.to_owned(), next);
        }
        if let Err(e) = self.persist(&state) {
            tracing::warn!(error = %e, "failed to persist scoped tool signatures");
        }
    }

    /// Drops a deleted chat's or agent's pins.
    pub fn forget_scope(&self, scope_key: &str) {
        let mut state = self.state.lock().unwrap();
        if state.scoped_signatures.remove(scope_key).is_some() {
            if let Err(e) = self.persist(&state) {
                tracing::warn!(error = %e, "failed to persist scoped tool signatures");
            }
        }
    }

    /// Startup cleanup: drops pins of scopes for which `keep` is false.
    pub fn prune_scopes(&self, keep: impl Fn(&str) -> bool) {
        let mut state = self.state.lock().unwrap();
        let before = state.scoped_signatures.len();
        state.scoped_signatures.retain(|k, _| keep(k));
        if state.scoped_signatures.len() != before {
            if let Err(e) = self.persist(&state) {
                tracing::warn!(error = %e, "failed to persist scoped tool signatures");
            }
        }
    }

    /// Whether the tool's pinned definition signature no longer matches the live one, i.e.
    /// an `AlwaysAllow` would currently be downgraded to `Ask` ("changed" badge in the UI).
    pub fn grant_stale(&self, server_id: &str, tool: &str, current_sig: &str) -> bool {
        let state = self.state.lock().unwrap();
        state
            .signatures
            .get(server_id)
            .and_then(|m| m.get(tool))
            .is_some_and(|s| s != current_sig)
    }

    /// `signature` should be the tool's definition signature — live when connected, else
    /// from the cached catalog. Without one (never listed) any old pin is dropped, and the
    /// definition is pinned when the server next lists it.
    pub fn set_tool_policy(
        &self,
        server_id: &str,
        tool: &str,
        policy: ToolPolicy,
        signature: Option<String>,
    ) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let s = state
            .servers
            .iter_mut()
            .find(|s| s.id == server_id)
            .ok_or_else(|| Error::NotFound(format!("MCP server `{server_id}`")))?;
        // Storing an explicit entry equal to the default is harmless and survives later
        // default changes; keep it simple and always record it.
        s.tool_policies.insert(tool.to_owned(), policy);
        let pins = state.signatures.entry(server_id.to_owned()).or_default();
        match signature {
            Some(sig) => pins.insert(tool.to_owned(), sig),
            None => pins.remove(tool),
        };
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
        // A default Allow is a grant for every known tool it covers: pin their current
        // definitions now rather than trusting whatever shows up at first use.
        Self::pin_unpinned_allowed(&mut state, server_id);
        self.persist(&state)
    }

    /// Drops every per-tool override so all tools follow the server default.
    pub fn reset_tool_policies(&self, server_id: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let s = state
            .servers
            .iter_mut()
            .find(|s| s.id == server_id)
            .ok_or_else(|| Error::NotFound(format!("MCP server `{server_id}`")))?;
        s.tool_policies.clear();
        Self::pin_unpinned_allowed(&mut state, server_id);
        self.persist(&state)
    }

    /// Pins the cached signature of every tool on `server_id` whose stored policy is
    /// `AlwaysAllow` and that has no pin yet. Existing (possibly stale) pins are kept, so a
    /// changed tool still needs explicit re-approval. Returns whether anything was pinned.
    fn pin_unpinned_allowed(state: &mut StoreFile, server_id: &str) -> bool {
        let Some(server) = state.servers.iter().find(|s| s.id == server_id) else {
            return false;
        };
        let allowed: Vec<(String, String)> = state
            .catalog
            .get(server_id)
            .into_iter()
            .flatten()
            .filter(|t| {
                server
                    .tool_policies
                    .get(&t.name)
                    .copied()
                    .unwrap_or(server.default_policy)
                    == ToolPolicy::AlwaysAllow
            })
            .map(|t| (t.name.clone(), t.signature.clone()))
            .collect();
        let pins = state.signatures.entry(server_id.to_owned()).or_default();
        let mut changed = false;
        for (name, sig) in allowed {
            if let std::collections::hash_map::Entry::Vacant(e) = pins.entry(name) {
                e.insert(sig);
                changed = true;
            }
        }
        changed
    }

    /// Records the server's current tool list (bounded) and pins newly discovered tools
    /// covered by an `AlwaysAllow` policy. Writes to disk only when something changed.
    pub fn set_catalog(&self, server_id: &str, tools: Vec<KnownTool>) -> Result<()> {
        let tools: Vec<KnownTool> = tools
            .into_iter()
            .filter(|t| !t.name.is_empty() && t.name.chars().count() <= MAX_CACHED_NAME)
            .take(MAX_CACHED_TOOLS)
            .map(KnownTool::bounded)
            .collect();
        let mut state = self.state.lock().unwrap();
        if !state.servers.iter().any(|s| s.id == server_id) {
            return Ok(());
        }
        let changed = state.catalog.get(server_id) != Some(&tools);
        if changed {
            state.catalog.insert(server_id.to_owned(), tools);
        }
        let pinned = Self::pin_unpinned_allowed(&mut state, server_id);
        if changed || pinned {
            self.persist(&state)?;
        }
        Ok(())
    }

    /// The last tool list seen for a server (empty if it never connected).
    pub fn catalog(&self, server_id: &str) -> Vec<KnownTool> {
        self.state
            .lock()
            .unwrap()
            .catalog
            .get(server_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Definition signature of a tool from the cached catalog.
    pub fn cached_signature(&self, server_id: &str, tool: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .catalog
            .get(server_id)?
            .iter()
            .find(|t| t.name == tool)
            .map(|t| t.signature.clone())
    }

    /// Records a server's handshake metadata. The instructions sent on first connect are
    /// accepted (pinned) unless the user turned them off — no instructions pin too, so a
    /// server that adds them later is treated like one that changed them. A changed
    /// version keeps the old pin and is excluded until the user accepts it. Writes only
    /// when something changed.
    pub fn set_server_meta(&self, server_id: &str, meta: ServerMeta) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if !state.servers.iter().any(|s| s.id == server_id) {
            return Ok(());
        }
        let mut changed = state.server_meta.get(server_id) != Some(&meta);
        if !state.instructions_off.contains(server_id)
            && !state.instruction_pins.contains_key(server_id)
        {
            state
                .instruction_pins
                .insert(server_id.to_owned(), meta.signature().to_owned());
            changed = true;
        }
        if changed {
            state.server_meta.insert(server_id.to_owned(), meta);
            self.persist(&state)?;
        }
        Ok(())
    }

    /// Handshake metadata last seen for a server.
    pub fn server_meta(&self, server_id: &str) -> Option<ServerMeta> {
        self.state
            .lock()
            .unwrap()
            .server_meta
            .get(server_id)
            .cloned()
    }

    /// Whether the server's current instructions, by `ServerMeta::signature`, go into the
    /// system prompt.
    pub fn instructions_status(&self, server_id: &str, signature: &str) -> InstructionsStatus {
        let state = self.state.lock().unwrap();
        if state.instructions_off.contains(server_id) {
            return InstructionsStatus::Off;
        }
        match state.instruction_pins.get(server_id) {
            Some(pin) if pin == signature => InstructionsStatus::Included,
            _ => InstructionsStatus::Changed,
        }
    }

    /// Turns a server's instructions off, or on by accepting `shown` — the text the user
    /// reviewed. Accepting fails if the server's instructions changed since (a reconnect
    /// can't slip a different version past the review).
    pub fn set_instructions_included(&self, server_id: &str, shown: Option<&str>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if !state.servers.iter().any(|s| s.id == server_id) {
            return Err(Error::NotFound(format!("MCP server `{server_id}`")));
        }
        match shown {
            Some(shown) => {
                let sig = match state.server_meta.get(server_id) {
                    Some(m) if m.instructions.as_deref() == Some(shown) => m.signature().to_owned(),
                    _ => {
                        return Err(Error::InvalidInput(
                            "the server's instructions changed — review them again".into(),
                        ))
                    }
                };
                state.instructions_off.remove(server_id);
                state.instruction_pins.insert(server_id.to_owned(), sig);
            }
            None => {
                state.instructions_off.insert(server_id.to_owned());
            }
        }
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
                let url = normalize_base_url(&input.url).map_err(|e| match e {
                    Error::InvalidInput(m) => Error::InvalidInput(format!("server URL: {m}")),
                    other => other,
                })?;
                let parsed = reqwest::Url::parse(&url)
                    .map_err(|e| Error::InvalidInput(format!("server URL: {e}")))?;
                if !parsed.username().is_empty() || parsed.password().is_some() {
                    return Err(Error::InvalidInput(
                        "credentials in the URL are not allowed; use headers".into(),
                    ));
                }
                (String::new(), Vec::new())
            }
        };
        const MAX_ENTRIES: usize = 64;
        if input.env.len() > MAX_ENTRIES || input.headers.len() > MAX_ENTRIES {
            return Err(Error::InvalidInput(format!(
                "at most {MAX_ENTRIES} env/header entries per server"
            )));
        }
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
        // Env names must be valid shell identifiers; header names/values must be legal
        // HTTP field syntax (rejects control characters / CRLF smuggling).
        for e in &input.env {
            let n = e.name.trim();
            let valid = n
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !valid {
                return Err(Error::InvalidInput(format!(
                    "invalid environment variable name `{n}`"
                )));
            }
        }
        for e in &input.headers {
            let n = e.name.trim();
            n.parse::<reqwest::header::HeaderName>()
                .map_err(|_| Error::InvalidInput(format!("invalid header name `{n}`")))?;
            e.value
                .parse::<reqwest::header::HeaderValue>()
                .map_err(|_| Error::InvalidInput(format!("invalid value for header `{n}`")))?;
        }
        let auth_input = input.auth;
        if !matches!(auth_input, McpAuthInput::None) {
            if input.transport != McpTransport::StreamableHttp {
                return Err(Error::InvalidInput(
                    "OAuth is only available for remote servers".into(),
                ));
            }
            // The OAuth bearer token goes in `Authorization`; a custom one would collide.
            if input
                .headers
                .iter()
                .any(|h| h.name.trim().eq_ignore_ascii_case("authorization"))
            {
                return Err(Error::InvalidInput(
                    "remove the Authorization header: OAuth sets it".into(),
                ));
            }
        }

        let mut state = self.state.lock().unwrap();
        let id = match input.id {
            Some(id) if state.servers.iter().any(|s| s.id == id) => id,
            Some(id) => {
                return Err(Error::NotFound(format!("MCP server `{id}`")));
            }
            None => uuid::Uuid::new_v4().to_string(),
        };
        if state.servers.iter().any(|s| s.id != id && s.name == name) {
            return Err(Error::InvalidInput(format!(
                "a server named `{name}` already exists"
            )));
        }
        let existing = state.servers.iter().find(|s| s.id == id).cloned();

        let map_entries = |scope: &str,
                           inputs: Vec<KeyValueInput>,
                           old: &[KeyValueEntry]|
         -> Result<Vec<KeyValueEntry>> {
            let mut out = Vec::with_capacity(inputs.len());
            // Two secret names that sanitize to the same keychain key would overwrite each
            // other's stored value; reject them.
            let mut secret_keys = HashSet::new();
            for e in inputs {
                let name = e.name.trim().to_owned();
                if e.secret {
                    let r = secret_ref(&id, scope, &name)?;
                    if !secret_keys.insert(r.0.clone()) {
                        return Err(Error::InvalidInput(format!(
                            "secret names `{name}` collide after sanitization"
                        )));
                    }
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

        // Keychain entries for secret rows that disappeared or were unmarked; deleted only
        // after the config persists so a failed save can't orphan a still-referenced secret.
        let mut stale_secrets = Vec::new();
        if let Some(old) = &existing {
            for (scope, old_entries, new_entries) in
                [("env", &old.env, &env), ("header", &old.headers, &headers)]
            {
                let kept: HashSet<&str> = new_entries
                    .iter()
                    .filter(|e| e.secret)
                    .map(|e| e.name.as_str())
                    .collect();
                stale_secrets.extend(
                    old_entries
                        .iter()
                        .filter(|e| e.secret && !kept.contains(e.name.as_str()))
                        .filter_map(|e| secret_ref(&id, scope, &e.name).ok()),
                );
            }
        }

        let old_auth = existing.as_ref().map(|s| &s.auth);
        let (auth, new_secret) = resolve_auth(auth_input, old_auth)?;
        let client_secret_ref = oauth_ref(&id, CLIENT_SECRET)?;
        if let Some(secret) = &new_secret {
            self.secrets.set(&client_secret_ref, secret)?;
        }
        if old_auth.is_some_and(McpAuth::has_client_secret) && !auth.has_client_secret() {
            stale_secrets.push(client_secret_ref);
        }
        let url = match input.transport {
            McpTransport::StreamableHttp => normalize_base_url(&input.url)?,
            McpTransport::Stdio => String::new(),
        };
        // Tokens belong to the server and client they were issued for. A client-credentials
        // token is also re-issued when its scopes or secret change (a browser sign-in
        // keeps its tokens: new scopes need a new sign-in anyway).
        let clear_tokens = existing.as_ref().is_some_and(|old| {
            old.url != url
                || old.auth.kind() != auth.kind()
                || old.auth.client_id() != auth.client_id()
                || (matches!(auth, McpAuth::OauthClientCredentials { .. })
                    && (new_secret.is_some() || old.auth.scopes() != auth.scopes()))
        });

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
            url,
            headers,
            auth,
            default_policy: input.default_policy,
            tool_policies: existing.map(|s| s.tool_policies).unwrap_or_default(),
            call_timeout_secs: input.call_timeout_secs,
        };
        let repointed = match state.servers.iter_mut().find(|s| s.id == id) {
            Some(slot) => {
                let old = std::mem::replace(slot, config.clone());
                old.transport != config.transport
                    || old.command != config.command
                    || old.args != config.args
                    || old.cwd != config.cwd
                    || old.url != config.url
            }
            None => {
                state.servers.push(config.clone());
                false
            }
        };
        // Re-pointed at another program or URL: the cached handshake metadata and the
        // accepted instructions belong to the old server, so the next connect starts over
        // as a first connect. Turning instructions off is the user's choice and is kept.
        if repointed {
            state.server_meta.remove(&id);
            state.instruction_pins.remove(&id);
        }
        Self::pin_unpinned_allowed(&mut state, &id);
        self.persist(&state)?;
        for r in stale_secrets {
            let _ = self.secrets.delete(&r);
        }
        if clear_tokens {
            if let Err(e) = super::oauth::clear_tokens(self.secrets.as_ref(), &id) {
                tracing::warn!(error = %e, "failed to clear stale OAuth tokens");
            }
        }
        Ok(config)
    }

    /// Whether saving `input` would change what a stdio server launches: a new server, or a
    /// different command, args, cwd or environment (e.g. `NODE_OPTIONS=--require …` runs
    /// code as surely as the command does). Such saves need explicit user confirmation.
    pub fn launch_changed(&self, input: &McpServerInput) -> bool {
        if input.transport != McpTransport::Stdio {
            return false;
        }
        let Some(old) = input.id.as_deref().and_then(|id| self.get(id).ok()) else {
            return true;
        };
        let parts = split_command(&input.command);
        let (command, args) = parts
            .split_first()
            .map_or(("", &[][..]), |(c, a)| (c.as_str(), a));
        let cwd = input
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty());
        old.transport != input.transport
            || old.command != command
            || old.args != args
            || old.cwd.as_deref() != cwd
            || entries_changed(&input.env, &old.env)
    }

    /// Whether saving `input` changes how the server is reached — launch settings for stdio
    /// or URL/headers for HTTP — so an existing session is out of date. A plain save that
    /// only renames or edits permissions/timeouts keeps the current connection.
    pub fn connection_changed(&self, input: &McpServerInput) -> bool {
        let Some(old) = input.id.as_deref().and_then(|id| self.get(id).ok()) else {
            return true;
        };
        match input.transport {
            McpTransport::Stdio => self.launch_changed(input),
            McpTransport::StreamableHttp => {
                old.transport != input.transport
                    || normalize_base_url(&input.url).map_or(true, |u| u != old.url)
                    || entries_changed(&input.headers, &old.headers)
                    || auth_changed(&input.auth, &old.auth)
            }
        }
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
        state.signatures.remove(id);
        state.catalog.remove(id);
        state.server_meta.remove(id);
        state.instruction_pins.remove(id);
        state.instructions_off.remove(id);
        for pins in state.scoped_signatures.values_mut() {
            pins.remove(id);
        }
        debug_assert!(state.servers.len() < before);
        for (scope, entries) in [("env", &removed.env), ("header", &removed.headers)] {
            for e in entries.iter().filter(|e| e.secret) {
                let _ = self.secrets.delete(&secret_ref(id, scope, &e.name)?);
            }
        }
        let _ = self.secrets.delete(&oauth_ref(id, CLIENT_SECRET)?);
        let _ = super::oauth::clear_tokens(self.secrets.as_ref(), id);
        self.persist(&state)
    }

    /// The secret store backing this config (OAuth token storage in `mcp::oauth`).
    pub(crate) fn secrets(&self) -> Arc<dyn SecretStore> {
        Arc::clone(&self.secrets)
    }

    /// The stored OAuth client secret, if the server's auth records one.
    pub fn oauth_client_secret(&self, config: &McpServerConfig) -> Result<Option<String>> {
        if !config.auth.has_client_secret() {
            return Ok(None);
        }
        self.secrets.get(&oauth_ref(&config.id, CLIENT_SECRET)?)
    }

    /// Whether OAuth tokens are stored for the server (reads the keychain).
    pub fn has_oauth_tokens(&self, id: &str) -> bool {
        super::oauth::read_tokens(self.secrets.as_ref(), id).is_ok_and(|t| t.is_some())
    }

    /// Forgets the server's OAuth tokens (sign out).
    pub fn clear_oauth_tokens(&self, id: &str) -> Result<()> {
        super::oauth::clear_tokens(self.secrets.as_ref(), id)
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

    /// The plaintext values of this server's secrets (env vars and headers), for stderr
    /// redaction at the manager. Values shorter than 4 chars are skipped to avoid
    /// over-redacting on trivial substrings.
    pub fn secret_values(&self, config: &McpServerConfig) -> Vec<String> {
        let mut out = Vec::new();
        for (scope, entries) in [("env", &config.env), ("header", &config.headers)] {
            for e in entries.iter().filter(|e| e.secret && e.has_value) {
                let v = secret_ref(&config.id, scope, &e.name).and_then(|r| self.secrets.get(&r));
                if let Ok(Some(v)) = v {
                    if v.chars().count() >= 4 {
                        out.push(v);
                    }
                }
            }
        }
        out
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
            auth: McpAuthInput::None,
            default_policy: ToolPolicy::Ask,
            call_timeout_secs: 60,
        }
    }

    #[test]
    fn instructions_are_pinned_on_first_sight_and_changes_need_acceptance() {
        use InstructionsStatus::*;
        let (s, tmp) = store();
        let id = s.save(input("gh")).unwrap().id;
        let meta = |t: &str| ServerMeta::from_handshake(None, Some(t));
        let sig = |t: &str| meta(t).signature().to_owned();
        s.set_server_meta(&id, meta("v1")).unwrap();
        assert_eq!(s.instructions_status(&id, &sig("v1")), Included);
        // A new version is left out until accepted.
        s.set_server_meta(&id, meta("v2")).unwrap();
        assert_eq!(s.instructions_status(&id, &sig("v2")), Changed);
        assert!(s.set_instructions_included(&id, Some("v1")).is_err());
        s.set_instructions_included(&id, Some("v2")).unwrap();
        assert_eq!(s.instructions_status(&id, &sig("v2")), Included);
        // Off survives reloads and blocks first-sight pinning.
        s.set_instructions_included(&id, None).unwrap();
        let reloaded = McpStore::load(
            &resolve_data_root(Some(&tmp)).unwrap(),
            Arc::new(MemorySecretStore::default()),
        )
        .unwrap();
        assert_eq!(reloaded.instructions_status(&id, &sig("v2")), Off);
        assert_eq!(
            reloaded
                .server_meta(&id)
                .and_then(|m| m.instructions)
                .as_deref(),
            Some("v2")
        );
        reloaded.delete(&id).unwrap();
        assert!(reloaded.server_meta(&id).is_none());
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn instructions_added_after_first_connect_need_acceptance() {
        use InstructionsStatus::*;
        let (s, tmp) = store();
        let id = s.save(input("gh")).unwrap().id;
        s.set_server_meta(&id, ServerMeta::from_handshake(None, None))
            .unwrap();
        let v1 = ServerMeta::from_handshake(None, Some("v1"));
        s.set_server_meta(&id, v1.clone()).unwrap();
        assert_eq!(s.instructions_status(&id, v1.signature()), Changed);
        s.set_instructions_included(&id, Some("v1")).unwrap();
        assert_eq!(s.instructions_status(&id, v1.signature()), Included);
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn repointing_a_connection_resets_instruction_trust() {
        use InstructionsStatus::*;
        let (s, tmp) = store();
        let mut i = input("gh");
        i.id = Some(s.save(i.clone()).unwrap().id);
        let id = i.id.clone().unwrap();
        let (v1, v2) = (
            ServerMeta::from_handshake(None, Some("v1")),
            ServerMeta::from_handshake(None, Some("v2")),
        );
        s.set_server_meta(&id, v1.clone()).unwrap();
        // Renaming keeps the pin, so a new version still needs review.
        i.name = "github".into();
        s.save(i.clone()).unwrap();
        s.set_server_meta(&id, v2.clone()).unwrap();
        assert_eq!(s.instructions_status(&id, v2.signature()), Changed);
        // A different command is a different server: its first instructions are accepted.
        i.command = "other-server --stdio".into();
        s.save(i.clone()).unwrap();
        assert!(s.server_meta(&id).is_none());
        s.set_server_meta(&id, v2.clone()).unwrap();
        assert_eq!(s.instructions_status(&id, v2.signature()), Included);
        // Off is the user's choice and survives re-pointing.
        s.set_instructions_included(&id, None).unwrap();
        i.command = "third-server".into();
        s.save(i).unwrap();
        s.set_server_meta(&id, v1.clone()).unwrap();
        assert_eq!(s.instructions_status(&id, v1.signature()), Off);
        std::fs::remove_dir_all(tmp).unwrap();
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
        s.set_tool_policy(&saved.id, "danger", ToolPolicy::Block, None)
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
    fn always_allow_pins_signature_on_first_use_and_drops_on_change() {
        let (s, tmp) = store();
        let id = s.save(input("x")).unwrap().id;
        // Server-default allow with no pin yet: allowed and pinned (not silently Ask).
        s.set_default_policy(&id, ToolPolicy::AlwaysAllow).unwrap();
        assert_eq!(
            s.effective_policy(&id, "t", Some("sig-a")),
            ToolPolicy::AlwaysAllow
        );
        assert!(!s.grant_stale(&id, "t", "sig-a"));
        // Definition changed behind the grant: Ask + stale badge.
        assert_eq!(s.effective_policy(&id, "t", Some("sig-b")), ToolPolicy::Ask);
        assert!(s.grant_stale(&id, "t", "sig-b"));
        // Not live: unverifiable.
        assert_eq!(s.effective_policy(&id, "t", None), ToolPolicy::Ask);
        // Re-approval re-pins.
        s.set_tool_policy(&id, "t", ToolPolicy::AlwaysAllow, Some("sig-b".into()))
            .unwrap();
        assert_eq!(
            s.effective_policy(&id, "t", Some("sig-b")),
            ToolPolicy::AlwaysAllow
        );
        // A grant made while offline drops the old pin and re-pins on next use.
        s.set_tool_policy(&id, "t", ToolPolicy::AlwaysAllow, None)
            .unwrap();
        assert_eq!(
            s.effective_policy(&id, "t", Some("sig-c")),
            ToolPolicy::AlwaysAllow
        );
        // Block is never affected by signatures.
        s.set_tool_policy(&id, "t", ToolPolicy::Block, Some("sig-c".into()))
            .unwrap();
        assert_eq!(
            s.effective_policy(&id, "t", Some("sig-c")),
            ToolPolicy::Block
        );
        std::fs::remove_dir_all(tmp).unwrap();
    }

    fn known(name: &str, sig: &str) -> KnownTool {
        KnownTool {
            name: name.into(),
            title: None,
            description: Some("d".repeat(MAX_CACHED_DESC + 10)),
            access: ToolAccess::Unannotated,
            signature: sig.into(),
        }
    }

    #[test]
    fn catalog_is_bounded_persisted_and_cleared_on_delete() {
        let (s, tmp) = store();
        let id = s.save(input("x")).unwrap().id;
        let mut tools = vec![
            known("a", "sig-a"),
            known(&"n".repeat(MAX_CACHED_NAME + 1), "x"),
        ];
        tools.extend((0..MAX_CACHED_TOOLS + 5).map(|i| known(&format!("t{i}"), "s")));
        s.set_catalog(&id, tools).unwrap();
        let cat = s.catalog(&id);
        assert_eq!(
            cat.len(),
            MAX_CACHED_TOOLS,
            "capped, oversized name dropped"
        );
        assert_eq!(cat[0].description.as_ref().unwrap().len(), MAX_CACHED_DESC);
        assert_eq!(s.cached_signature(&id, "a").as_deref(), Some("sig-a"));
        let root = resolve_data_root(Some(&tmp)).unwrap();
        let reloaded = McpStore::load(&root, Arc::new(MemorySecretStore::default())).unwrap();
        assert_eq!(reloaded.catalog(&id).len(), MAX_CACHED_TOOLS);
        s.set_catalog("unknown-server", vec![known("a", "s")])
            .unwrap();
        assert!(s.catalog("unknown-server").is_empty());
        s.delete(&id).unwrap();
        assert!(s.catalog(&id).is_empty());
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn default_allow_pins_at_discovery_not_first_use() {
        let (s, tmp) = store();
        let id = s.save(input("x")).unwrap().id;
        s.set_default_policy(&id, ToolPolicy::AlwaysAllow).unwrap();
        s.set_catalog(&id, vec![known("t", "sig-a")]).unwrap();
        // The definition changed between listing and first use: not trusted.
        assert_eq!(s.effective_policy(&id, "t", Some("sig-b")), ToolPolicy::Ask);
        assert_eq!(
            s.effective_policy(&id, "t", Some("sig-a")),
            ToolPolicy::AlwaysAllow
        );
        // A later listing with a new definition keeps the original pin (stays stale).
        s.set_catalog(&id, vec![known("t", "sig-b")]).unwrap();
        assert!(s.grant_stale(&id, "t", "sig-b"));
        // Switching the default to Allow pins known tools that weren't pinned yet.
        let id2 = s.save(input("y")).unwrap().id;
        s.set_catalog(&id2, vec![known("u", "sig-u")]).unwrap();
        s.set_default_policy(&id2, ToolPolicy::AlwaysAllow).unwrap();
        assert_eq!(
            s.effective_policy(&id2, "u", Some("other")),
            ToolPolicy::Ask
        );
        // Reset clears overrides back to the default.
        s.set_tool_policy(&id2, "u", ToolPolicy::Block, None)
            .unwrap();
        s.reset_tool_policies(&id2).unwrap();
        assert_eq!(s.policy_for(&id2, "u"), ToolPolicy::AlwaysAllow);
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn connection_changed_ignores_renames_and_permissions() {
        let (s, tmp) = store();
        assert!(s.connection_changed(&input("x")), "new server");
        let id = s.save(input("x")).unwrap().id;
        let mut i = input("renamed");
        i.id = Some(id.clone());
        i.env[1].value = String::new();
        i.default_policy = ToolPolicy::AlwaysAllow;
        i.call_timeout_secs = 30;
        assert!(!s.connection_changed(&i));
        i.command = "npx other".into();
        assert!(s.connection_changed(&i));

        let mut h = input("remote");
        h.transport = McpTransport::StreamableHttp;
        h.command = String::new();
        h.env.clear();
        h.url = "http://localhost:8900/mcp".into();
        let hid = s.save(h.clone()).unwrap().id;
        h.id = Some(hid);
        assert!(!s.connection_changed(&h));
        let mut h2 = h.clone();
        h2.url = "http://localhost:8901/mcp".into();
        assert!(s.connection_changed(&h2));
        let mut h3 = h.clone();
        h3.headers.push(KeyValueInput {
            name: "X-Key".into(),
            value: "v".into(),
            secret: false,
        });
        assert!(s.connection_changed(&h3));
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn launch_changed_covers_command_cwd_and_env() {
        let (s, tmp) = store();
        assert!(
            s.launch_changed(&input("x")),
            "new server needs confirmation"
        );
        let id = s.save(input("x")).unwrap().id;
        let same = || {
            let mut i = input("renamed");
            i.id = Some(id.clone());
            i.env[1].value = String::new(); // keep stored secret
            i
        };
        assert!(
            !s.launch_changed(&same()),
            "rename / keep-secret is not a launch change"
        );
        let mut i = same();
        i.command = "npx -y @scope/other".into();
        assert!(s.launch_changed(&i));
        let mut i = same();
        i.cwd = Some("/tmp".into());
        assert!(s.launch_changed(&i));
        let mut i = same();
        i.env.push(KeyValueInput {
            name: "NODE_OPTIONS".into(),
            value: "--require /tmp/x.js".into(),
            secret: false,
        });
        assert!(s.launch_changed(&i));
        let mut i = same();
        i.env[0].value = "v2".into();
        assert!(s.launch_changed(&i));
        let mut i = same();
        i.env[1].value = "new-secret".into();
        assert!(s.launch_changed(&i));
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn removed_secret_rows_are_deleted_from_keychain() {
        let (s, tmp) = store();
        let saved = s.save(input("files")).unwrap();
        let mut edit = input("files");
        edit.id = Some(saved.id.clone());
        edit.env.retain(|e| e.name != "TOKEN");
        s.save(edit).unwrap();
        let r = secret_ref(&saved.id, "env", "TOKEN").unwrap();
        assert!(
            s.secrets.get(&r).unwrap().is_none(),
            "stale secret left in keychain"
        );
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn save_rejects_bad_env_names_unknown_ids_userinfo_and_dup_names() {
        let (s, tmp) = store();
        let mut bad_env = input("x");
        bad_env.env[0].name = "BAD-NAME".into();
        assert!(s.save(bad_env).is_err());
        let mut bad_id = input("x");
        bad_id.id = Some("does-not-exist".into());
        assert!(s.save(bad_id).is_err());
        let mut userinfo = input("x");
        userinfo.transport = McpTransport::StreamableHttp;
        userinfo.url = "https://user:pw@example.com/mcp".into();
        assert!(s.save(userinfo).is_err());
        s.save(input("dup")).unwrap();
        assert!(s.save(input("dup")).is_err());
        std::fs::remove_dir_all(tmp).unwrap();
    }

    fn remote(name: &str, auth: McpAuthInput) -> McpServerInput {
        let mut i = input(name);
        i.transport = McpTransport::StreamableHttp;
        i.command = String::new();
        i.env.clear();
        i.url = "http://localhost:8900/mcp".into();
        i.auth = auth;
        i
    }

    fn user_auth(client_id: &str, secret: &str) -> McpAuthInput {
        McpAuthInput::OauthUser {
            client_id: client_id.into(),
            client_secret: secret.into(),
            scopes: "read  write read".into(),
            redirect_port: None,
        }
    }

    #[test]
    fn auth_less_config_loads_unchanged() {
        let (s, tmp) = store();
        let id = s.save(input("x")).unwrap().id;
        // Strip `auth` as a pre-OAuth build would have written the file.
        let path = tmp.join("mcp.json");
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["servers"][0].as_object_mut().unwrap().remove("auth");
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        let root = resolve_data_root(Some(&tmp)).unwrap();
        let reloaded = McpStore::load(&root, Arc::new(MemorySecretStore::default())).unwrap();
        assert_eq!(reloaded.get(&id).unwrap().auth, McpAuth::None);
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn oauth_config_round_trips_with_secret_out_of_band() {
        let (s, tmp) = store();
        let c = s
            .save(remote("r", user_auth("app-1", "cl-secret")))
            .unwrap();
        assert_eq!(
            c.auth,
            McpAuth::OauthUser {
                client_id: Some("app-1".into()),
                has_client_secret: true,
                scopes: vec!["read".into(), "write".into()],
                redirect_port: None,
            }
        );
        let raw = std::fs::read_to_string(tmp.join("mcp.json")).unwrap();
        assert!(
            !raw.contains("cl-secret"),
            "client secret leaked into mcp.json"
        );
        assert_eq!(
            s.oauth_client_secret(&c).unwrap().as_deref(),
            Some("cl-secret")
        );
        let root = resolve_data_root(Some(&tmp)).unwrap();
        let reloaded = McpStore::load(&root, Arc::new(MemorySecretStore::default())).unwrap();
        assert_eq!(reloaded.get(&c.id).unwrap().auth, c.auth);

        // Blank secret with the same client ID keeps it.
        let mut edit = remote("r", user_auth("app-1", ""));
        edit.id = Some(c.id.clone());
        assert!(!s.connection_changed(&edit));
        let c2 = s.save(edit).unwrap();
        assert_eq!(
            s.oauth_client_secret(&c2).unwrap().as_deref(),
            Some("cl-secret")
        );
        // A different client ID drops the old client's secret.
        let mut edit = remote("r", user_auth("app-2", ""));
        edit.id = Some(c.id.clone());
        assert!(s.connection_changed(&edit));
        let c3 = s.save(edit).unwrap();
        assert!(!c3.auth.has_client_secret());
        assert!(s
            .secrets
            .get(&oauth_ref(&c.id, CLIENT_SECRET).unwrap())
            .unwrap()
            .is_none());
        // Delete removes OAuth secrets and tokens.
        let c4 = s
            .save({
                let mut e = remote("r", user_auth("app-2", "s2"));
                e.id = Some(c.id.clone());
                e
            })
            .unwrap();
        super::super::oauth::write_tokens(s.secrets.as_ref(), &c4.id, "{}").unwrap();
        s.delete(&c4.id).unwrap();
        assert!(!s.has_oauth_tokens(&c4.id));
        assert!(s
            .secrets
            .get(&oauth_ref(&c4.id, CLIENT_SECRET).unwrap())
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn tokens_are_cleared_when_the_server_or_client_changes() {
        let (s, tmp) = store();
        let id = s.save(remote("r", user_auth("", ""))).unwrap().id;
        let seed = || super::super::oauth::write_tokens(s.secrets.as_ref(), &id, "{}").unwrap();
        let edit = |auth: McpAuthInput, url: &str| {
            let mut e = remote("r", auth);
            e.id = Some(id.clone());
            e.url = url.into();
            e
        };
        seed();
        // Scope edits keep the sign-in; it applies at the next sign-in.
        let mut same = edit(user_auth("", ""), "http://localhost:8900/mcp");
        if let McpAuthInput::OauthUser { scopes, .. } = &mut same.auth {
            *scopes = "other".into();
        }
        assert!(s.connection_changed(&same));
        s.save(same).unwrap();
        assert!(s.has_oauth_tokens(&id));
        s.save(edit(user_auth("", ""), "http://localhost:8901/mcp"))
            .unwrap();
        assert!(!s.has_oauth_tokens(&id), "URL change clears tokens");
        seed();
        s.save(edit(user_auth("cid", ""), "http://localhost:8901/mcp"))
            .unwrap();
        assert!(!s.has_oauth_tokens(&id), "client ID change clears tokens");
        seed();
        s.save(edit(McpAuthInput::None, "http://localhost:8901/mcp"))
            .unwrap();
        assert!(!s.has_oauth_tokens(&id), "auth kind change clears tokens");
        // A client-credentials token is re-issued for new scopes or a new secret.
        let cc = |secret: &str, scopes: &str| {
            edit(
                McpAuthInput::OauthClientCredentials {
                    client_id: "svc".into(),
                    client_secret: secret.into(),
                    scopes: scopes.into(),
                },
                "http://localhost:8901/mcp",
            )
        };
        s.save(cc("s1", "a")).unwrap();
        seed();
        s.save(cc("", "a")).unwrap();
        assert!(s.has_oauth_tokens(&id), "an unchanged save keeps the token");
        s.save(cc("", "a b")).unwrap();
        assert!(!s.has_oauth_tokens(&id), "scope change clears the token");
        seed();
        s.save(cc("s2", "a b")).unwrap();
        assert!(!s.has_oauth_tokens(&id), "secret change clears the token");
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn auth_validation() {
        let (s, tmp) = store();
        let cc = |id: &str, secret: &str, scopes: &str| McpAuthInput::OauthClientCredentials {
            client_id: id.into(),
            client_secret: secret.into(),
            scopes: scopes.into(),
        };
        assert!(
            s.save(remote("a", cc("", "s", ""))).is_err(),
            "client ID required"
        );
        assert!(
            s.save(remote("a", cc("id", "", ""))).is_err(),
            "secret required"
        );
        assert!(s.save(remote("a", cc("id", "s", "bad\"scope"))).is_err());
        let many = (0..=MAX_SCOPES)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(s.save(remote("a", cc("id", "s", &many))).is_err());
        assert!(s
            .save(remote("a", user_auth("", "secret-without-id")))
            .is_err());
        assert!(s
            .save(remote(
                "a",
                McpAuthInput::OauthUser {
                    client_id: String::new(),
                    client_secret: String::new(),
                    scopes: String::new(),
                    redirect_port: Some(80),
                }
            ))
            .is_err());
        let mut header = remote("a", user_auth("", ""));
        header.headers.push(KeyValueInput {
            name: "authorization".into(),
            value: "Bearer x".into(),
            secret: true,
        });
        assert!(
            s.save(header).is_err(),
            "Authorization header conflicts with OAuth"
        );
        let mut stdio = input("a");
        stdio.auth = user_auth("", "");
        assert!(s.save(stdio).is_err(), "OAuth is remote-only");
        // A stored secret satisfies client credentials on later edits.
        let id = s.save(remote("a", cc("id", "s", "x"))).unwrap().id;
        let mut e = remote("a", cc("id", "", "x"));
        e.id = Some(id);
        assert!(!s.connection_changed(&e));
        s.save(e).unwrap();
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
