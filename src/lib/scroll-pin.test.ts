// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { ScrollPin } from "./scroll-pin.svelte";

// jsdom has no layout: fake a 100px viewport over `height` px of content.
function scroller(height = 1000) {
  const el = document.createElement("div");
  let content = height;
  Object.defineProperty(el, "clientHeight", { get: () => 100 });
  Object.defineProperty(el, "scrollHeight", { get: () => content });
  document.body.appendChild(el);
  return { el, grow: (by: number) => (content += by) };
}

function setup(threshold = 8) {
  const { el, grow } = scroller();
  const pin = new ScrollPin({ threshold });
  pin.attach(el);
  pin.scrollToBottom();
  el.dispatchEvent(new Event("scroll"));
  return { el, grow, pin, scrollTo: (top: number) => ((el.scrollTop = top), el.dispatchEvent(new Event("scroll"))) };
}

describe("ScrollPin", () => {
  it("follows growth while pinned, absorbing its own scroll events", () => {
    const { el, grow, pin } = setup();
    grow(200);
    pin.follow();
    el.dispatchEvent(new Event("scroll"));
    expect(pin.pinned).toBe(true);
    expect(el.scrollTop).toBe(1100);
  });

  it("unpins on the first upward wheel, before any scroll event, and stops writing scrollTop", () => {
    const { el, grow, pin } = setup();
    el.dispatchEvent(new WheelEvent("wheel", { deltaY: -10 }));
    expect(pin.pinned).toBe(false);
    grow(500);
    pin.follow();
    expect(el.scrollTop).toBe(900);
  });

  it("unpins on upward keys but not on keys typed into an input", () => {
    const { el, pin } = setup();
    const input = document.createElement("input");
    el.appendChild(input);
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true }));
    expect(pin.pinned).toBe(true);
    el.dispatchEvent(new KeyboardEvent("keydown", { key: "PageUp" }));
    expect(pin.pinned).toBe(false);
  });

  it("unpins on an upward scroll (e.g. an overlay scrollbar drag) and re-pins at the bottom", () => {
    const { pin, scrollTo } = setup();
    scrollTo(600);
    expect(pin.pinned).toBe(false);
    scrollTo(895);
    expect(pin.pinned).toBe(true);
  });

  it("stays pinned when a nested scroller consumes the wheel", () => {
    const { el, pin } = setup(48);
    const inner = document.createElement("div");
    inner.style.overflowY = "auto";
    Object.defineProperty(inner, "clientHeight", { get: () => 50 });
    Object.defineProperty(inner, "scrollHeight", { get: () => 300 });
    inner.scrollTop = 40;
    el.appendChild(inner);
    inner.dispatchEvent(new WheelEvent("wheel", { deltaY: -10, bubbles: true }));
    expect(pin.pinned).toBe(true);
  });
});
