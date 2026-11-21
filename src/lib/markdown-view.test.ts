// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { markdownView } from "./markdown-view";

let frames: FrameRequestCallback[] = [];
const flushFrame = () => {
  const queued = frames;
  frames = [];
  for (const cb of queued) cb(0);
};

beforeEach(() => {
  frames = [];
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => frames.push(cb));
  vi.stubGlobal("cancelAnimationFrame", (id: number) => {
    frames[id - 1] = () => {};
  });
});
afterEach(() => vi.unstubAllGlobals());

const mount = () => {
  const node = document.createElement("div");
  document.body.appendChild(node);
  return node;
};

describe("markdownView", () => {
  it("coalesces deferred renders to one per frame, of the newest text", () => {
    const node = mount();
    const onrender = vi.fn();
    for (const text of ["Hel", "Hello wo", "Hello world"]) markdownView(text, { partial: true, deferred: true, onrender })(node);
    expect(node.textContent).toBe("");
    expect(frames).toHaveLength(1);
    flushFrame();
    expect(node.textContent?.trim()).toBe("Hello world");
    expect(onrender).toHaveBeenCalledTimes(1);
  });

  it("renders synchronously once streaming ends and drops the queued frame", () => {
    const node = mount();
    const onrender = vi.fn();
    markdownView("partial $$\nx_{", { partial: true, deferred: true, onrender })(node);
    markdownView("final", { partial: false, onrender })(node);
    expect(node.textContent?.trim()).toBe("final");
    flushFrame();
    expect(node.textContent?.trim()).toBe("final");
    expect(onrender).toHaveBeenCalledTimes(1);
  });

  it("does nothing when re-run with the input it already shows", () => {
    const node = mount();
    const onrender = vi.fn();
    markdownView("same $x$", { partial: false, onrender })(node);
    const p = node.querySelector("p");
    markdownView("same $x$", { partial: false, onrender })(node);
    markdownView("same $x$", { partial: false, deferred: true, onrender })(node);
    expect(frames).toHaveLength(0);
    expect(onrender).toHaveBeenCalledTimes(1);
    expect(node.querySelector("p")).toBe(p);
  });

  it("skips a queued render for a node that was removed", () => {
    const node = mount();
    markdownView("gone", { partial: true, deferred: true })(node);
    node.remove();
    flushFrame();
    expect(node.textContent).toBe("");
  });
});
