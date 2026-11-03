//! MCP client: server configuration (`config`), spawned-stdio environment (`stdio`), the
//! connection manager and tool dispatch (`manager`), plus a one-shot probe used by tests.

pub mod config;
pub mod manager;
pub mod permissions;
pub mod stdio;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{config::product, Error, Result};

pub use config::{
    split_command, KeyValueEntry, KeyValueInput, McpServerConfig, McpServerInput, McpStore,
    McpTransport,
};
pub use manager::{
    McpManager, McpServerStatus, McpServerView, ScopedServerView, ScopedToolView, ToolInfo,
};
pub use permissions::{ChatToolOverrides, PermissionScope, ScopePolicies};

/// Launch configuration for a stdio MCP server. No shell is involved: `command` is executed
/// directly with `args`, a minimal allow-listed environment and an explicit working directory.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StdioServerSpec {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ToolSummary {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ServerProbe {
    pub server_name: String,
    pub server_version: String,
    pub protocol_version: String,
    pub tools: Vec<ToolSummary>,
}

pub(crate) fn client_config() -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new(product().slug, product().version).with_title(product().display_name),
    )
}

/// Spawns the server, performs the MCP handshake, lists tools, and shuts the server down.
pub async fn probe_stdio(spec: &StdioServerSpec, timeout: Duration) -> Result<ServerProbe> {
    let mut cmd = tokio::process::Command::new(&spec.command);
    cmd.args(&spec.args).env_clear().envs(&spec.env);
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    let transport = TokioChildProcess::new(cmd).map_err(|e| Error::Mcp(e.to_string()))?;

    let probe = async {
        let client = client_config()
            .serve(transport)
            .await
            .map_err(|e| Error::Mcp(e.to_string()))?;
        let info = client.peer_info();
        let tools = client
            .list_all_tools()
            .await
            .map_err(|e| Error::Mcp(e.to_string()))?;
        let server_impl = info.as_ref().and_then(|i| i.server_info.as_ref());
        let result = ServerProbe {
            server_name: server_impl.map(|s| s.name.clone()).unwrap_or_default(),
            server_version: server_impl.map(|s| s.version.clone()).unwrap_or_default(),
            protocol_version: info
                .as_ref()
                .map(|i| i.protocol_version.to_string())
                .unwrap_or_default(),
            tools: tools
                .into_iter()
                .map(|t| ToolSummary {
                    name: t.name.to_string(),
                    description: t.description.map(|d| d.to_string()),
                })
                .collect(),
        };
        client
            .cancel()
            .await
            .map_err(|e| Error::Mcp(e.to_string()))?;
        Ok::<_, Error>(result)
    };

    tokio::time::timeout(timeout, probe)
        .await
        .map_err(|_| Error::Mcp(format!("MCP probe timed out after {timeout:?}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set `OPENGENT_TEST_MCP_COMMAND="npx -y @modelcontextprotocol/server-everything"` (or any
    /// stdio MCP server) and run with `--ignored` to exercise a live handshake.
    #[tokio::test]
    #[ignore = "requires an external stdio MCP server"]
    async fn live_stdio_probe() {
        let var = format!("{}_TEST_MCP_COMMAND", product().slug.to_ascii_uppercase());
        let Ok(cmdline) = std::env::var(&var) else {
            eprintln!("{var} not set; skipping");
            return;
        };
        let mut parts = cmdline.split_whitespace().map(str::to_owned);
        let spec = StdioServerSpec {
            command: parts.next().expect("command"),
            args: parts.collect(),
            cwd: None,
            env: HashMap::new(),
        };
        let probe = probe_stdio(&spec, Duration::from_secs(60)).await.unwrap();
        assert!(!probe.server_name.is_empty());
        assert!(!probe.tools.is_empty());
    }
}
