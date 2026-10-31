// MCP server connections, tool policies and loading settings. Mirrors Rust state; Rust is
// authoritative and pushes `mcp-changed` whenever connection status or catalogs change, so
// this store only needs to refetch.
import { listen } from "@tauri-apps/api/event";
import {
  commands,
  describeError,
  type ApprovalDecision,
  type McpServerInput,
  type McpServerView,
  type ToolLoading,
  type ToolPolicy,
  type ToolSettings,
} from "$lib/ipc/commands";

class McpStore {
  servers = $state<McpServerView[]>([]);
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

  get toolCount(): number {
    return this.servers.reduce((n, s) => n + s.tools.length, 0);
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
  }

  private async run(p: Promise<unknown>) {
    try {
      await p;
      this.error = null;
    } catch (e) {
      this.error = describeError(e);
      throw e;
    } finally {
      await this.load();
    }
  }

  save(input: McpServerInput) {
    return this.run(commands.saveMcpServer(input));
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

  setLoading(loading: ToolLoading) {
    return this.run(commands.setToolSettings({ ...this.settings, loading }));
  }

  approve(runId: string, callId: string, decision: ApprovalDecision) {
    return commands.resolveToolApproval(runId, callId, decision);
  }
}

export const mcpStore = new McpStore();
