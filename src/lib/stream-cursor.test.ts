// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { renderMarkdown } from "./markdown";
import { markdownHtml } from "./stream-cursor";

function render(source: string, streaming = true) {
  const root = document.createElement("div");
  markdownHtml(renderMarkdown(source), streaming)(root);
  const cursor = root.querySelector(".stream-cursor");
  return { root, cursor, host: cursor?.parentElement, prev: cursor?.previousSibling?.textContent };
}

describe("placeStreamCursor", () => {
  it("ends a paragraph", () => {
    const { host, prev } = render("Hello **bold** wor");
    expect(host?.tagName).toBe("P");
    expect(prev).toBe(" wor");
  });

  it("follows into the deepest nested list item", () => {
    const { host, prev } = render("- a\n  - b\n    - deep tok");
    expect(host?.tagName).toBe("LI");
    expect(host?.closest("ul ul ul")).not.toBeNull();
    expect(prev).toBe("deep tok");
  });

  it("lands in the last table cell, including an empty one", () => {
    const filled = render("| a | b |\n|---|---|\n| 1 | tw");
    expect(filled.host?.tagName).toBe("TD");
    expect(filled.prev).toBe("tw");

    const empty = render("| a | b |\n|---|---|\n| 1 |");
    expect(empty.host?.tagName).toBe("TD");
    expect(empty.host?.textContent).toBe("");
  });

  it("stays inside a blockquote's text", () => {
    const { host } = render("> quoted te");
    expect(host?.tagName).toBe("P");
    expect(host?.parentElement?.tagName).toBe("BLOCKQUOTE");
  });

  it("sits on the last code line, before the trailing newline", () => {
    const mid = render("```js\nlet a = 1;\nlet b");
    expect(mid.cursor?.closest("pre")).not.toBeNull();
    expect(mid.prev).toBe(" b");

    const eol = render("```js\nlet a = 1;\nlet b\n");
    expect(eol.prev).toBe(" b");
    expect(eol.cursor?.nextSibling?.textContent).toBe("\n");
    expect(eol.root.querySelector("pre")?.textContent).toBe("let a = 1;\nlet b\n");
  });

  it("goes after rendered math, never inside it", () => {
    const { cursor } = render("Inline $x^2$");
    expect(cursor?.closest(".katex")).toBeNull();
    expect(cursor?.previousElementSibling?.classList.contains("katex")).toBe(true);
  });

  it("renders no cursor when not streaming", () => {
    expect(render("- a\n  - b", false).cursor).toBeNull();
  });
});
