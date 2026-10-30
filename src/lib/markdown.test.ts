// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
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

  it("keeps KaTeX output free of active content", () => {
    const html = renderMarkdown("$\\href{javascript:alert(1)}{x}$ and $x$");
    const doc = new DOMParser().parseFromString(html, "text/html");
    expect(doc.querySelectorAll("a[href], script, iframe").length).toBe(0);
  });
});
