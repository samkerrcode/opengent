// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { renderMarkdown } from "./markdown";
import { applyHtml } from "./stream-cursor";

const fresh = (source: string, partial = true) => {
  const el = document.createElement("div");
  el.innerHTML = renderMarkdown(source, { partial });
  return el;
};

const trace = [
  "Approach A: slicing the circle.",
  "",
  "- Area of a strip: $dA = 2\\sqrt{r^2 - x^2}\\,dx$",
  "- Integrate from $-r$ to $r$:",
  "  $$",
  "  A = \\int_{-r}^{r} 2\\sqrt{r^2 - x^2}\\,dx",
  "  $$",
  "- Substitute $x = r\\sin\\theta$, so",
  "  1. $dx = r\\cos\\theta\\,d\\theta$",
  "  2. bounds become $\\pm\\pi/2$",
  "",
  "Heading",
  "---",
  "",
  "```math",
  "A = \\pi r^2",
  "```",
  "",
  "| step | result |",
  "|---|---|",
  "| polar | $\\tfrac12 r^2 \\cdot 2\\pi$ |",
].join("\n");

describe("patchChildren via applyHtml", () => {
  // Every third prefix (jsdom's DOM is slow), alternating the caret, which splits text.
  it("matches a full render after streamed prefixes, with and without a caret", { timeout: 30_000 }, () => {
    const el = document.createElement("div");
    const full = document.createElement("div");
    for (let i = 0; i <= trace.length; i += 3) {
      const html = renderMarkdown(trace.slice(0, i), { partial: true });
      applyHtml(el, html, i % 2 === 0);
      el.querySelector(".stream-cursor")?.remove();
      el.normalize();
      full.innerHTML = html;
      expect(el.isEqualNode(full), `prefix ${i}`).toBe(true);
    }
    applyHtml(el, renderMarkdown(trace), false);
    expect(el.innerHTML).toBe(fresh(trace, false).innerHTML);
  });

  it("keeps unchanged nodes, including earlier items of a growing list", () => {
    const el = document.createElement("div");
    const src = "Intro $x^2$.\n\n- first $a_1$\n- second $a_2$\n- thi";
    applyHtml(el, renderMarkdown(src, { partial: true }), true);
    const intro = el.querySelector("p");
    const list = el.querySelector("ul");
    const [first, second] = el.querySelectorAll("li");
    const formula = first!.querySelector(".katex");

    applyHtml(el, renderMarkdown(src + "rd $a_3$\n- fourth", { partial: true }), true);
    expect(el.querySelector("p")).toBe(intro);
    expect(el.querySelector("ul")).toBe(list);
    const items = el.querySelectorAll("li");
    expect(items[0]).toBe(first);
    expect(items[1]).toBe(second);
    expect(items[0]!.querySelector(".katex")).toBe(formula);
    expect(items).toHaveLength(4);
    expect(el.querySelectorAll(".stream-cursor")).toHaveLength(1);
  });

  it("replaces a block whose type changes", () => {
    const el = document.createElement("div");
    applyHtml(el, renderMarkdown("Title\n-", { partial: true }), false);
    applyHtml(el, renderMarkdown("Title\n---", { partial: true }), false);
    expect(el.querySelector("h2")?.textContent).toBe("Title");
    expect(el.isEqualNode(fresh("Title\n---"))).toBe(true);
  });
});
