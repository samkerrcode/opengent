// Markdown → sanitized HTML. Raw HTML in the source is disabled at the parser level and the
// output is sanitized again with DOMPurify as defense in depth. Links are forced to open via the
// host (see link interception in App.svelte); the webview itself never navigates.
//
// Math: KaTeX renders `$...$` / `$$...$$` (via @vscode/markdown-it-katex), ```math fenced
// blocks, and `\(...\)` / `\[...\]` delimiters (custom rules below, reusing the plugin's
// renderers). KaTeX output needs MathML + SVG + inline style attributes, so the sanitizer
// profile allows them; raw HTML still can't reach the output because the parser escapes it.
import "katex/dist/katex.min.css";
import MarkdownIt, { type MarkdownIt as Md } from "markdown-it";
import markdownItKatexPkg from "@vscode/markdown-it-katex";
import DOMPurify from "dompurify";
import hljs from "./hljs";

// The plugin is CJS exporting `{ default: fn }`; bundler interop differs on whether the
// default import unwraps it (vite dev does not, vitest does), so handle both shapes.
const markdownItKatex =
  typeof markdownItKatexPkg === "function"
    ? markdownItKatexPkg
    : (markdownItKatexPkg as { default: (md: Md, options?: unknown) => void }).default;

const md = new MarkdownIt({
  html: false,
  linkify: true,
  breaks: false,
  typographer: false,
});

// Non-empty return goes into <pre><code class="hljs"> as markup; hljs escapes the source
// itself. Unknown/empty language falls back to escaped plain text (no auto-detect — it
// guesses wrong often enough to be worse than none).
md.options.highlight = (code, lang) => {
  if (lang && hljs.getLanguage(lang)) {
    try {
      return hljs.highlight(code, { language: lang, ignoreIllegals: true }).value;
    } catch {
      // fall through to escaped plain text
    }
  }
  return md.utils.escapeHtml(code);
};

// The button is intentionally empty: its icon is drawn via a CSS -webkit-mask data-URI so it
// follows currentColor and can't be mangled by the sanitizer (DOMPurify strips nearly all SVG
// geometry attributes). The click is handled by delegated listeners in the views.
const codeCopyButton = `<button class="code-copy" type="button" title="Copy code" aria-label="Copy code"></button>`;

// Wrap fenced code in a block with a header strip: language label on the left, copy button
// on the right. (```math fences never reach here — the katex plugin intercepts them.)
const defaultFence = md.renderer.rules.fence!;
md.renderer.rules.fence = (tokens, idx, options, env, self) => {
  const token = tokens[idx]!;
  const lang = token.info.trim().split(/\s+/)[0] ?? "";
  const html = defaultFence(tokens, idx, options, env, self);
  const label = lang ? `<span class="code-lang">${md.utils.escapeHtml(lang)}</span>` : "<span></span>";
  return `<div class="code-block"><div class="code-head">${label}${codeCopyButton}</div>${html}</div>`;
};

// Registered after our fence wrapper: it intercepts ```math fences itself and delegates all
// other fences to the wrapper above, so non-math code keeps its language label.
md.use(markdownItKatex, { enableFencedBlocks: true });

// \(...\) inline math. The plugin only handles $...$; models also emit \(...\) / \[...\].
// These rules emit the plugin's math_inline / math_block token types to reuse its renderers.
md.inline.ruler.before("escape", "math_inline_paren", (state, silent) => {
  if (!state.src.startsWith("\\(", state.pos)) return false;
  const close = state.src.indexOf("\\)", state.pos + 2);
  if (close === -1 || close === state.pos + 2) return false;
  if (!silent) {
    const token = state.push("math_inline", "math", 0);
    token.markup = "\\(\\)";
    token.content = state.src.slice(state.pos + 2, close);
  }
  state.pos = close + 2;
  return true;
});

// \[...\] display math, single- or multi-line, as its own block.
md.block.ruler.before("fence", "math_block_bracket", (state, startLine, endLine, silent) => {
  const start = state.bMarks[startLine]! + state.tShift[startLine]!;
  if (!state.src.startsWith("\\[", start)) return false;

  let close = -1;
  let closeLine = -1;
  for (let l = startLine; l < endLine; l++) {
    const from = l === startLine ? start + 2 : state.bMarks[l]!;
    const idx = state.src.indexOf("\\]", from);
    if (idx === -1) break;
    if (idx + 2 <= state.eMarks[l]!) {
      close = idx;
      closeLine = l;
      break;
    }
  }
  if (close === -1) return false;

  if (!silent) {
    const token = state.push("math_block", "math", 0);
    token.block = true;
    token.markup = "\\[\\]";
    token.content = state.src.slice(start + 2, close);
    token.map = [startLine, closeLine + 1];
  }
  state.line = closeLine + 1;
  return true;
});

DOMPurify.setConfig({
  ALLOWED_URI_REGEXP: /^(?:https?|mailto):/i,
  USE_PROFILES: { html: true, mathMl: true, svg: true },
  // KaTeX wraps MathML in <semantics> with the raw TeX in <annotation>; both are treated as
  // disallowed by the mathMl profile, so re-allow them (and friends) explicitly.
  ADD_TAGS: ["semantics", "annotation", "annotation-xml", "mprescripts", "none"],
  ADD_ATTR: ["encoding"],
  FORBID_TAGS: ["style", "iframe", "object", "embed", "form", "input"],
  FORBID_ATTR: ["onerror", "onload"],
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
