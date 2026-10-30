// Pure reducer applying ordered RunEvents to an assistant turn. Kept free of Svelte so it is
// unit-testable and reusable by any view.
import type { RunEvent } from "$lib/generated/bindings/RunEvent";
import type { RunPhase } from "$lib/generated/bindings/RunPhase";
import type { StopReason } from "$lib/generated/bindings/StopReason";
import type { Usage } from "$lib/generated/bindings/Usage";

export const RUN_EVENT_SCHEMA = 1;

export interface AssistantTurn {
  runId: string | null;
  phase: RunPhase;
  text: string;
  /** Model reasoning streamed before the answer (thinking models). */
  thinking: string;
  usage: Usage | null;
  stopReason: StopReason | null;
  error: string | null;
  notices: string[];
  lastSeq: number;
  /** Set when a sequence gap was observed; the UI should request a snapshot (Phase 1+). */
  desynced: boolean;
}

export function emptyTurn(): AssistantTurn {
  return { runId: null, phase: "queued", text: "", thinking: "", usage: null, stopReason: null, error: null, notices: [], lastSeq: -1, desynced: false };
}

export function isTerminal(phase: RunPhase): boolean {
  return phase === "completed" || phase === "cancelled" || phase === "failed";
}

export function applyRunEvent(turn: AssistantTurn, event: RunEvent): AssistantTurn {
  if (event.schema !== RUN_EVENT_SCHEMA) {
    return { ...turn, phase: "failed", error: `unsupported event schema ${event.schema}` };
  }
  if (turn.runId !== null && turn.runId !== event.run_id) return turn;
  const next: AssistantTurn = { ...turn, runId: event.run_id, lastSeq: event.seq };
  if (event.seq !== turn.lastSeq + 1) next.desynced = true;

  const p = event.payload;
  switch (p.type) {
    case "phase":
      next.phase = p.phase;
      break;
    case "text_delta":
      next.text += p.text;
      break;
    case "thinking_delta":
      next.thinking += p.text;
      break;
    case "usage":
      next.usage = p.usage;
      break;
    case "notice":
      next.notices = [...turn.notices, p.message];
      break;
    case "completed":
      next.phase = "completed";
      next.stopReason = p.stop_reason;
      break;
    case "cancelled":
      next.phase = "cancelled";
      break;
    case "failed":
      next.phase = "failed";
      next.error = p.message;
      break;
  }
  return next;
}
