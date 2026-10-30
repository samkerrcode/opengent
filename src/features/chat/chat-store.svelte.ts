// In-memory chat state (Phase 1 replaces this with SQLite-backed commands; the shape mirrors the
// canonical domain model so that swap is mechanical).
import { commands, describeError, type Message } from "$lib/ipc/commands";
import { applyRunEvent, emptyTurn, isTerminal, type AssistantTurn } from "$lib/state/run-reducer";
import { providerStore } from "$features/providers/provider-store.svelte";

export interface UserTurn {
  kind: "user";
  id: string;
  text: string;
}
export interface AssistantTurnItem {
  kind: "assistant";
  id: string;
  model: string;
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

const uuid = () => crypto.randomUUID();

/** Converts UI turns into the canonical message list sent to Rust. Failed/empty assistant turns are skipped. */
export function toHistory(items: TurnItem[]): Message[] {
  const out: Message[] = [];
  for (const item of items) {
    if (item.kind === "user") {
      out.push({ role: "user", blocks: [{ type: "text", text: item.text }] });
    } else if (item.turn.text) {
      out.push({ role: "assistant", blocks: [{ type: "text", text: item.turn.text }] });
    }
  }
  return out;
}

class ChatStore {
  chats = $state<Chat[]>([]);
  activeId = $state<string | null>(null);

  get active(): Chat | null {
    return this.chats.find((c) => c.id === this.activeId) ?? null;
  }

  newChat(): Chat {
    const current = this.active;
    if (current && current.items.length === 0) return current;
    const chat: Chat = { id: uuid(), title: "New chat", createdAt: Date.now(), items: [], activeRunId: null };
    this.chats.unshift(chat);
    this.activeId = chat.id;
    return chat;
  }

  select(id: string) {
    this.activeId = id;
  }

  remove(id: string) {
    this.chats = this.chats.filter((c) => c.id !== id);
    if (this.activeId === id) this.activeId = this.chats[0]?.id ?? null;
    if (!this.activeId) this.newChat();
  }

  async send(chat: Chat, prompt: string) {
    const text = prompt.trim();
    if (!text || chat.activeRunId) return;
    const selection = providerStore.active;

    chat.items.push({ kind: "user", id: uuid(), text });
    if (chat.items.length === 1) chat.title = text.length > 48 ? `${text.slice(0, 48)}…` : text;

    const assistant: AssistantTurnItem = { kind: "assistant", id: uuid(), model: selection?.model ?? "", turn: emptyTurn() };
    chat.items.push(assistant);

    if (!selection) {
      assistant.turn = { ...assistant.turn, phase: "failed", error: "Choose a provider and model first (⌘, / Ctrl+,)." };
      return;
    }

    const history = toHistory(chat.items);
    try {
      const runId = await commands.startRun(chat.id, selection.provider_id, selection.model, history, (event) => {
        assistant.turn = applyRunEvent(assistant.turn, event);
        if (isTerminal(assistant.turn.phase)) chat.activeRunId = null;
      });
      if (!isTerminal(assistant.turn.phase)) chat.activeRunId = runId;
    } catch (e) {
      assistant.turn = { ...assistant.turn, phase: "failed", error: describeError(e) };
      chat.activeRunId = null;
    }
  }

  /** Re-runs the last user prompt as a fresh attempt (the failed/stopped turn is replaced). */
  async retry(chat: Chat) {
    const last = chat.items.at(-1);
    const prev = chat.items.at(-2);
    if (!last || last.kind !== "assistant" || !prev || prev.kind !== "user" || chat.activeRunId) return;
    chat.items.splice(-2, 2);
    await this.send(chat, prev.text);
  }

  async stop(chat: Chat) {
    if (chat.activeRunId) await commands.cancelRun(chat.activeRunId);
  }
}

export const chatStore = new ChatStore();
