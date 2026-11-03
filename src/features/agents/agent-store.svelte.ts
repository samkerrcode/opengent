// User-configured agents and their schedules. Mirrors Rust (SQLite is authoritative): every
// mutation refetches, and Rust pushes `agents-changed` when the scheduler advances schedules
// or an agent's tool permissions change outside the editor.
import { listen } from "@tauri-apps/api/event";
import { commands, describeError, type Agent, type AgentInput, type AgentSchedule, type ScheduleInput } from "$lib/ipc/commands";

class AgentStore {
  agents = $state<Agent[]>([]);
  schedules = $state<AgentSchedule[]>([]);
  loaded = $state(false);
  error = $state<string | null>(null);
  private listening = false;

  byId(id: string | null | undefined): Agent | null {
    return id ? (this.agents.find((a) => a.id === id) ?? null) : null;
  }

  schedulesFor(agentId: string): AgentSchedule[] {
    return this.schedules.filter((s) => s.agent_id === agentId);
  }

  /** Soonest upcoming run across an agent's enabled schedules. */
  nextRun(agentId: string): number | null {
    const times = this.schedulesFor(agentId)
      .filter((s) => s.enabled && s.next_run_at !== null)
      .map((s) => s.next_run_at as number);
    return times.length ? Math.min(...times) : null;
  }

  async init() {
    if (!this.listening) {
      this.listening = true;
      // Schedules advanced, or an agent's tools changed (Tools menu / approval grant).
      await listen("agents-changed", () => void this.load());
    }
    await this.load();
  }

  async load() {
    try {
      [this.agents, this.schedules] = await Promise.all([commands.listAgents(), commands.listSchedules()]);
      this.error = null;
    } catch (e) {
      this.error = describeError(e);
    } finally {
      this.loaded = true;
    }
  }

  async loadSchedules() {
    try {
      this.schedules = await commands.listSchedules();
    } catch (e) {
      this.error = describeError(e);
    }
  }

  async save(input: AgentInput): Promise<Agent> {
    const saved = await commands.saveAgent(input);
    await this.load();
    return saved;
  }

  async remove(id: string) {
    await commands.deleteAgent(id);
    await this.load();
  }

  async saveSchedule(input: ScheduleInput): Promise<AgentSchedule> {
    const saved = await commands.saveSchedule(input);
    await this.loadSchedules();
    return saved;
  }

  async setScheduleEnabled(id: string, enabled: boolean) {
    await commands.setScheduleEnabled(id, enabled);
    await this.loadSchedules();
  }

  async removeSchedule(id: string) {
    await commands.deleteSchedule(id);
    await this.loadSchedules();
  }

  async runNow(id: string) {
    const chat = await commands.runScheduleNow(id);
    await this.loadSchedules();
    return chat;
  }
}

export const agentStore = new AgentStore();
