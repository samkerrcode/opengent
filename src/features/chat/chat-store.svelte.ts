// Chat state cached from Rust (SQLite is authoritative). Streaming events are applied to the
// in-memory copy of the assistant message; Rust persists them independently via RunRecorder.
import {
  commands,
  describeError,
  type AttachmentMeta,
  type ChatSummary,
  type StoredMessage,
} from "$lib/ipc/commands";
import { applyRunEvent, emptyTurn, isTerminal, type AssistantTurn, type TurnSegment } from "$lib/state/run-reducer";
import type { ToolCallStatus } from "$lib/ipc/commands";
import { providerStore } from "$features/providers/provider-store.svelte";

export interface UserTurn {
  kind: "user";
  id: string;
  text: string;
  attachments: { id: string; name: string; mime: string; char_count: number }[];
}
export interface AssistantTurnItem {
  kind: "assistant";
  id: string;
  model: string;
  turn: AssistantTurn;
}
export type TurnItem = UserTurn | AssistantTurnItem;

export interface Chat {
  summary: ChatSummary;
  items: TurnItem[];
  loaded: boolean;
  activeRunId: string | null;
  /** Attachments picked for the next message. */
  pending: AttachmentMeta[];
}

const SEND_DEBOUNCE_MS = 120;

function fromStored(m: StoredMessage): TurnItem {
  if (m.role === "user") {
    const text = m.blocks
      .filter((b): b is Extract<typeof b, { type: "text" }> => b.type === "text")
      .map((b) => b.text)
      .join("\n");
    const attachments = m.blocks
      .filter((b): b is Extract<typeof b, { type: "attachment" }> => b.type === "attachment")
      .map((b) => ({ id: b.id, name: b.name, mime: b.mime, char_count: b.char_count }));
    return { kind: "user", id: m.id, text, attachments };
  }
  const segments: TurnSegment[] = [];
  for (const b of m.blocks) {
    if (b.type === "text" || b.type === "thinking") {
      const last = segments[segments.length - 1];
      if (last && last.kind === b.type) last.text += b.text;
      else segments.push({ kind: b.type, text: b.text });
    } else if (b.type === "tool_call") {
      segments.push({
        kind: "tool",
        call: { id: b.id, name: b.name, server: b.server ?? null, tool: b.tool ?? b.name, arguments: b.arguments },
        // Filled in when the matching tool_result block arrives.
        status: "completed",
        content: null,
        isError: false,
      });
    } else if (b.type === "tool_result") {
      const seg = segments.find(
        (s): s is Extract<TurnSegment, { kind: "tool" }> => s.kind === "tool" && s.call.id === b.call_id,
      );
      if (seg) {
        seg.status = (b.status ?? (b.is_error ? "failed" : "completed")) as ToolCallStatus;
        seg.content = b.content;
        seg.isError = b.is_error;
      }
    }
  }
  // Calls that never produced a result (cancelled/interrupted mid-call).
  for (const s of segments) {
    if (s.kind === "tool" && s.content === null) s.status = "cancelled";
  }
  const turn: AssistantTurn = {
    ...emptyTurn(),
    runId: m.run_id,
    segments,
    usage: m.usage,
    error: m.error,
    phase:
      m.status === "complete" ? "completed"
      : m.status === "cancelled" ? "cancelled"
      : m.status === "failed" ? "failed"
      : m.status === "interrupted" ? "failed"
      : "generating",
  };
  if (m.status === "interrupted") turn.error = "Generation was interrupted when the app closed.";
  return { kind: "assistant", id: m.id, model: m.model ?? "", turn };
}

class ChatStore {
  chats = $state<Chat[]>([]);
  activeId = $state<string | null>(null);
  query = $state("");
  error = $state<string | null>(null);
  private searchTimer: ReturnType<typeof setTimeout> | null = null;

  get active(): Chat | null {
    return this.chats.find((c) => c.summary.id === this.activeId) ?? null;
  }

  private upsert(summary: ChatSummary): Chat {
    let chat = this.chats.find((c) => c.summary.id === summary.id);
    if (chat) {
      chat.summary = summary;
    } else {
      chat = { summary, items: [], loaded: false, activeRunId: null, pending: [] };
      this.chats.unshift(chat);
    }
    return chat;
  }

  async loadList() {
    try {
      const list = this.query.trim() ? await commands.searchChats(this.query) : await commands.listChats();
      const ids = new Set(list.map((s) => s.id));
      // Keep local state for chats still present; drop the rest (except an unsaved active chat).
      const kept = new Map(this.chats.filter((c) => ids.has(c.summary.id)).map((c) => [c.summary.id, c]));
      this.chats = list.map((s) => {
        const existing = kept.get(s.id);
        if (existing) {
          existing.summary = s;
          return existing;
        }
        return { summary: s, items: [], loaded: false, activeRunId: null, pending: [] };
      });
      this.error = null;
    } catch (e) {
      this.error = describeError(e);
    }
  }

  setQuery(q: string) {
    this.query = q;
    if (this.searchTimer) clearTimeout(this.searchTimer);
    this.searchTimer = setTimeout(() => void this.loadList(), SEND_DEBOUNCE_MS);
  }

  async ensureLoaded(chat: Chat) {
    if (chat.loaded || chat.activeRunId) return;
    try {
      const detail = await commands.getChat(chat.summary.id);
      chat.summary = detail.chat;
      chat.items = detail.messages.map(fromStored);
      chat.loaded = true;
    } catch (e) {
      this.error = describeError(e);
    }
  }

  async newChat(): Promise<Chat | null> {
    const current = this.active;
    if (current && current.loaded && current.items.length === 0) return current;
    try {
      const summary = await commands.createChat();
      const chat = this.upsert(summary);
      chat.loaded = true;
      this.activeId = chat.summary.id;
      return chat;
    } catch (e) {
      this.error = describeError(e);
      return null;
    }
  }

  async select(id: string) {
    this.activeId = id;
    const chat = this.active;
    if (chat) await this.ensureLoaded(chat);
  }

  async rename(chat: Chat, title: string) {
    try {
      await commands.renameChat(chat.summary.id, title);
      chat.summary.title = title.trim();
    } catch (e) {
      this.error = describeError(e);
    }
  }

  async remove(id: string) {
    try {
      await commands.deleteChat(id);
    } catch (e) {
      this.error = describeError(e);
      return;
    }
    this.chats = this.chats.filter((c) => c.summary.id !== id);
    if (this.activeId === id) {
      this.activeId = this.chats[0]?.summary.id ?? null;
      if (this.activeId) await this.select(this.activeId);
      else await this.newChat();
    }
  }

  async pickAttachments(chat: Chat) {
    try {
      const picked = await commands.pickAttachments(providerStore.supportsImages);
      for (const a of picked) if (!chat.pending.some((p) => p.id === a.id)) chat.pending.push(a);
    } catch (e) {
      this.error = describeError(e);
    }
  }

  removePending(chat: Chat, id: string) {
    chat.pending = chat.pending.filter((a) => a.id !== id);
  }

  async send(chat: Chat, prompt: string, replaceFrom: string | null = null) {
    const text = prompt.trim();
    if ((!text && chat.pending.length === 0) || chat.activeRunId) return;
    const selection = providerStore.active;
    const attachmentIds = chat.pending.map((a) => a.id);

    // Optimistic local turns; replaced by the stored copies once Rust confirms.
    // IMPORTANT: after push(), mutations must go through chat.items[i] — the $state proxy —
    // not the raw object reference, or the UI will not re-render.
    const idx = chat.items.length;
    chat.items.push(
      {
        kind: "user",
        id: `local-${crypto.randomUUID()}`,
        text,
        attachments: chat.pending.map((a) => ({ id: a.id, name: a.name, mime: a.mime, char_count: a.char_count })),
      },
      { kind: "assistant", id: `local-${crypto.randomUUID()}`, model: selection?.model ?? "", turn: emptyTurn() },
    );
    chat.pending = [];
    if (chat.summary.title === "New chat" && text) chat.summary.title = text.length > 60 ? `${text.slice(0, 60)}…` : text;

    const assistantItem = (): AssistantTurnItem | null => {
      const item = chat.items[idx + 1];
      return item?.kind === "assistant" ? item : null;
    };
    const fail = (message: string) => {
      const item = assistantItem();
      if (item) item.turn = { ...item.turn, phase: "failed", error: message };
      chat.activeRunId = null;
    };

    if (!selection) {
      fail("Choose a provider and model first (⌘, / Ctrl+,).");
      return;
    }

    chat.activeRunId = "pending";
    try {
      const started = await commands.startRun(
        chat.summary.id,
        selection.provider_id,
        selection.model,
        text,
        attachmentIds,
        replaceFrom,
        (event) => {
          try {
            const item = assistantItem();
            if (!item) return;
            item.turn = applyRunEvent(item.turn, event);
            if (isTerminal(item.turn.phase)) {
              chat.activeRunId = null;
              chat.summary.updated_at = Date.now();
              // Rust already persisted the final state; pull it so the view matches the
              // database even if intermediate events were missed.
              void this.refresh(chat);
            }
          } catch (e) {
            console.error("failed to apply run event", e, event);
          }
        },
      );
      const user = chat.items[idx];
      const asst = assistantItem();
      if (user?.kind === "user") user.id = started.user_message.id;
      if (asst) asst.id = started.assistant_message.id;
      chat.summary.message_count = chat.items.length;
      if (!isTerminal(asst?.turn.phase ?? "queued")) chat.activeRunId = started.run_id;
    } catch (e) {
      fail(describeError(e));
    }
  }

  /** Re-reads a chat from SQLite, preserving any optimistic (unsent) tail items. */
  private async refresh(chat: Chat) {
    try {
      const detail = await commands.getChat(chat.summary.id);
      chat.summary = detail.chat;
      const optimistic = chat.items.filter((i) => i.id.startsWith("local-"));
      chat.items = [...detail.messages.map(fromStored), ...optimistic];
      chat.loaded = true;
    } catch {
      // Keep the streamed state if the refetch fails.
    }
  }

  /** Re-runs the last user prompt as a fresh attempt; Rust removes the old pair first. */
  async retry(chat: Chat) {
    const last = chat.items.at(-1);
    const prev = chat.items.at(-2);
    if (!last || last.kind !== "assistant" || !prev || prev.kind !== "user" || chat.activeRunId) return;
    const replaceFrom = prev.id.startsWith("local-") ? null : prev.id;
    chat.items.splice(-2, 2);
    chat.pending = prev.attachments.map((a) => ({ id: a.id, name: a.name, char_count: a.char_count, sha256: "", mime: a.mime, size_bytes: 0 }));
    await this.send(chat, prev.text, replaceFrom);
  }

  async stop(chat: Chat) {
    if (chat.activeRunId && chat.activeRunId !== "pending") await commands.cancelRun(chat.activeRunId);
  }
}

export const chatStore = new ChatStore();
