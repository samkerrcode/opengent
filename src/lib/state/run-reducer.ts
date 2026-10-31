// Pure reducer applying ordered RunEvents to an assistant turn. Kept free of Svelte so it is
// unit-testable and reusable by any view.
import type { RunEvent } from "$lib/generated/bindings/RunEvent";
import type { RunPhase } from "$lib/generated/bindings/RunPhase";
import type { StopReason } from "$lib/generated/bindings/StopReason";
import type { ToolCallInfo } from "$lib/generated/bindings/ToolCallInfo";
import type { ToolCallStatus } from "$lib/generated/bindings/ToolCallStatus";
import type { Usage } from "$lib/generated/bindings/Usage";

export const RUN_EVENT_SCHEMA = 2;

export interface ToolSegment {
  kind: "tool";
  call: ToolCallInfo;
  status: ToolCallStatus;
  /** MCP-style result parts; null until the call finishes. */
  content: unknown;
  isError: boolean;
}
export type TurnSegment = { kind: "text"; text: string } | { kind: "thinking"; text: string } | ToolSegment;

export interface AssistantTurn {
  runId: string | null;
  phase: RunPhase;
  /** Ordered stream: text/thinking/tool segments as they were produced. */
  segments: TurnSegment[];
  usage: Usage | null;
  stopReason: StopReason | null;
  error: string | null;
  notices: string[];
  lastSeq: number;
  /** Set when a sequence gap was observed; the UI should request a snapshot (Phase 1+). */
  desynced: boolean;
}

export function emptyTurn(): AssistantTurn {
  return { runId: null, phase: "queued", segments: [], usage: null, stopReason: null, error: null, notices: [], lastSeq: -1, desynced: false };
}

export function isTerminal(phase: RunPhase): boolean {
  return phase === "completed" || phase === "cancelled" || phase === "failed";
}

/** All assistant text across segments (for copy/export). */
export function turnText(turn: AssistantTurn): string {
  return turn.segments.filter((s) => s.kind === "text").map((s) => s.text).join("\n\n");
}

export function turnThinking(turn: AssistantTurn): string {
  return turn.segments.filter((s) => s.kind === "thinking").map((s) => s.text).join("\n\n");
}

// Copy-on-write updates: unchanged segments keep their object identity so Svelte doesn't
// re-render (or re-run markdown-it + DOMPurify on) them for every streamed delta.
function pushText(turn: AssistantTurn, kind: "text" | "thinking", delta: string) {
  const segs = turn.segments;
  const last = segs[segs.length - 1];
  if (last && last.kind === kind) {
    turn.segments = [...segs.slice(0, -1), { ...last, text: last.text + delta }];
  } else {
    turn.segments = [...segs, { kind, text: delta }];
  }
}

function patchToolSegment(turn: AssistantTurn, callId: string, patch: Partial<ToolSegment>) {
  const idx = turn.segments.findIndex((s) => s.kind === "tool" && s.call.id === callId);
  const seg = idx >= 0 ? turn.segments[idx] : undefined;
  if (!seg || seg.kind !== "tool") return;
  turn.segments = [...turn.segments.slice(0, idx), { ...seg, ...patch }, ...turn.segments.slice(idx + 1)];
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
      pushText(next, "text", p.text);
      break;
    case "thinking_delta":
      pushText(next, "thinking", p.text);
      break;
    case "usage":
      next.usage = p.usage;
      break;
    case "notice":
      next.notices = [...turn.notices, p.message];
      break;
    case "tool_call":
      next.segments = [...turn.segments, { kind: "tool", call: p.call, status: "running", content: null, isError: false }];
      break;
    case "tool_status":
      patchToolSegment(next, p.call_id, { status: p.status });
      break;
    case "tool_result":
      patchToolSegment(next, p.call_id, { status: p.status, content: p.content, isError: p.is_error });
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
