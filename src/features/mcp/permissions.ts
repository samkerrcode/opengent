// Agent tool permissions, edited in the agent editor and an agent chat's ⚒ Tools menu.
// Rust resolves and enforces them (`mcp/permissions.rs`); these helpers only build the
// `AgentServerAccess` the UI saves.
import type { AgentServerAccess, McpServerView, ToolInfo, ToolPolicy } from "$lib/ipc/commands";

/** What an agent starts with for a tool, copied from the connection. Allow carries over
 * only for read-only tools: agents can run unattended, so tools that may change things
 * must be allowed for the agent explicitly. */
export function snapshotPolicy(t: ToolInfo): ToolPolicy {
  if (t.policy === "always_allow") return t.access === "read_only" ? "always_allow" : "ask";
  return t.policy;
}

/** The agent's access for a server, copied from the connection's current permissions.
 * New tools default to Ask. `prev` with a legacy allow-list keeps unlisted tools off. */
export function snapshotAccess(s: McpServerView, prev?: AgentServerAccess | null): AgentServerAccess {
  const allow = prev?.tools ?? null;
  const policies: Record<string, ToolPolicy> = {};
  for (const t of s.tools) policies[t.name] = allow && !allow.includes(t.name) ? "block" : snapshotPolicy(t);
  return { server_id: s.id, tools: null, default_policy: allow ? "block" : "ask", policies };
}

/** Legacy access (no own permissions yet) converted to agent-owned permissions. */
export function ownAccess(a: AgentServerAccess, s: McpServerView | undefined): AgentServerAccess {
  return a.default_policy === null && s ? snapshotAccess(s, a) : a;
}

export function toolPolicy(a: AgentServerAccess, name: string): ToolPolicy {
  if (a.tools && !a.tools.includes(name)) return "block";
  return a.policies[name] ?? a.default_policy ?? "ask";
}

export function toolOn(a: AgentServerAccess, name: string): boolean {
  return toolPolicy(a, name) !== "block";
}

export function withTool(a: AgentServerAccess, t: ToolInfo, policy: ToolPolicy | "on"): AgentServerAccess {
  const p = policy === "on" ? (snapshotPolicy(t) === "block" ? "ask" : snapshotPolicy(t)) : policy;
  return { ...a, policies: { ...a.policies, [t.name]: p } };
}

/** Tools the agent runs without asking, and how many of them aren't read-only. */
export function allowedSummary(a: AgentServerAccess, s: McpServerView): { allowed: number; risky: number } {
  let allowed = 0;
  let risky = 0;
  for (const t of s.tools) {
    if (t.policy === "block" || toolPolicy(a, t.name) !== "always_allow") continue;
    allowed++;
    if (t.access !== "read_only") risky++;
  }
  return { allowed, risky };
}
