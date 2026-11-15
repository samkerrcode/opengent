// Which top-level view fills the canvas. Chats remain the default; the Agents area has a
// list/intro page and an editor (agentId null = creating a new agent).
export type View = { kind: "chat" } | { kind: "agents" } | { kind: "agent"; agentId: string | null };

class Nav {
  view = $state<View>({ kind: "chat" });
  /** A modal dialog open over the canvas; file drops are ignored while one is up. */
  modal = $state<"settings" | "connections" | null>(null);

  showChat() {
    this.view = { kind: "chat" };
  }

  showAgents() {
    this.view = { kind: "agents" };
  }

  editAgent(agentId: string | null) {
    this.view = { kind: "agent", agentId };
  }
}

export const nav = new Nav();
