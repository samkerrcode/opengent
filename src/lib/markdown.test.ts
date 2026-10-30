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
});
