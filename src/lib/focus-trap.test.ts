// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { trapFocus } from "./focus-trap";

function setup() {
  document.body.innerHTML = `
    <button id="opener">open</button>
    <div id="dialog" tabindex="-1">
      <button id="a">a</button>
      <button id="off" disabled>off</button>
      <input id="b" />
      <button id="skip" tabindex="-1">skip</button>
    </div>`;
  const opener = document.getElementById("opener")!;
  opener.focus();
  const dialog = document.getElementById("dialog")!;
  const action = trapFocus(dialog);
  const tab = (shiftKey = false) =>
    document.activeElement!.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", shiftKey, bubbles: true, cancelable: true }));
  return { opener, dialog, action, tab, byId: (id: string) => document.getElementById(id)! };
}

afterEach(() => {
  document.body.innerHTML = "";
});

describe("trapFocus", () => {
  it("focuses the dialog and wraps Tab at both ends, skipping disabled and untabbable controls", () => {
    const { dialog, tab, byId } = setup();
    expect(document.activeElement).toBe(dialog);
    tab();
    expect(document.activeElement).toBe(byId("a"));
    byId("b").focus();
    tab();
    expect(document.activeElement).toBe(byId("a"));
    tab(true);
    expect(document.activeElement).toBe(byId("b"));
  });

  it("skips controls hidden by CSS or a closed details element", () => {
    const { dialog, tab, byId } = setup();
    dialog.insertAdjacentHTML(
      "beforeend",
      `<div style="display: none"><button>gone</button></div>
       <button style="visibility: hidden">ghost</button>
       <details><summary id="sum">more</summary><button>folded</button></details>`,
    );
    byId("a").focus();
    tab(true);
    expect(document.activeElement).toBe(byId("sum"));
    tab();
    expect(document.activeElement).toBe(byId("a"));
  });

  it("pulls focus back in when it fell out of the dialog", () => {
    const { action, tab, byId } = setup();
    byId("a").focus();
    byId("a").remove();
    expect(document.activeElement).toBe(document.body);
    tab();
    expect(document.activeElement).toBe(byId("b"));
    action.destroy();
    byId("opener").focus();
    tab();
    expect(document.activeElement).toBe(byId("opener"));
  });

  it("restores focus to the opener on destroy", () => {
    const { opener, action, dialog } = setup();
    dialog.remove();
    action.destroy();
    expect(document.activeElement).toBe(opener);
  });

  it("leaves focus alone when another dialog already took it", () => {
    const { action, dialog } = setup();
    const next = document.createElement("button");
    document.body.appendChild(next);
    next.focus();
    dialog.remove();
    action.destroy();
    expect(document.activeElement).toBe(next);
  });
});
