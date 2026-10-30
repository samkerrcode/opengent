// In-memory chat state for Phase 0. Phase 1 replaces this with SQLite-backed commands;
// the shape is kept close to the canonical domain model to make that swap mechanical.
import { commands, describeError } from "$lib/ipc/commands";
import { applyRunEvent, emptyTurn, isTerminal, type AssistantTurn } from "$lib/state/run-reducer";

export interface UserTurn {
  kind: "user";
  id: string;
  text: string;
}
export interface AssistantTurnItem {
  kind: "assistant";
  id: string;
  turn: AssistantTurn;
}
export type TurnItem = UserTurn | AssistantTurnItem;

export interface Chat {
  id: string;
  title: string;
  createdAt: number;
  items: TurnItem[];
  activeRunId: string | null;
}

function uuid(): string {
  return crypto.randomUUID();
}

class ChatStore {
  chats = $state<Chat[]>([]);
  activeId = $state<string | null>(null);

  get active(): Chat | null {
    return this.chats.find((c) => c.id === this.activeId) ?? null;
  }

  newChat(): Chat {
    const chat: Chat = { id: uuid(), title: "New chat", createdAt: Date.now(), items: [], activeRunId: null };
    this.chats.unshift(chat);
    this.activeId = chat.id;
    return chat;
  }

  select(id: string) {
    this.activeId = id;
  }

  async send(chat: Chat, prompt: string) {
    const text = prompt.trim();
    if (!text || chat.activeRunId) return;

    chat.items.push({ kind: "user", id: uuid(), text });
    if (chat.items.length === 1) chat.title = text.length > 48 ? `${text.slice(0, 48)}…` : text;

    const assistant: AssistantTurnItem = { kind: "assistant", id: uuid(), turn: emptyTurn() };
    chat.items.push(assistant);

    try {
      const runId = await commands.startRun(chat.id, text, (event) => {
        assistant.turn = applyRunEvent(assistant.turn, event);
        if (isTerminal(assistant.turn.phase)) chat.activeRunId = null;
      });
      // Events may already have arrived before the invoke resolved; don't clobber a finished run.
      if (!isTerminal(assistant.turn.phase)) chat.activeRunId = runId;
    } catch (e) {
      assistant.turn = { ...assistant.turn, phase: "failed", error: describeError(e) };
      chat.activeRunId = null;
    }
  }

  async stop(chat: Chat) {
    if (chat.activeRunId) await commands.cancelRun(chat.activeRunId);
  }
}

export const chatStore = new ChatStore();
