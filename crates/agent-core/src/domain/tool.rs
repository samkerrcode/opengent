use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Per-tool permission, enforced in Rust at dispatch time (the UI only edits it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ToolPolicy {
    /// Runs without asking.
    AlwaysAllow,
    /// Pauses the run until the user approves or denies the call.
    #[default]
    Ask,
    /// Never offered to the model; a call that names it anyway is rejected.
    Block,
}

/// The user's answer to an approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalDecision {
    AllowOnce,
    /// Allow now and switch the tool's policy to `AlwaysAllow`.
    AlwaysAllow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ToolCallStatus {
    PendingApproval,
    Running,
    Completed,
    /// The tool ran (or could not be reached) and reported an error.
    Failed,
    Denied,
    Blocked,
    Cancelled,
}

/// Flattens a stored tool result (array of MCP-style parts, or a bare string) into the text
/// sent back to the model. Binary parts become labeled placeholders; unknown part types
/// (internal bookkeeping such as `loaded_tools`) are skipped.
pub fn tool_result_text(content: &serde_json::Value) -> String {
    use serde_json::Value;
    let parts = match content {
        Value::String(s) => return s.clone(),
        Value::Array(parts) => parts,
        other => return other.to_string(),
    };
    let str_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    parts
        .iter()
        .filter_map(|p| match p.get("type").and_then(Value::as_str) {
            Some("text") => str_of(p, "text"),
            Some("image") | Some("audio") => Some(format!(
                "[{} omitted: {}]",
                str_of(p, "type").unwrap_or_default(),
                str_of(p, "mimeType").unwrap_or_else(|| "binary".into())
            )),
            Some("resource") => p.get("resource").map(|r| {
                str_of(r, "text").unwrap_or_else(|| {
                    format!("[resource {}]", str_of(r, "uri").unwrap_or_default())
                })
            }),
            Some("resource_link") => Some(format!(
                "[resource link {}]",
                str_of(p, "uri").unwrap_or_default()
            )),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A tool call as shown to the user: the model-facing name plus where it is routed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ToolCallInfo {
    pub id: String,
    /// Name exposed to the model (e.g. `files__read_file`).
    pub name: String,
    /// Display name of the MCP server; `None` for built-in tools such as `find_tools`.
    pub server: Option<String>,
    /// Original tool name on the server.
    pub tool: String,
    #[ts(type = "unknown")]
    pub arguments: serde_json::Value,
}
