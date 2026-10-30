import { describe, expect, it } from "vitest";
import { applyRunEvent, emptyTurn, isTerminal } from "./run-reducer";
import type { RunEvent } from "$lib/generated/bindings/RunEvent";

const base = { schema: 1, chat_id: "c", run_id: "r" };
const ev = (seq: number, payload: RunEvent["payload"]): RunEvent => ({ ...base, seq, payload });

describe("applyRunEvent", () => {
  it("accumulates text and tracks phases", () => {
    let t = emptyTurn();
    t = applyRunEvent(t, ev(0, { type: "phase", phase: "generating" }));
    t = applyRunEvent(t, ev(1, { type: "text_delta", text: "Hel" }));
    t = applyRunEvent(t, ev(2, { type: "text_delta", text: "lo" }));
    t = applyRunEvent(t, ev(3, { type: "completed", stop_reason: "end_turn" }));
    expect(t.text).toBe("Hello");
    expect(t.phase).toBe("completed");
    expect(t.desynced).toBe(false);
    expect(isTerminal(t.phase)).toBe(true);
  });

  it("flags sequence gaps", () => {
    let t = applyRunEvent(emptyTurn(), ev(0, { type: "phase", phase: "queued" }));
    t = applyRunEvent(t, ev(2, { type: "text_delta", text: "x" }));
    expect(t.desynced).toBe(true);
  });

  it("ignores events from another run", () => {
    let t = applyRunEvent(emptyTurn(), ev(0, { type: "text_delta", text: "a" }));
    t = applyRunEvent(t, { ...ev(1, { type: "text_delta", text: "b" }), run_id: "other" });
    expect(t.text).toBe("a");
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
    expect(t.text).toBe("hi");
  });
});
