//! Tools as seen by a run: the catalog of callable tools (already filtered by policy), how
//! their definitions reach the model, and the `find_tools` meta-tool used in on-demand mode.
//!
//! The agent loop only knows the `ToolHost` trait; the MCP connection manager implements it.
//! Keeping the seam here lets the loop be tested without spawning servers.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use super::context::estimate_tokens;
use crate::domain::{ContentBlock, Message, ToolPolicy};
use crate::providers::ToolSpec;

/// Name of the built-in search tool offered in on-demand mode.
pub const FIND_TOOLS: &str = "find_tools";
/// Cap on tool result text kept in history and sent back to the model.
pub const MAX_TOOL_RESULT_CHARS: usize = 40_000;
/// On-demand mode keeps at most this many loaded tools (most recently used win).
const MAX_LOADED_TOOLS: usize = 32;
/// Model-facing function names must match `^[a-zA-Z0-9_-]{1,64}$` (OpenAI and most others).
const MAX_TOOL_NAME: usize = 64;

/// How tool definitions are sent to the model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ToolLoading {
    /// All tools while they are small; on-demand once they exceed the threshold.
    #[default]
    Auto,
    /// Every enabled tool's full definition in every request.
    All,
    /// Only `find_tools` plus tools the model has loaded through it.
    OnDemand,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ToolSettings {
    pub loading: ToolLoading,
    /// In `auto`, switch to on-demand when all tool definitions together exceed this many
    /// (estimated) tokens, or a quarter of the model's context window.
    pub auto_threshold_tokens: u32,
    /// Maximum number of tools one `find_tools` search returns and loads.
    pub search_limit: u32,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            loading: ToolLoading::Auto,
            auto_threshold_tokens: 6_000,
            search_limit: 5,
        }
    }
}

/// One callable tool from a connected server. Blocked tools never become entries.
#[derive(Debug, Clone)]
pub struct ToolEntry {
    /// Model-facing definition; `spec.name` is the exposed (namespaced) name.
    pub spec: ToolSpec,
    pub server_id: String,
    pub server_name: String,
    /// Original tool name on the server.
    pub tool: String,
    /// Estimated prompt cost of sending this definition, computed once at catalog build.
    pub def_tokens: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    /// Array of MCP-style content parts.
    pub content: Value,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>, is_error: bool) -> Self {
        Self {
            content: json!([{ "type": "text", "text": text.into() }]),
            is_error,
        }
    }
}

pub type ToolFuture = Pin<Box<dyn Future<Output = ToolOutput> + Send>>;

/// Executes tools and owns their permissions. Policies are read at dispatch time so a change
/// made mid-run (e.g. "Always allow" on an earlier call) applies to later calls.
pub trait ToolHost: Send + Sync + 'static {
    fn policy(&self, server_id: &str, tool: &str) -> ToolPolicy;
    fn set_policy(&self, server_id: &str, tool: &str, policy: ToolPolicy);
    fn call(
        &self,
        server_id: &str,
        tool: &str,
        arguments: Value,
        cancel: CancellationToken,
    ) -> ToolFuture;
}

/// Estimated prompt cost of sending these definitions.
pub fn definitions_tokens<'a>(specs: impl IntoIterator<Item = &'a ToolSpec>) -> u32 {
    specs
        .into_iter()
        .map(|s| {
            estimate_tokens(&s.name)
                + estimate_tokens(&s.description)
                + estimate_tokens(&s.parameters.to_string())
                + 8
        })
        .sum()
}

/// Decides whether a run uses on-demand loading.
pub fn resolve_on_demand(
    settings: &ToolSettings,
    catalog: &[ToolEntry],
    context_window: u32,
) -> bool {
    match settings.loading {
        ToolLoading::All => false,
        ToolLoading::OnDemand => true,
        ToolLoading::Auto => {
            let cost: u32 = catalog.iter().map(|e| e.def_tokens).sum();
            cost > settings.auto_threshold_tokens || cost > context_window / 4
        }
    }
}

fn sanitize(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max));
    let mut last_us = false;
    for c in s.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '-' {
            c
        } else {
            '_'
        };
        if c == '_' && last_us {
            continue;
        }
        last_us = c == '_';
        out.push(c);
    }
    let out: String = out.trim_matches('_').chars().take(max).collect();
    if out.is_empty() {
        "tool".into()
    } else {
        out
    }
}

/// Assigns stable, unique, provider-safe names of the form `server__tool`. Feed tools in a
/// deterministic order (config order, then tool name) so names survive across turns — the
/// on-demand loaded set is recovered from history by name.
pub struct ToolNamer {
    used: HashSet<String>,
}

impl Default for ToolNamer {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolNamer {
    pub fn new() -> Self {
        let mut used = HashSet::new();
        used.insert(FIND_TOOLS.to_owned());
        Self { used }
    }

    pub fn name(&mut self, server: &str, tool: &str) -> String {
        let prefix = sanitize(&server.to_ascii_lowercase(), 24);
        let base: String = format!("{prefix}__{}", sanitize(tool, MAX_TOOL_NAME))
            .chars()
            .take(MAX_TOOL_NAME)
            .collect();
        let mut candidate = base.clone();
        let mut n = 2;
        while self.used.contains(&candidate) {
            let suffix = format!("_{n}");
            candidate = format!(
                "{}{suffix}",
                &base[..base.len().min(MAX_TOOL_NAME - suffix.len())]
            );
            n += 1;
        }
        self.used.insert(candidate.clone());
        candidate
    }
}

/// Splits on non-alphanumerics and camelCase boundaries, lowercased.
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if c.is_uppercase() && prev_lower && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = c.is_lowercase() || c.is_numeric();
            cur.extend(c.to_lowercase());
        } else {
            prev_lower = false;
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Ranks catalog entries for a keyword query (TF-IDF-style over name and description, with
/// name matches weighted higher and prefix matches counted partially). Returns indices.
pub fn search(catalog: &[ToolEntry], query: &str, limit: usize) -> Vec<usize> {
    let terms: Vec<String> = {
        let mut seen = HashSet::new();
        tokenize(query)
            .into_iter()
            .filter(|t| seen.insert(t.clone()))
            .collect()
    };
    if terms.is_empty() || catalog.is_empty() {
        return Vec::new();
    }
    let docs: Vec<(Vec<String>, Vec<String>)> = catalog
        .iter()
        .map(|e| {
            let mut name = tokenize(&e.tool);
            name.extend(tokenize(&e.server_name));
            (name, tokenize(&e.spec.description))
        })
        .collect();
    let matches = |words: &[String], term: &str| -> f32 {
        if words.iter().any(|w| w == term) {
            1.0
        } else if term.len() >= 3 && words.iter().any(|w| w.starts_with(term)) {
            0.5
        } else {
            0.0
        }
    };
    let n = catalog.len() as f32;
    let idf: HashMap<&str, f32> = terms
        .iter()
        .map(|t| {
            let df = docs
                .iter()
                .filter(|(a, b)| matches(a, t) > 0.0 || matches(b, t) > 0.0)
                .count() as f32;
            (t.as_str(), (1.0 + n / (1.0 + df)).ln())
        })
        .collect();
    let mut scored: Vec<(usize, f32)> = docs
        .iter()
        .enumerate()
        .map(|(i, (name, desc))| {
            let s = terms
                .iter()
                .map(|t| idf[t.as_str()] * (3.0 * matches(name, t) + matches(desc, t)))
                .sum::<f32>();
            (i, s)
        })
        .filter(|(_, s)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().take(limit).map(|(i, _)| i).collect()
}

fn find_tools_spec(search_limit: usize) -> ToolSpec {
    ToolSpec {
        name: FIND_TOOLS.into(),
        description: "Search the tools available from the user's connected MCP servers and load \
            the best matches so you can call them. Use a few keywords describing the capability \
            you need (for example \"read file\" or \"create github issue\"). Loaded tools stay \
            available for the rest of the conversation; search again if none fit."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Keywords describing the needed capability." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 20, "description": format!("Maximum tools to load (default {search_limit}).") }
            },
            "required": ["query"]
        }),
    }
}

/// Caps a tool result for storage and for the model: long text is truncated with a note and
/// binary payloads (images, audio, blob resources) are reduced to a labeled stub.
pub fn cap_result(content: Value) -> Value {
    cap_result_with(content, MAX_TOOL_RESULT_CHARS)
}

/// `cap_result` with an explicit character budget shared across all parts.
pub fn cap_result_with(content: Value, max_chars: usize) -> Value {
    let Value::Array(parts) = content else {
        return content;
    };
    let mut budget = max_chars;
    let mut out = Vec::with_capacity(parts.len());
    for mut p in parts {
        match p.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = p.get("text").and_then(Value::as_str).unwrap_or("");
                let len = text.chars().count();
                if len > budget {
                    let kept: String = text.chars().take(budget).collect();
                    p["text"] = Value::String(format!(
                        "{kept}\n[… truncated: showing {budget} of {len} characters]"
                    ));
                    budget = 0;
                } else {
                    budget -= len;
                }
            }
            Some("image") | Some("audio") => {
                if let Some(Value::String(data)) = p.get("data") {
                    let bytes = data.len() / 4 * 3;
                    p["data"] = Value::Null;
                    p["omitted_bytes"] = json!(bytes);
                }
            }
            Some("resource") => {
                if let Some(r) = p.get_mut("resource") {
                    if let Some(text) = r.get("text").and_then(Value::as_str) {
                        let len = text.chars().count();
                        if len > budget {
                            let kept: String = text.chars().take(budget).collect();
                            r["text"] = Value::String(format!(
                                "{kept}\n[… truncated: showing {budget} of {len} characters]"
                            ));
                            budget = 0;
                        } else {
                            budget -= len;
                        }
                    }
                    if r.get("blob").is_some() {
                        r["blob"] = Value::Null;
                    }
                }
            }
            _ => {
                // Unknown/vendor part types share the budget by serialized size; anything
                // over it becomes a stub rather than inflating the transcript.
                let len = p.to_string().chars().count();
                if len > budget {
                    p = json!({ "type": "text", "text": "[tool output part omitted]" });
                    budget = 0;
                } else {
                    budget -= len;
                }
            }
        }
        out.push(p);
    }
    Value::Array(out)
}

/// The tool configuration for one run.
pub struct RunTools {
    pub host: Arc<dyn ToolHost>,
    catalog: Vec<ToolEntry>,
    on_demand: bool,
    search_limit: usize,
    /// Per-result character budget: `MAX_TOOL_RESULT_CHARS` capped further for small
    /// context windows so one result can't swamp the model.
    pub result_cap: usize,
    /// On-demand mode: exposed names currently loaded, oldest first.
    loaded: Vec<String>,
    /// Exposed names of user-disabled tools: never offered, but recognized at dispatch so
    /// the model is told "disabled" rather than "unknown" / "call find_tools".
    disabled: HashSet<String>,
}

impl RunTools {
    /// `history` lets on-demand mode restore tools loaded or used in earlier turns.
    /// `context_window` sizes `result_cap` (~a quarter of the window).
    pub fn new(
        host: Arc<dyn ToolHost>,
        catalog: Vec<ToolEntry>,
        on_demand: bool,
        search_limit: usize,
        history: &[Message],
        context_window: u32,
    ) -> Self {
        let mut tools = Self {
            host,
            catalog,
            on_demand,
            search_limit: search_limit.clamp(1, 20),
            // ~3 chars/token over a quarter of the window, floored so a bogus tiny window
            // can't reduce every result to nothing.
            result_cap: MAX_TOOL_RESULT_CHARS.min((context_window as usize / 4 * 3).max(2_000)),
            loaded: Vec::new(),
            disabled: HashSet::new(),
        };
        if on_demand {
            for b in history.iter().flat_map(|m| &m.blocks) {
                match b {
                    ContentBlock::ToolCall { name, .. } => tools.touch(name),
                    ContentBlock::ToolResult { content, .. } => {
                        for name in loaded_names(content) {
                            tools.touch(&name);
                        }
                    }
                    _ => {}
                }
            }
        }
        tools
    }

    /// Registers the exposed names of user-disabled tools (see `is_disabled`).
    pub fn with_disabled(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.disabled.extend(names);
        self
    }

    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled.contains(name)
    }

    pub fn is_on_demand(&self) -> bool {
        self.on_demand
    }

    pub fn catalog(&self) -> &[ToolEntry] {
        &self.catalog
    }

    fn touch(&mut self, name: &str) {
        if !self.catalog.iter().any(|e| e.spec.name == name) {
            return;
        }
        self.loaded.retain(|n| n != name);
        self.loaded.push(name.to_owned());
        if self.loaded.len() > MAX_LOADED_TOOLS {
            self.loaded.remove(0);
        }
    }

    /// Definitions to send with the next request.
    pub fn specs(&self) -> Vec<ToolSpec> {
        if !self.on_demand {
            return self.catalog.iter().map(|e| e.spec.clone()).collect();
        }
        let mut specs = vec![find_tools_spec(self.search_limit)];
        specs.extend(
            self.catalog
                .iter()
                .filter(|e| self.loaded.contains(&e.spec.name))
                .map(|e| e.spec.clone()),
        );
        specs
    }

    /// Estimated prompt cost of the definitions `specs()` will send (cached per entry, so
    /// this doesn't re-serialize schemas at run start).
    pub fn definition_tokens(&self) -> u32 {
        if !self.on_demand {
            return self.catalog.iter().map(|e| e.def_tokens).sum();
        }
        let find = find_tools_spec(self.search_limit);
        let mut tokens = definitions_tokens([&find]);
        tokens += self
            .catalog
            .iter()
            .filter(|e| self.loaded.contains(&e.spec.name))
            .map(|e| e.def_tokens)
            .sum::<u32>();
        tokens
    }

    /// Looks up a callable tool. In on-demand mode only loaded tools resolve, so the model
    /// cannot call a tool whose definition it has not seen.
    pub fn entry(&self, name: &str) -> Option<&ToolEntry> {
        self.catalog
            .iter()
            .find(|e| e.spec.name == name)
            .filter(|e| !self.on_demand || self.loaded.contains(&e.spec.name))
    }

    /// Runs `find_tools`: ranks the catalog, loads the matches, and describes them.
    pub fn find_tools(&mut self, arguments: &Value) -> ToolOutput {
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            return ToolOutput::text("`query` is required.", true);
        }
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .map(|l| (l as usize).clamp(1, 20))
            .unwrap_or(self.search_limit);
        let hits = search(&self.catalog, query, limit);
        if hits.is_empty() {
            let mut servers: Vec<(&str, usize)> = Vec::new();
            for e in &self.catalog {
                match servers.iter_mut().find(|(s, _)| *s == e.server_name) {
                    Some((_, n)) => *n += 1,
                    None => servers.push((&e.server_name, 1)),
                }
            }
            let list = servers
                .iter()
                .map(|(s, n)| format!("{s} ({n} tools)"))
                .collect::<Vec<_>>()
                .join(", ");
            return ToolOutput::text(
                format!(
                    "No tools matched \"{query}\". Connected servers: {list}. Try other keywords."
                ),
                false,
            );
        }
        let names: Vec<String> = hits
            .iter()
            .map(|&i| self.catalog[i].spec.name.clone())
            .collect();
        let lines = hits
            .iter()
            .map(|&i| {
                let e = &self.catalog[i];
                let desc = e.spec.description.lines().next().unwrap_or("").trim();
                let desc: String = desc.chars().take(200).collect();
                format!("- {} ({}): {desc}", e.spec.name, e.server_name)
            })
            .collect::<Vec<_>>()
            .join("\n");
        for n in &names {
            self.touch(n);
        }
        ToolOutput {
            content: json!([
                { "type": "text", "text": format!("Loaded {} tool(s); you can call them now:\n{lines}", names.len()) },
                { "type": "loaded_tools", "names": names },
            ]),
            is_error: false,
        }
    }
}

fn loaded_names(content: &Value) -> Vec<String> {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("loaded_tools"))
        .flat_map(|p| {
            p.get("names")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::domain::Role;

    pub fn entry(server: &str, tool: &str, desc: &str, namer: &mut ToolNamer) -> ToolEntry {
        let spec = ToolSpec {
            name: namer.name(server, tool),
            description: desc.into(),
            parameters: json!({ "type": "object" }),
        };
        let def_tokens = definitions_tokens([&spec]);
        ToolEntry {
            spec,
            server_id: format!("id-{server}"),
            server_name: server.into(),
            tool: tool.into(),
            def_tokens,
        }
    }

    pub struct NoopHost;
    impl ToolHost for NoopHost {
        fn policy(&self, _: &str, _: &str) -> ToolPolicy {
            ToolPolicy::AlwaysAllow
        }
        fn set_policy(&self, _: &str, _: &str, _: ToolPolicy) {}
        fn call(&self, _: &str, _: &str, _: Value, _: CancellationToken) -> ToolFuture {
            Box::pin(async { ToolOutput::text("ok", false) })
        }
    }

    #[test]
    fn names_are_sanitized_namespaced_and_unique() {
        let mut n = ToolNamer::new();
        assert_eq!(n.name("My Files!", "read.file"), "my_files__read_file");
        assert_eq!(n.name("My Files!", "read.file"), "my_files__read_file_2");
        let long = n.name("server", &"x".repeat(100));
        assert_eq!(long.len(), 64);
        assert!(long
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        assert_eq!(n.name("", "find_tools"), "tool__find_tools");
    }

    #[test]
    fn tokenize_splits_camel_and_snake() {
        assert_eq!(
            tokenize("readFile list_dir GitHub"),
            vec!["read", "file", "list", "dir", "git", "hub"]
        );
    }

    #[test]
    fn search_prefers_name_matches() {
        let mut n = ToolNamer::new();
        let catalog = vec![
            entry("fs", "write_file", "Write text to a file on disk", &mut n),
            entry("fs", "read_file", "Read a file from disk", &mut n),
            entry(
                "github",
                "create_issue",
                "Open an issue in a repository",
                &mut n,
            ),
            entry("time", "now", "Current time; can read the clock", &mut n),
        ];
        let hits = search(&catalog, "read file", 2);
        assert_eq!(catalog[hits[0]].tool, "read_file");
        assert_eq!(
            catalog[search(&catalog, "github issue", 5)[0]].tool,
            "create_issue"
        );
        assert!(search(&catalog, "zzz", 5).is_empty());
        assert!(search(&catalog, "  ", 5).is_empty());
    }

    #[test]
    fn on_demand_loads_through_search_and_restores_from_history() {
        let mut n = ToolNamer::new();
        let catalog = vec![
            entry("fs", "read_file", "Read a file", &mut n),
            entry("fs", "write_file", "Write a file", &mut n),
        ];
        let mut tools = RunTools::new(Arc::new(NoopHost), catalog.clone(), true, 5, &[], 8192);
        assert_eq!(tools.specs().len(), 1, "only find_tools before any search");
        assert!(tools.entry("fs__read_file").is_none());
        let out = tools.find_tools(&json!({ "query": "read" }));
        assert!(!out.is_error);
        assert!(tools.entry("fs__read_file").is_some());
        assert!(tools.entry("fs__write_file").is_none());
        assert_eq!(tools.specs().len(), 2);

        // A later turn recovers the loaded set from the stored result.
        let history = vec![Message {
            role: Role::Assistant,
            blocks: vec![ContentBlock::ToolResult {
                call_id: "c".into(),
                content: out.content,
                is_error: false,
                status: None,
            }],
        }];
        let restored = RunTools::new(Arc::new(NoopHost), catalog.clone(), true, 5, &history, 8192);
        assert!(restored.entry("fs__read_file").is_some());

        let all = RunTools::new(Arc::new(NoopHost), catalog, false, 5, &[], 8192);
        assert_eq!(all.specs().len(), 2);
        assert!(all.entry("fs__write_file").is_some());
    }

    #[test]
    fn auto_mode_switches_on_size() {
        let mut n = ToolNamer::new();
        let catalog: Vec<_> = (0..200)
            .map(|i| entry("s", &format!("t{i}"), &"describe ".repeat(20), &mut n))
            .collect();
        let s = ToolSettings::default();
        assert!(resolve_on_demand(&s, &catalog, 128_000));
        assert!(!resolve_on_demand(&s, &catalog[..3], 128_000));
        assert!(
            resolve_on_demand(&s, &catalog[..20], 4_000),
            "small window forces on-demand"
        );
        let all = ToolSettings {
            loading: ToolLoading::All,
            ..s
        };
        assert!(!resolve_on_demand(&all, &catalog, 128_000));
    }

    #[test]
    fn cap_result_bounds_resource_text_and_unknown_parts() {
        let capped = cap_result_with(
            json!([
                { "type": "resource", "resource": { "uri": "file:///x", "text": "z".repeat(100) } },
                { "type": "vendor_big", "blob": "y".repeat(100) },
            ]),
            50,
        );
        let parts = capped.as_array().unwrap();
        assert!(parts[0]["resource"]["text"]
            .as_str()
            .unwrap()
            .contains("truncated"));
        assert!(parts[1]["text"].as_str().unwrap().contains("omitted"));
    }

    #[test]
    fn cap_result_truncates_text_and_strips_binary() {
        let big = "a".repeat(MAX_TOOL_RESULT_CHARS + 5);
        let capped = cap_result(json!([
            { "type": "text", "text": big },
            { "type": "image", "data": "aGVsbG8=", "mimeType": "image/png" }
        ]));
        let text = capped[0]["text"].as_str().unwrap();
        assert!(text.contains("truncated"));
        assert!(capped[1]["data"].is_null());
        assert_eq!(capped[1]["omitted_bytes"], 6);
    }
}
