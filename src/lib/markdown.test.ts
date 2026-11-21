// @vitest-environment jsdom
import { createRequire } from "node:module";
import { describe, expect, it, vi } from "vitest";
import { renderMarkdown } from "./markdown";

describe("renderMarkdown", () => {
  it("renders basic markdown", () => {
    const html = renderMarkdown("# Hi\n\nSome **bold** text.");
    expect(html).toContain("<h1>Hi</h1>");
    expect(html).toContain("<strong>bold</strong>");
  });

  it("strips raw html and scripts", () => {
    const html = renderMarkdown('<script>alert(1)</script><img src=x onerror="alert(1)">\n\ntext');
    const doc = new DOMParser().parseFromString(html, "text/html");
    expect(doc.querySelectorAll("script, img").length).toBe(0);
    expect(doc.querySelectorAll("[onerror]").length).toBe(0);
    // The raw markup survives only as escaped, inert text.
    expect(doc.body.textContent).toContain("<script>");
  });

  it("blocks javascript: links and marks links external", () => {
    const html = renderMarkdown("[x](javascript:alert(1)) [ok](https://example.com)");
    expect(html).not.toMatch(/href="javascript:/);
    expect(html).toContain('href="https://example.com"');
    expect(html).toContain('rel="noopener noreferrer"');
  });

  it("labels fenced code with its language", () => {
    const html = renderMarkdown("```rust\nfn main() {}\n```");
    expect(html).toContain('class="code-lang"');
    expect(html).toContain("language-rust");
  });

  it("syntax-highlights code in a registered language", () => {
    const html = renderMarkdown("```rust\nlet x: i32 = 42;\n```");
    const doc = new DOMParser().parseFromString(html, "text/html");
    expect(doc.querySelector(".hljs-keyword")?.textContent).toBe("let");
    expect(doc.querySelector(".hljs-number")?.textContent).toBe("42");
  });

  it("falls back to plain escaped text for unknown languages", () => {
    const html = renderMarkdown("```cobol\nDISPLAY x\n```");
    expect(html).toContain("language-cobol");
    expect(html).not.toContain("hljs-keyword");
    expect(html).toContain("DISPLAY x");
  });

  it("renders a code header with a copy button that survives sanitization", () => {
    const html = renderMarkdown("```js\nlet a = 1;\n```");
    const doc = new DOMParser().parseFromString(html, "text/html");
    const btn = doc.querySelector("button.code-copy");
    expect(btn).not.toBeNull();
    expect(doc.querySelector(".code-head .code-lang")?.textContent).toBe("js");
  });

  it("renders inline $...$ math as KaTeX", () => {
    const html = renderMarkdown("Working in $\\mathbb{Z}_n$ here.");
    const doc = new DOMParser().parseFromString(html, "text/html");
    expect(doc.querySelector(".katex")).not.toBeNull();
    // The rendered glyph survives (Z blackboard-bold), and the raw TeX lives only
    // inside the MathML <annotation> used for accessibility/copy.
    expect(doc.querySelector(".katex-html")).not.toBeNull();
    expect(doc.querySelector("annotation")).not.toBeNull();
  });

  it("renders $$...$$ and \\[...\\] display math blocks", () => {
    const dollars = renderMarkdown("$$\n\\int_0^1 x^2\\,dx\n$$");
    expect(dollars).toContain('class="katex-block"');
    expect(dollars).toContain('class="katex"');

    const brackets = renderMarkdown("\\[\nx = \\sqrt{x^2 - n}\n\\]");
    expect(brackets).toContain('class="katex-block"');
  });

  it("renders \\(...\\) inline math and ```math fenced blocks", () => {
    const parens = renderMarkdown("inline \\(x_i - x_j\\) math");
    expect(new DOMParser().parseFromString(parens, "text/html").querySelector(".katex")).not.toBeNull();

    const fenced = renderMarkdown("```math\nx = \\lceil \\sqrt{n} \\rceil\n```");
    expect(fenced).toContain('class="katex-block"');
    expect(fenced).not.toContain("language-math");
    expect(fenced).not.toContain("<pre>");
  });

  it("renders invalid math as an error span instead of throwing", () => {
    const html = renderMarkdown("$\\badcommand{x}$");
    expect(html).toContain("katex-error");
    expect(html).not.toContain("<math");
  });

  it("does not eat currency dollars", () => {
    const html = renderMarkdown("The price is $5 and $10 total.");
    expect(html).toContain("$5");
    expect(html).toContain("$10");
    expect(html).not.toContain("katex");
  });

  it("keeps the SVG geometry KaTeX draws square roots with", () => {
    const doc = new DOMParser().parseFromString(renderMarkdown("$\\sqrt{r^2 - x^2}$"), "text/html");
    const svg = doc.querySelector(".katex svg");
    expect(svg?.getAttribute("viewBox")).toBeTruthy();
    expect(svg?.getAttribute("width")).toBeTruthy();
    expect(svg?.querySelector("path")?.getAttribute("d")).toBeTruthy();
  });

  it("allows only http(s) and mailto URLs in links and images", () => {
    const doc = new DOMParser().parseFromString(
      renderMarkdown("[a](https://x.test) [b](mailto:a@x.test) [c](ftp://x.test) [d](/relative) ![e](tel:123)"),
      "text/html",
    );
    const hrefs = [...doc.querySelectorAll("a")].map((a) => a.getAttribute("href"));
    expect(hrefs).toEqual(["https://x.test", "mailto:a@x.test", null, null]);
    expect(doc.querySelector("img")?.getAttribute("src") ?? null).toBeNull();
  });

  it("keeps KaTeX output free of active content", () => {
    const html = renderMarkdown("$\\href{javascript:alert(1)}{x}$ and $x$");
    const doc = new DOMParser().parseFromString(html, "text/html");
    expect(doc.querySelectorAll("a[href], script, iframe").length).toBe(0);
  });
});

describe("KaTeX cache", () => {
  it("renders each distinct formula once across repeated renders, with identical output", () => {
    // The plugin is CommonJS: spy on the instance it requires, not the ESM build.
    const katex = createRequire(import.meta.url)("katex") as { renderToString: (...a: unknown[]) => string };
    const spy = vi.spyOn(katex, "renderToString");
    try {
      const src = "Cache $q_{cache}^1$ and \\(q_{cache}^2\\).\n\n$$\nq_{cache}^3\n$$\n\n```math\nq_{cache}^3\n```";
      const first = renderMarkdown(src);
      expect(spy).toHaveBeenCalledTimes(3);
      expect(renderMarkdown(src)).toBe(first);
      expect(renderMarkdown(src, { partial: true })).toBe(first);
      expect(spy).toHaveBeenCalledTimes(3);
    } finally {
      spy.mockRestore();
    }
  });
});

describe("renderMarkdown partial mode", () => {
  const fixture = [
    "Working in $\\mathbb{Z}_n$ and prices $5 and $10.",
    "",
    "$$",
    "\\sum_{i=1}^{N} C_{i} \\cdot x_i",
    "$$",
    "",
    "```math",
    "\\int_0^1 x^2\\,dx = \\frac{1}{3}",
    "```",
    "",
    "\\[",
    "e^{i\\pi} + 1 = 0",
    "\\]",
    "",
    "Done.",
  ].join("\n");

  it("never emits a KaTeX error for any prefix of a math-heavy stream", () => {
    for (let i = 0; i <= fixture.length; i++) {
      const html = renderMarkdown(fixture.slice(0, i), { partial: true });
      expect(html, `prefix of length ${i}`).not.toContain("katex-error");
    }
  });

  it("is identical to the final render once all delimiters are closed", () => {
    expect(renderMarkdown(fixture, { partial: true })).toBe(renderMarkdown(fixture));
  });

  it("renders a closed math fence at the tail as math, even with a trailing newline", () => {
    const html = renderMarkdown("```math\nx_i\n```\n", { partial: true });
    expect(html).toContain('class="katex-block"');
    expect(html).not.toContain("katex-pending");
  });

  it("renders unfinished $$ math as a pending block, not an error", () => {
    const html = renderMarkdown("$$\n\\sum_{i=1}^{N} C_{", { partial: true });
    expect(html).toContain('class="katex-block katex-pending"');
    expect(html).toContain("\\sum_{i=1}^{N} C_{");
    expect(html).not.toContain("katex-error");
  });

  it("renders an unfinished ```math fence as a pending block", () => {
    const html = renderMarkdown("```math\nx_{i} + y_", { partial: true });
    expect(html).toContain('class="katex-block katex-pending"');
    expect(html).not.toContain("katex-error");
    expect(html).not.toContain("language-math");
  });

  it("renders an unfinished \\[ block as pending instead of raw text", () => {
    const html = renderMarkdown("\\[\ne^{i\\pi} +", { partial: true });
    expect(html).toContain('class="katex-block katex-pending"');
    expect(html).not.toContain("katex-error");
  });

  it("marks pending math nested in a still-open blockquote", () => {
    expect(renderMarkdown("> $$\n> x_{", { partial: true })).toContain("katex-pending");
    expect(renderMarkdown("> ```math\n> x_{", { partial: true })).toContain("katex-pending");
  });

  it("keeps completed-but-invalid math an error even in partial output", () => {
    // Delimiter closure decides, not KaTeX success: a closed block is final.
    const html = renderMarkdown("$$\\badcommand{$$\n\n$$\nx_{", { partial: true });
    expect(html).toContain("katex-error");
    expect(html).toContain("katex-pending");
  });

  it("leaves currency, code spans and code fences alone in partial output", () => {
    const dollars = renderMarkdown("The price is $5 and $10 total.", { partial: true });
    expect(dollars).toContain("$5");
    expect(dollars).not.toContain("katex");

    const span = renderMarkdown("`$$x` in a code span", { partial: true });
    expect(span).toContain("<code>$$x</code>");
    expect(span).not.toContain("katex");

    const code = renderMarkdown("```js\n$$x", { partial: true });
    expect(code).toContain("$$x");
    expect(code).not.toContain("katex-pending");
  });

  it("handles ~~~math fences, open and closed", () => {
    expect(renderMarkdown("~~~math\nx_{", { partial: true })).toContain("katex-pending");
    const closed = renderMarkdown("~~~math\nx_i\n~~~", { partial: true });
    expect(closed).toContain('class="katex-block"');
    expect(closed).not.toContain("katex-pending");
  });

  it("recognises closing fences and $$ inside (nested) list items", () => {
    for (const src of ["- ```math\n  x_i\n  ```", "- a\n  - ```math\n    x_i\n    ```", "- $$\n  x_i\n  $$"]) {
      const html = renderMarkdown(src, { partial: true });
      expect(html, src).toContain('class="katex-block"');
      expect(html, src).not.toContain("katex-pending");
    }
    expect(renderMarkdown("- a\n  - ```math\n    x_{", { partial: true })).toContain("katex-pending");
    expect(renderMarkdown("- $$\n  x_{", { partial: true })).toContain("katex-pending");
  });

  it("treats CRLF input like LF", () => {
    const crlf = fixture.replace(/\n/g, "\r\n");
    expect(renderMarkdown(crlf, { partial: true })).toBe(renderMarkdown(fixture));
    expect(renderMarkdown("```math\r\nx_{\r\n", { partial: true })).toContain("katex-pending");
    expect(renderMarkdown("$$\r\nx_{", { partial: true })).toContain("katex-pending");
  });

  it("hides a closing delimiter that has only partly arrived", () => {
    const text = (src: string) =>
      new DOMParser().parseFromString(renderMarkdown(src, { partial: true }), "text/html").querySelector(".katex-pending")
        ?.textContent;
    expect(text("$$\nx_i\n$")).toBe("x_i");
    expect(text("\\[\nx_i \\")).toBe("x_i");
    expect(text("```math\nx_i\n``")).toBe("x_i");
    expect(text("\\[\nx \\\\")).toBe("x \\\\");
  });

  it("strips quote markers from pending and closed \\[ blocks and stops at the container", () => {
    const pending = renderMarkdown("> \\[\n> e^{i\\pi} +", { partial: true });
    expect(pending).toContain("katex-pending");
    expect(pending).not.toContain("&gt;");
    expect(renderMarkdown("> \\[\n> x_i\n> \\]", { partial: true })).not.toContain("katex-error");
    // More content after an unclosed \[ means it was a paragraph after all.
    const later = renderMarkdown("\\[\nx +\n\nNext paragraph", { partial: true });
    expect(later).toBe(renderMarkdown("\\[\nx +\n\nNext paragraph"));
  });

  it("only marks the document tail pending", () => {
    // An unclosed fence ended by its blockquote, with content after it, is final output.
    const html = renderMarkdown("> ```math\n> x_{\n\nafter", { partial: true });
    expect(html).toBe(renderMarkdown("> ```math\n> x_{\n\nafter"));
  });

  it("pins plugin 1.1.2 behaviour: unclosed $$ and ```math reach KaTeX without partial mode", () => {
    expect(renderMarkdown("$$\n\\sum_{i=1}^{N} C_{")).toContain("katex-error");
    expect(renderMarkdown("```math\nx_{")).toContain("katex-error");
    // An unclosed \[ falls back to paragraph text today; non-partial keeps that.
    const brackets = renderMarkdown("\\[\ne^{i\\pi} +");
    expect(brackets).toContain("e^{i\\pi}");
    expect(brackets).not.toContain("katex");
  });
});
