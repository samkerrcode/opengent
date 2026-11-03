//! Permission scopes. Connection permissions (`McpServerConfig::default_policy` +
//! `tool_policies`) are the global defaults; every run also has a scope that can tighten
//! them or choose between Allow and Ask:
//!
//! - **Chat** — per-chat overrides from the ⚒ Tools menu (`chats.tool_overrides`): a server
//!   can be turned off for the chat without disconnecting it, and tools can be set to
//!   Allow / Ask / Disable for this chat only.
//! - **Agent** — the agent's own server access and per-tool permissions (agent config).
//!
//! Resolution (see `ScopePolicies::resolve`): a disabled connection offers nothing; tools
//! off in the scope are neither offered nor named; a global Block is a hard ceiling no scope
//! can loosen; otherwise the scope's policy applies, falling back to the global one. An
//! `AlwaysAllow` is honored only while the definition signature pinned for the scope that
//! granted it (or the global pin, for an inherited value) still matches.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agents::AgentServerAccess;
use crate::domain::{AgentId, ChatId, ToolPolicy};
use crate::{Error, Result};

/// Bounds on webview-supplied scope data.
pub const MAX_SCOPE_SERVERS: usize = 64;
pub const MAX_SCOPE_TOOLS: usize = 500;
pub const MAX_SCOPE_TOOL_NAME: usize = 128;

/// Which permissions a run uses: its agent's, or (plain chats) the chat's overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
#[ts(export)]
pub enum PermissionScope {
    Chat(ChatId),
    Agent(AgentId),
}

impl PermissionScope {
    /// Stable key for scoped signature pins in `mcp.json`.
    pub fn key(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for PermissionScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Chat(id) => write!(f, "chat:{id}"),
            Self::Agent(id) => write!(f, "agent:{id}"),
        }
    }
}

/// One server's overrides in a chat.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerOverride {
    /// Turned off for this chat (the connection stays up for everything else).
    #[serde(default)]
    pub off: bool,
    /// Per-tool policies for this chat; absent tools follow the connection.
    #[serde(default)]
    pub policies: BTreeMap<String, ToolPolicy>,
}

impl ServerOverride {
    fn is_empty(&self) -> bool {
        !self.off && self.policies.is_empty()
    }
}

/// Per-chat tool overrides, stored as JSON in `chats.tool_overrides`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatToolOverrides {
    #[serde(default)]
    pub servers: BTreeMap<String, ServerOverride>,
}

/// Trims a tool name and checks its bounds.
pub fn clean_tool_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > MAX_SCOPE_TOOL_NAME {
        return Err(Error::InvalidInput(format!("invalid tool name `{n}`")));
    }
    Ok(n.to_owned())
}

impl ChatToolOverrides {
    /// Drops servers that aren't configured and empty entries; bounds everything else.
    pub fn validated(self, known_server: impl Fn(&str) -> bool) -> Result<Self> {
        let mut servers = BTreeMap::new();
        for (id, o) in self.servers {
            if !known_server(&id) || o.is_empty() {
                continue;
            }
            if o.policies.len() > MAX_SCOPE_TOOLS {
                return Err(Error::InvalidInput("too many tool overrides".into()));
            }
            let mut policies = BTreeMap::new();
            for (t, p) in o.policies {
                policies.insert(clean_tool_name(&t)?, p);
            }
            servers.insert(
                id,
                ServerOverride {
                    off: o.off,
                    policies,
                },
            );
        }
        if servers.len() > MAX_SCOPE_SERVERS {
            return Err(Error::InvalidInput("too many servers".into()));
        }
        Ok(Self { servers })
    }

    pub fn set_server_off(&mut self, server_id: &str, off: bool) {
        let o = self.servers.entry(server_id.to_owned()).or_default();
        o.off = off;
        if o.is_empty() {
            self.servers.remove(server_id);
        }
    }

    /// `None` resets the tool to the connection's policy.
    pub fn set_tool(&mut self, server_id: &str, tool: &str, policy: Option<ToolPolicy>) {
        let o = self.servers.entry(server_id.to_owned()).or_default();
        match policy {
            Some(p) => o.policies.insert(tool.to_owned(), p),
            None => o.policies.remove(tool),
        };
        if o.is_empty() {
            self.servers.remove(server_id);
        }
    }

    /// Clears one server's overrides, or all of them.
    pub fn reset(&mut self, server_id: Option<&str>) {
        match server_id {
            Some(id) => {
                self.servers.remove(id);
            }
            None => self.servers.clear(),
        }
    }
}

/// Where an effective `AlwaysAllow` came from, i.e. which signature pin verifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinSource {
    /// Inherited from the connection — verified against the global pin.
    Global,
    /// Set by the scope — verified against the scope's own pin.
    Scope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    /// Stored policy before signature verification.
    pub policy: ToolPolicy,
    pub pin: PinSource,
    /// The scope sets this tool explicitly (vs. inheriting a default).
    pub overridden: bool,
    /// Disabled in Connections, which no scope can loosen.
    pub ceiling: bool,
}

/// A scope's permissions, held by the MCP manager while runs use them.
#[derive(Debug, Clone, PartialEq)]
pub enum ScopePolicies {
    Chat(ChatToolOverrides),
    Agent(Vec<AgentServerAccess>),
    /// The chat or agent no longer exists (deleted mid-run): fail closed.
    Missing,
}

impl ScopePolicies {
    fn agent_access(&self, server_id: &str) -> Option<&AgentServerAccess> {
        match self {
            Self::Agent(servers) => servers.iter().find(|s| s.server_id == server_id),
            _ => None,
        }
    }

    /// Whether the scope uses this server at all.
    pub fn allows_server(&self, server_id: &str) -> bool {
        match self {
            Self::Chat(o) => !o.servers.get(server_id).is_some_and(|s| s.off),
            Self::Agent(_) => self.agent_access(server_id).is_some(),
            Self::Missing => true,
        }
    }

    /// Whether the tool is in scope (server used and, for legacy agent allow-lists, listed).
    pub fn allows_tool(&self, server_id: &str, tool: &str) -> bool {
        if !self.allows_server(server_id) {
            return false;
        }
        match self.agent_access(server_id) {
            Some(a) => a.tools.as_ref().is_none_or(|t| t.iter().any(|n| n == tool)),
            None => true,
        }
    }

    /// The tool's policy in this scope, ignoring whether it is in scope. `global` is the
    /// connection's stored policy (`tool_policies[t] ?? default_policy`).
    pub fn policy_for(&self, server_id: &str, tool: &str, global: ToolPolicy) -> Resolved {
        let inherit = Resolved {
            policy: global,
            pin: PinSource::Global,
            overridden: false,
            ceiling: false,
        };
        if global == ToolPolicy::Block {
            return Resolved {
                ceiling: true,
                ..inherit
            };
        }
        let scoped = |policy, overridden| Resolved {
            policy,
            pin: PinSource::Scope,
            overridden,
            ceiling: false,
        };
        match self {
            Self::Chat(o) => match o.servers.get(server_id).and_then(|s| s.policies.get(tool)) {
                Some(&p) => scoped(p, true),
                None => inherit,
            },
            Self::Agent(_) => match self.agent_access(server_id) {
                Some(a) => match (a.policies.get(tool), a.default_policy) {
                    (Some(&p), _) => scoped(p, true),
                    (None, Some(p)) => scoped(p, false),
                    // Legacy agent (no own permissions yet): follows the connection.
                    (None, None) => inherit,
                },
                None => inherit,
            },
            Self::Missing => Resolved {
                policy: ToolPolicy::Ask,
                ..inherit
            },
        }
    }

    /// `None` when the tool is out of scope (neither offered nor named to the model).
    pub fn resolve(&self, server_id: &str, tool: &str, global: ToolPolicy) -> Option<Resolved> {
        self.allows_tool(server_id, tool)
            .then(|| self.policy_for(server_id, tool, global))
    }

    /// Records an approval-card grant in the scope. Returns false if the scope can't hold it.
    pub(crate) fn grant(&mut self, server_id: &str, tool: &str, policy: ToolPolicy) -> bool {
        match self {
            Self::Chat(o) => {
                o.set_tool(server_id, tool, Some(policy));
                true
            }
            Self::Agent(servers) => match servers.iter_mut().find(|s| s.server_id == server_id) {
                Some(a) => {
                    a.policies.insert(tool.to_owned(), policy);
                    true
                }
                None => false,
            },
            Self::Missing => false,
        }
    }

    /// Tools this scope itself sets to `AlwaysAllow`: explicit entries plus, for an agent
    /// whose own default is Allow, the `known` tools (`server → names`) it covers. These
    /// are the grants that need a scoped signature pin.
    pub fn scoped_allows(&self, known: impl Fn(&str) -> Vec<String>) -> Vec<(String, String)> {
        let mut out = Vec::new();
        match self {
            Self::Chat(o) => {
                for (server, s) in &o.servers {
                    for (tool, p) in &s.policies {
                        if *p == ToolPolicy::AlwaysAllow {
                            out.push((server.clone(), tool.clone()));
                        }
                    }
                }
            }
            Self::Agent(servers) => {
                for a in servers {
                    let mut seen = HashSet::new();
                    for (tool, p) in &a.policies {
                        seen.insert(tool.clone());
                        if *p == ToolPolicy::AlwaysAllow {
                            out.push((a.server_id.clone(), tool.clone()));
                        }
                    }
                    if a.default_policy == Some(ToolPolicy::AlwaysAllow) {
                        for tool in known(&a.server_id) {
                            if !seen.contains(&tool) {
                                out.push((a.server_id.clone(), tool));
                            }
                        }
                    }
                }
            }
            Self::Missing => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ToolPolicy::*;

    fn agent(access: Vec<AgentServerAccess>) -> ScopePolicies {
        ScopePolicies::Agent(access)
    }

    fn access(server: &str) -> AgentServerAccess {
        AgentServerAccess {
            server_id: server.into(),
            tools: None,
            default_policy: Some(Ask),
            policies: BTreeMap::new(),
        }
    }

    #[test]
    fn chat_overrides_tighten_or_choose_but_never_loosen_block() {
        let mut o = ChatToolOverrides::default();
        o.set_tool("fs", "read", Some(AlwaysAllow));
        o.set_tool("fs", "rm", Some(AlwaysAllow));
        let p = ScopePolicies::Chat(o.clone());
        let r = p.resolve("fs", "read", Ask).unwrap();
        assert_eq!(
            (r.policy, r.pin, r.overridden),
            (AlwaysAllow, PinSource::Scope, true)
        );
        // Inherited value is verified against the global pin.
        let r = p.resolve("fs", "write", AlwaysAllow).unwrap();
        assert_eq!((r.policy, r.pin), (AlwaysAllow, PinSource::Global));
        // Global Block is a ceiling.
        let r = p.resolve("fs", "rm", Block).unwrap();
        assert!(r.ceiling && r.policy == Block);
        // Off in the chat: out of scope.
        o.set_server_off("fs", true);
        let p = ScopePolicies::Chat(o);
        assert!(p.resolve("fs", "read", Ask).is_none());
        assert!(p.allows_server("web"));
    }

    #[test]
    fn agent_policies_fall_back_to_own_default_then_global_for_legacy() {
        let mut a = access("fs");
        a.policies.insert("read".into(), AlwaysAllow);
        a.policies.insert("rm".into(), Block);
        let p = agent(vec![a]);
        assert_eq!(p.resolve("fs", "read", Ask).unwrap().policy, AlwaysAllow);
        assert_eq!(p.resolve("fs", "rm", AlwaysAllow).unwrap().policy, Block);
        // New tool: the agent's own default, not the connection's Allow.
        let r = p.resolve("fs", "new", AlwaysAllow).unwrap();
        assert_eq!(
            (r.policy, r.pin, r.overridden),
            (Ask, PinSource::Scope, false)
        );
        assert!(p.resolve("web", "x", Ask).is_none(), "server not included");

        let mut legacy = access("fs");
        legacy.default_policy = None;
        legacy.tools = Some(vec!["read".into()]);
        let p = agent(vec![legacy]);
        let r = p.resolve("fs", "read", AlwaysAllow).unwrap();
        assert_eq!((r.policy, r.pin), (AlwaysAllow, PinSource::Global));
        assert!(p.resolve("fs", "write", Ask).is_none(), "legacy allow-list");
    }

    #[test]
    fn missing_scope_fails_closed() {
        let p = ScopePolicies::Missing;
        assert_eq!(p.resolve("fs", "t", AlwaysAllow).unwrap().policy, Ask);
        assert_eq!(p.resolve("fs", "t", Block).unwrap().policy, Block);
        let mut p = ScopePolicies::Missing;
        assert!(!p.grant("fs", "t", AlwaysAllow));
    }

    #[test]
    fn scoped_allows_lists_explicit_and_default_covered_tools() {
        let mut a = access("fs");
        a.default_policy = Some(AlwaysAllow);
        a.policies.insert("rm".into(), Ask);
        a.policies.insert("read".into(), AlwaysAllow);
        let p = agent(vec![a]);
        let mut got = p.scoped_allows(|_| vec!["read".into(), "rm".into(), "ls".into()]);
        got.sort();
        assert_eq!(
            got,
            vec![("fs".into(), "ls".into()), ("fs".into(), "read".into())]
        );
    }

    #[test]
    fn overrides_validation_drops_unknown_and_bounds_names() {
        let mut o = ChatToolOverrides::default();
        o.set_tool("fs", " read ", Some(Ask));
        o.set_tool("gone", "x", Some(Ask));
        o.set_tool("fs", "tmp", None);
        let v = o.clone().validated(|s| s == "fs").unwrap();
        assert_eq!(v.servers.len(), 1);
        assert!(v.servers["fs"].policies.contains_key("read"));
        o.set_tool("fs", &"n".repeat(MAX_SCOPE_TOOL_NAME + 1), Some(Ask));
        assert!(o.validated(|_| true).is_err());
        let mut r = ChatToolOverrides::default();
        r.set_server_off("fs", true);
        r.set_server_off("fs", false);
        assert!(r.servers.is_empty(), "empty entries are dropped");
    }
}
