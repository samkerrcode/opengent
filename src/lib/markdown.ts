// Markdown → sanitized HTML. Raw HTML in the source is disabled at the parser level and the
// output is sanitized again with DOMPurify as defense in depth. Links are forced to open via the
// host (see link interception in App.svelte); the webview itself never navigates.
import MarkdownIt from "markdown-it";
import DOMPurify from "dompurify";

const md = new MarkdownIt({
  html: false,
  linkify: true,
  breaks: false,
  typographer: false,
});

// Add a language class and a copy-friendly wrapper around fenced code.
const defaultFence = md.renderer.rules.fence!;
md.renderer.rules.fence = (tokens, idx, options, env, self) => {
  const token = tokens[idx]!;
  const lang = token.info.trim().split(/\s+/)[0] ?? "";
  const html = defaultFence(tokens, idx, options, env, self);
  const label = lang ? `<span class="code-lang">${md.utils.escapeHtml(lang)}</span>` : "";
  return `<div class="code-block">${label}${html}</div>`;
};

DOMPurify.setConfig({
  ALLOWED_URI_REGEXP: /^(?:https?|mailto):/i,
  FORBID_TAGS: ["style", "iframe", "object", "embed", "form", "input", "svg", "math"],
  FORBID_ATTR: ["style", "onerror", "onload"],
});
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A") {
    node.setAttribute("rel", "noopener noreferrer");
    node.setAttribute("target", "_blank");
  }
});

export function renderMarkdown(source: string): string {
  return DOMPurify.sanitize(md.render(source));
}
