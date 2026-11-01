//! User-configured agents: a named persona with its own models (default + fallback),
//! instructions appended to the built-in agent system prompt, and a restricted tool set.
//! Schedules run an agent with a preset prompt (see `schedule`). Persistence lives in
//! `storage::agents`; the run loop itself is `crate::agent`.

pub mod schedule;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agent::tools::ToolFilter;
use crate::domain::AgentId;
use crate::{Error, Result};

pub use schedule::{
    next_after, plan_due, AgentSchedule, DuePlan, MissedRunPolicy, Recurrence, ScheduleInput,
    ScheduleOutcome, MISSED_GRACE_MS,
};

pub const MAX_AGENT_NAME: usize = 64;
pub const MAX_AGENT_DESCRIPTION: usize = 500;
pub const MAX_AGENT_PROMPT: usize = 32 * 1024;
const MAX_SERVERS: usize = 64;
const MAX_TOOLS_PER_SERVER: usize = 500;

/// A provider + model pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelRef {
    pub provider_id: String,
    pub model: String,
}

/// One MCP server the agent may use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentServerAccess {
    pub server_id: String,
    /// Original tool names the agent may call. `None` = every tool on the server,
    /// including tools the server adds later. Global permissions (Ask / Block) still apply.
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub description: String,
    pub default_model: ModelRef,
    pub fallback_model: Option<ModelRef>,
    /// The user's instructions, appended to the built-in agent system prompt.
    pub system_prompt: String,
    /// Servers (and tools) the agent may use; empty = no tools.
    pub servers: Vec<AgentServerAccess>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// Everything but identity/name, stored as one JSON column so the shape can grow without
/// migrations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentConfigJson {
    #[serde(default)]
    pub description: String,
    pub default_model: ModelRef,
    #[serde(default)]
    pub fallback_model: Option<ModelRef>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub servers: Vec<AgentServerAccess>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct AgentInput {
    pub id: Option<AgentId>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub default_model: ModelRef,
    pub fallback_model: Option<ModelRef>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub servers: Vec<AgentServerAccess>,
}

fn clean_model(m: ModelRef, what: &str) -> Result<ModelRef> {
    let (provider_id, model) = (m.provider_id.trim(), m.model.trim());
    if provider_id.is_empty() || model.is_empty() {
        return Err(Error::InvalidInput(format!("choose a {what}")));
    }
    Ok(ModelRef {
        provider_id: provider_id.to_owned(),
        model: model.to_owned(),
    })
}

impl AgentInput {
    /// Trims and bounds every field; returns the normalized name and config.
    pub(crate) fn validate(self) -> Result<(Option<AgentId>, String, AgentConfigJson)> {
        let name = self.name.trim().to_owned();
        if name.is_empty() || name.chars().count() > MAX_AGENT_NAME {
            return Err(Error::InvalidInput(format!(
                "name must be 1–{MAX_AGENT_NAME} characters"
            )));
        }
        let description = self.description.trim().to_owned();
        if description.chars().count() > MAX_AGENT_DESCRIPTION {
            return Err(Error::InvalidInput(format!(
                "description must be at most {MAX_AGENT_DESCRIPTION} characters"
            )));
        }
        let system_prompt = self.system_prompt.trim().to_owned();
        if system_prompt.chars().count() > MAX_AGENT_PROMPT {
            return Err(Error::InvalidInput(format!(
                "system prompt must be at most {MAX_AGENT_PROMPT} characters"
            )));
        }
        let default_model = clean_model(self.default_model, "default model")?;
        let fallback_model = self
            .fallback_model
            .filter(|m| !m.provider_id.trim().is_empty() && !m.model.trim().is_empty())
            .map(|m| clean_model(m, "fallback model"))
            .transpose()?
            .filter(|m| *m != default_model);
        if self.servers.len() > MAX_SERVERS {
            return Err(Error::InvalidInput("too many MCP servers".into()));
        }
        let mut seen = HashSet::new();
        let mut servers = Vec::with_capacity(self.servers.len());
        for s in self.servers {
            let server_id = s.server_id.trim().to_owned();
            if server_id.is_empty() || !seen.insert(server_id.clone()) {
                continue;
            }
            let tools = s.tools.map(|t| {
                let mut uniq = HashSet::new();
                t.into_iter()
                    .map(|n| n.trim().to_owned())
                    .filter(|n| !n.is_empty() && uniq.insert(n.clone()))
                    .collect::<Vec<_>>()
            });
            if tools
                .as_ref()
                .is_some_and(|t| t.len() > MAX_TOOLS_PER_SERVER)
            {
                return Err(Error::InvalidInput("too many tools selected".into()));
            }
            servers.push(AgentServerAccess { server_id, tools });
        }
        Ok((
            self.id,
            name,
            AgentConfigJson {
                description,
                default_model,
                fallback_model,
                system_prompt,
                servers,
            },
        ))
    }
}

impl Agent {
    pub(crate) fn from_parts(
        id: AgentId,
        name: String,
        config: AgentConfigJson,
        created_at: i64,
        updated_at: i64,
    ) -> Self {
        Self {
            id,
            name,
            description: config.description,
            default_model: config.default_model,
            fallback_model: config.fallback_model,
            system_prompt: config.system_prompt,
            servers: config.servers,
            created_at,
            updated_at,
        }
    }

    /// The agent's tool restriction for `McpManager::prepare_tools`.
    pub fn tool_filter(&self) -> ToolFilter {
        let mut f = ToolFilter::default();
        for s in &self.servers {
            f.allow_server(&s.server_id, s.tools.clone());
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> AgentInput {
        AgentInput {
            id: None,
            name: "  Researcher ".into(),
            description: " Finds things ".into(),
            default_model: ModelRef {
                provider_id: "p".into(),
                model: " m1 ".into(),
            },
            fallback_model: Some(ModelRef {
                provider_id: "p".into(),
                model: "m1".into(),
            }),
            system_prompt: "  Be thorough.  ".into(),
            servers: vec![
                AgentServerAccess {
                    server_id: "fs".into(),
                    tools: Some(vec!["read".into(), " read ".into(), "".into()]),
                },
                AgentServerAccess {
                    server_id: "fs".into(),
                    tools: None,
                },
                AgentServerAccess {
                    server_id: "web".into(),
                    tools: None,
                },
            ],
        }
    }

    #[test]
    fn validate_normalizes_and_dedupes() {
        let (_, name, c) = input().validate().unwrap();
        assert_eq!(name, "Researcher");
        assert_eq!(c.description, "Finds things");
        assert_eq!(c.system_prompt, "Be thorough.");
        assert_eq!(c.default_model.model, "m1");
        assert_eq!(
            c.fallback_model, None,
            "fallback equal to default is dropped"
        );
        assert_eq!(c.servers.len(), 2);
        assert_eq!(
            c.servers[0].tools.as_deref(),
            Some(&["read".to_owned()][..])
        );
    }

    #[test]
    fn validate_rejects_missing_model_and_bad_name() {
        let mut i = input();
        i.default_model.model = " ".into();
        assert!(i.validate().is_err());
        let mut i = input();
        i.name = "x".repeat(MAX_AGENT_NAME + 1);
        assert!(i.validate().is_err());
    }

    #[test]
    fn tool_filter_honors_server_and_tool_lists() {
        let (_, name, c) = input().validate().unwrap();
        let agent = Agent::from_parts(AgentId::new(), name, c, 0, 0);
        let f = agent.tool_filter();
        assert!(f.allows_tool("fs", "read"));
        assert!(!f.allows_tool("fs", "write"));
        assert!(f.allows_tool("web", "anything"));
        assert!(!f.allows_server("other"));
    }
}
