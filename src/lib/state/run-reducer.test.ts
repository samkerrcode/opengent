import { describe, expect, it } from "vitest";
import { applyRunEvent, emptyTurn, isTerminal, turnText } from "./run-reducer";
import type { RunEvent } from "$lib/generated/bindings/RunEvent";

const base = { schema: 2, chat_id: "c", run_id: "r" };
const ev = (seq: number, payload: RunEvent["payload"]): RunEvent => ({ ...base, seq, payload });

describe("applyRunEvent", () => {
  it("accumulates text and tracks phases", () => {
    let t = emptyTurn();
    t = applyRunEvent(t, ev(0, { type: "phase", phase: "generating" }));
    t = applyRunEvent(t, ev(1, { type: "text_delta", text: "Hel" }));
    t = applyRunEvent(t, ev(2, { type: "text_delta", text: "lo" }));
    t = applyRunEvent(t, ev(3, { type: "completed", stop_reason: "end_turn" }));
    expect(turnText(t)).toBe("Hello");
    expect(t.phase).toBe("completed");
    expect(t.desynced).toBe(false);
    expect(isTerminal(t.phase)).toBe(true);
  });

  it("keeps text around tool calls in separate segments", () => {
    let t = emptyTurn();
    t = applyRunEvent(t, ev(0, { type: "text_delta", text: "Checking." }));
    t = applyRunEvent(
      t,
      ev(1, {
        type: "tool_call",
        call: { id: "c1", name: "fs__list", server: "fs", tool: "list", arguments: {} },
      }),
    );
    t = applyRunEvent(t, ev(2, { type: "tool_status", call_id: "c1", status: "pending_approval" }));
    t = applyRunEvent(
      t,
      ev(3, {
        type: "tool_result",
        call_id: "c1",
        content: [{ type: "text", text: "a" }],
        is_error: false,
        status: "completed",
      }),
    );
    t = applyRunEvent(t, ev(4, { type: "text_delta", text: "Done." }));
    expect(t.segments.map((s) => s.kind)).toEqual(["text", "tool", "text"]);
    const tool = t.segments[1];
    expect(tool?.kind).toBe("tool");
    if (tool?.kind === "tool") expect(tool.status).toBe("completed");
    expect(turnText(t)).toBe("Checking.\n\nDone.");
  });

  it("flags sequence gaps", () => {
    let t = applyRunEvent(emptyTurn(), ev(0, { type: "phase", phase: "queued" }));
    t = applyRunEvent(t, ev(2, { type: "text_delta", text: "x" }));
    expect(t.desynced).toBe(true);
  });

  it("ignores events from another run", () => {
    let t = applyRunEvent(emptyTurn(), ev(0, { type: "text_delta", text: "a" }));
    t = applyRunEvent(t, { ...ev(1, { type: "text_delta", text: "b" }), run_id: "other" });
    expect(turnText(t)).toBe("a");
  });

  it("records failures", () => {
    const t = applyRunEvent(emptyTurn(), ev(0, { type: "failed", message: "boom" }));
    expect(t.phase).toBe("failed");
    expect(t.error).toBe("boom");
  });
});

describe("notices", () => {
  it("accumulates notices without touching text", () => {
    let t = applyRunEvent(emptyTurn(), ev(0, { type: "notice", message: "trimmed" }));
    t = applyRunEvent(t, ev(1, { type: "text_delta", text: "hi" }));
    expect(t.notices).toEqual(["trimmed"]);
    expect(turnText(t)).toBe("hi");
  });
});
