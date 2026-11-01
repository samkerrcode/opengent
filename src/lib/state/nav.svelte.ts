// Which top-level view fills the canvas. Chats remain the default; the Agents area has a
// list/intro page and an editor (agentId null = creating a new agent).
export type View = { kind: "chat" } | { kind: "agents" } | { kind: "agent"; agentId: string | null };

class Nav {
  view = $state<View>({ kind: "chat" });

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
