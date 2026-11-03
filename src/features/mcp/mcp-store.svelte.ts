// MCP server connections, tool policies and loading settings. Mirrors Rust state; Rust is
// authoritative and pushes `mcp-changed` whenever connection status or catalogs change, so
// this store only needs to refetch.
import { listen } from "@tauri-apps/api/event";
import {
  commands,
  describeError,
  type ApprovalDecision,
  type AgentServerAccess,
  type McpServerInput,
  type McpServerView,
  type PermissionScope,
  type ScopedServerView,
  type ToolLoading,
  type ToolPolicy,
  type ToolSettings,
} from "$lib/ipc/commands";

/** Scopes whose views are kept fresh on `mcp-changed` (most recent last). */
const MAX_TRACKED_SCOPES = 8;

export function scopeKey(scope: PermissionScope): string {
  return `${scope.kind}:${scope.id}`;
}

class McpStore {
  servers = $state<McpServerView[]>([]);
  /** Per-scope (chat/agent) permission views, keyed by `scopeKey`. */
  scoped = $state<Record<string, ScopedServerView[]>>({});
  private tracked = new Map<string, PermissionScope>();
  settings = $state<ToolSettings>({ loading: "auto", auto_threshold_tokens: 6000, search_limit: 5 });
  loaded = $state(false);
  error = $state<string | null>(null);
  private listening = false;

  get connected(): McpServerView[] {
    return this.servers.filter((s) => s.status.kind === "connected");
  }

  get enabledCount(): number {
    return this.servers.filter((s) => s.enabled).length;
  }

  /** Tools on connected servers (`tools` also holds the cached list of offline ones). */
  get toolCount(): number {
    return this.connected.reduce((n, s) => n + s.tools.length, 0);
  }

  /** Tools actually offered to the model: excludes disabled (block-policy) tools and
   * tools on disabled or non-connected servers. */
  get activeToolCount(): number {
    return this.servers.reduce(
      (n, s) =>
        n +
        (s.enabled && s.status.kind === "connected"
          ? s.tools.filter((t) => t.policy !== "block").length
          : 0),
      0,
    );
  }

  async init() {
    if (this.listening) return;
    this.listening = true;
    await listen("mcp-changed", () => void this.load());
    await this.load();
  }

  async load() {
    try {
      [this.servers, this.settings] = await Promise.all([commands.listMcpServers(), commands.getToolSettings()]);
      this.error = null;
    } catch (e) {
      this.error = describeError(e);
    } finally {
      this.loaded = true;
    }
    await Promise.all([...this.tracked.values()].map((s) => this.loadScope(s)));
  }

  scopedFor(scope: PermissionScope): ScopedServerView[] | undefined {
    return this.scoped[scopeKey(scope)];
  }

  /** Fetches (and keeps refreshing) a chat's or agent's permission view. */
  async loadScope(scope: PermissionScope) {
    const key = scopeKey(scope);
    this.tracked.delete(key);
    this.tracked.set(key, scope);
    for (const k of this.tracked.keys()) {
      if (this.tracked.size <= MAX_TRACKED_SCOPES) break;
      this.tracked.delete(k);
      delete this.scoped[k];
    }
    try {
      this.scoped[key] = await commands.listToolPermissions(scope);
    } catch (e) {
      this.error = describeError(e);
    }
  }

  /** Tools a scope's runs are offered: connected, enabled, in scope and not disabled. */
  activeToolCountFor(scope: PermissionScope): number {
    const views = this.scopedFor(scope);
    if (!views) return this.activeToolCount;
    return this.connected
      .filter((s) => s.enabled)
      .reduce((n, s) => {
        const v = views.find((x) => x.server_id === s.id);
        return n + (v?.in_scope ? v.tools.filter((t) => t.in_scope && t.policy !== "block").length : 0);
      }, 0);
  }

  private async scopedRun<T>(scope: PermissionScope, p: Promise<T>): Promise<T> {
    try {
      const v = await p;
      this.error = null;
      return v;
    } catch (e) {
      this.error = describeError(e);
      throw e;
    } finally {
      await this.loadScope(scope);
    }
  }

  setChatServerEnabled(chatId: string, serverId: string, enabled: boolean) {
    return this.scopedRun({ kind: "chat", id: chatId }, commands.setChatServerEnabled(chatId, serverId, enabled));
  }

  setChatToolPolicy(chatId: string, serverId: string, tool: string, policy: ToolPolicy | null) {
    return this.scopedRun({ kind: "chat", id: chatId }, commands.setChatToolPolicy(chatId, serverId, tool, policy));
  }

  resetChatToolPermissions(chatId: string, serverId: string | null) {
    return this.scopedRun({ kind: "chat", id: chatId }, commands.resetChatToolPermissions(chatId, serverId));
  }

  /** Resolves to the updated agent. */
  setAgentServerAccess(agentId: string, serverId: string, access: AgentServerAccess | null) {
    return this.scopedRun({ kind: "agent", id: agentId }, commands.setAgentServerAccess(agentId, serverId, access));
  }

  private async run<T>(p: Promise<T>): Promise<T> {
    try {
      const v = await p;
      this.error = null;
      return v;
    } catch (e) {
      this.error = describeError(e);
      throw e;
    } finally {
      await this.load();
    }
  }

  /** Resolves to the server id. */
  save(input: McpServerInput, connect: boolean) {
    return this.run(commands.saveMcpServer(input, connect));
  }

  remove(id: string) {
    return this.run(commands.deleteMcpServer(id));
  }

  setEnabled(id: string, enabled: boolean) {
    return this.run(commands.setMcpServerEnabled(id, enabled));
  }

  reconnect(id: string) {
    return this.run(commands.reconnectMcpServer(id));
  }

  setToolPolicy(serverId: string, tool: string, policy: ToolPolicy) {
    return this.run(commands.setMcpToolPolicy(serverId, tool, policy));
  }

  setDefaultPolicy(serverId: string, policy: ToolPolicy) {
    return this.run(commands.setMcpDefaultPolicy(serverId, policy));
  }

  resetToolPolicies(serverId: string) {
    return this.run(commands.resetMcpToolPolicies(serverId));
  }

  setLoading(loading: ToolLoading) {
    return this.run(commands.setToolSettings({ ...this.settings, loading }));
  }

  approve(runId: string, callId: string, decision: ApprovalDecision) {
    return commands.resolveToolApproval(runId, callId, decision);
  }
}

export const mcpStore = new McpStore();
