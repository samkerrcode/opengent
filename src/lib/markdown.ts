// Markdown → sanitized HTML. Raw HTML in the source is disabled at the parser level and the
// output is sanitized again with DOMPurify as defense in depth. Links are forced to open via the
// host (see link interception in App.svelte); the webview itself never navigates.
//
// Math: KaTeX renders `$...$` / `$$...$$` (via @vscode/markdown-it-katex), ```math fenced
// blocks, and `\(...\)` / `\[...\]` delimiters (custom rules below, reusing the plugin's
// renderers). KaTeX output needs MathML + SVG + inline style attributes, so the sanitizer
// profile allows them; raw HTML still can't reach the output because the parser escapes it.
import "katex/dist/katex.min.css";
import MarkdownIt, { type MarkdownIt as Md, type StateBlock, type Token } from "markdown-it";
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

// --- Pending math in partial output ---------------------------------------------------
// While a run streams (or after it was stopped or cut off), the document can end inside
// display math whose closing delimiter never arrived. The plugin emits a math token
// anyway (an unclosed $$ block or ```math fence runs to the end of input), so KaTeX
// would render a red parse error for a *prefix* of the formula. With `env.partial` the
// unfinished construct at the tail is marked pending and rendered as muted source
// inside the same .katex-block container, swapping to the rendered formula once its
// delimiter arrives. Closure is decided inside the block rules, where container
// markers and indentation are already resolved.

const isPartial = (state: StateBlock) => state.env?.["partial"] === true;

// Nothing but whitespace follows `line` (exclusive end) in the whole document.
const isTail = (state: StateBlock, line: number) => state.src.slice(state.eMarks[line - 1]!).trim() === "";

// Lines [first, last] of a delimited block with container markers stripped: the first
// line from `from`, the last one up to `to`.
function blockLines(state: StateBlock, first: number, from: number, last: number, to: number): string {
  if (first === last) return state.src.slice(from, to);
  return (
    state.src.slice(from, state.eMarks[first]!) +
    "\n" +
    state.getLines(first + 1, last, state.blkIndent, true) +
    state.src.slice(state.bMarks[last]! + state.tShift[last]!, to)
  );
}

// Replace a block rule with `wrap(original)`, keeping its position and alt chains.
type RuleBlock = Parameters<typeof md.block.ruler.at>[1];
function wrapBlockRule(name: string, wrap: (rule: RuleBlock) => RuleBlock) {
  const rule = md.block.ruler.__rules__[md.block.ruler.__find__(name)]!;
  md.block.ruler.at(name, wrap(rule.fn), { alt: rule.alt });
}

// \[...\] display math, single- or multi-line, as its own block. With `env.partial`, an
// unterminated \[ at the tail is emitted as pending math instead of falling through to
// raw paragraph text; it ends where a paragraph would (blank line or dedent), so once
// more content arrives after it, it renders as the paragraph the final output shows.
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
  if (close === -1) {
    if (!isPartial(state)) return false;
    let lastLine = startLine;
    while (lastLine + 1 < endLine && !state.isEmpty(lastLine + 1) && state.sCount[lastLine + 1]! >= state.blkIndent) {
      lastLine++;
    }
    if (!isTail(state, lastLine + 1)) return false;
    if (!silent) {
      const token = state.push("math_block", "math", 0);
      token.block = true;
      token.markup = "\\[\\]";
      token.content = blockLines(state, startLine, start + 2, lastLine, state.eMarks[lastLine]!);
      token.map = [startLine, lastLine + 1];
      token.meta = { pending: true };
    }
    state.line = lastLine + 1;
    return true;
  }

  if (!silent) {
    const token = state.push("math_block", "math", 0);
    token.block = true;
    token.markup = "\\[\\]";
    token.content = blockLines(state, startLine, start + 2, closeLine, close);
    token.map = [startLine, closeLine + 1];
  }
  state.line = closeLine + 1;
  return true;
});

// The plugin's $$ block rule: when it finds no closing $$ it stops past the last line
// (`state.line > endLine`); a found close or a dedent leaves `state.line <= endLine`.
wrapBlockRule("math_block", (rule) => (state, startLine, endLine, silent) => {
  if (!rule(state, startLine, endLine, silent)) return false;
  if (!silent && isPartial(state) && state.line > endLine && isTail(state, endLine)) {
    const token = state.tokens[state.tokens.length - 1]!;
    if (token.type === "math_block") token.meta = { pending: true };
  }
  return true;
});

// markdown-it's fence rule: an unclosed fence runs to `endLine`. A fence closed on the
// container's last line also ends there, so check that line the way the rule does: the
// opener's marker char, at least as long, under 4 columns of relative indent.
wrapBlockRule("fence", (rule) => (state, startLine, endLine, silent) => {
  if (!rule(state, startLine, endLine, silent)) return false;
  if (silent || !isPartial(state) || state.line < endLine || !isTail(state, endLine)) return true;
  const token = state.tokens[state.tokens.length - 1]!;
  if (token.type !== "fence" || token.info.trim().toLowerCase() !== "math") return true;
  const last = endLine - 1;
  const text = state.src.slice(state.bMarks[last]! + state.tShift[last]!, state.eMarks[last]!).trimEnd();
  const marker = token.markup[0]!;
  const closed =
    last > startLine &&
    state.sCount[last]! - state.blkIndent < 4 &&
    text.length >= token.markup.length &&
    [...text].every((c) => c === marker);
  if (!closed) token.meta = { pending: true };
  return true;
});

// Drop a closing delimiter that has only partly arrived (a lone `$` of `$$`, the `\` of
// `\]`, or a short run of fence markers on the last line), so it doesn't flash.
function withoutPartialCloser(token: Token): string {
  const s = token.content.replace(/\s+$/, "");
  if (token.markup === "$$") return s.endsWith("$") && !s.endsWith("\\$") ? s.slice(0, -1) : s;
  if (token.markup === "\\[\\]") return (/\\+$/.exec(s)?.[0].length ?? 0) % 2 === 1 ? s.slice(0, -1) : s;
  const nl = s.lastIndexOf("\n");
  const lastLine = s.slice(nl + 1).trim();
  return lastLine && [...lastLine].every((c) => c === token.markup[0]) ? s.slice(0, Math.max(nl, 0)) : s;
}

// Escaped source in the same .katex-block wrapper as a rendered formula, so the swap
// keeps identical margins. Renderers key off token.meta.pending (partial output only).
const pendingMathHtml = (token: Token) =>
  `<p class="katex-block katex-pending">${md.utils.escapeHtml(withoutPartialCloser(token).trim())}</p>\n`;

// --- KaTeX output cache ---------------------------------------------------------------
// A streaming segment is re-rendered on every update, so without a cache every finished
// formula is laid out by KaTeX again each time. The plugin's output is a pure function of
// the TeX source (inline vs. display is part of the key), so cached HTML is identical to a
// fresh render; it is still sanitized with the rest of the document. Bounded by size, not
// entry count: each render visits formulas in document order, so an LRU smaller than one
// long trace would evict every entry just before it is needed again.
const KATEX_CACHE_MAX_CHARS = 4 * 1024 * 1024;
const katexCache = new Map<string, string>();
let katexCacheChars = 0;

function cachedKatex(key: string, render: () => string): string {
  const hit = katexCache.get(key);
  if (hit !== undefined) {
    katexCache.delete(key);
    katexCache.set(key, hit);
    return hit;
  }
  const html = render();
  katexCache.set(key, html);
  katexCacheChars += key.length + html.length;
  for (const [k, v] of katexCache) {
    if (katexCacheChars <= KATEX_CACHE_MAX_CHARS) break;
    katexCache.delete(k);
    katexCacheChars -= k.length + v.length;
  }
  return html;
}

// Display math from $$, \[ and inline $$ shares one key space: the plugin renders all of
// them through the same block renderer.
for (const [rule, kind] of [
  ["math_inline", "inline"],
  ["math_inline_block", "block"],
] as const) {
  const plugin = md.renderer.rules[rule]!;
  md.renderer.rules[rule] = (tokens, idx, options, env, self) =>
    cachedKatex(`${kind}:${tokens[idx]!.content}`, () => plugin(tokens, idx, options, env, self));
}

const pluginMathBlock = md.renderer.rules.math_block!;
md.renderer.rules.math_block = (tokens, idx, options, env, self) => {
  const token = tokens[idx]!;
  if (token.meta?.["pending"]) return pendingMathHtml(token);
  return cachedKatex(`block:${token.content}`, () => pluginMathBlock(tokens, idx, options, env, self));
};

const pluginFence = md.renderer.rules.fence!;
md.renderer.rules.fence = (tokens, idx, options, env, self) => {
  const token = tokens[idx]!;
  if (token.meta?.["pending"]) return pendingMathHtml(token);
  if (token.info.trim().toLowerCase() !== "math") return pluginFence(tokens, idx, options, env, self);
  return cachedKatex(`block:${token.content}`, () => pluginFence(tokens, idx, options, env, self));
};

// Links and images may only point at http(s)/mailto. This is enforced in a hook on
// URL-bearing attributes rather than via ALLOWED_URI_REGEXP: DOMPurify tests that regexp
// against every attribute outside its small URI-safe list, so a strict one also strips
// KaTeX's SVG geometry (viewBox, width, height, path d) and √ and other stretchy symbols
// render empty. DOMPurify's default URI check (no javascript: etc.) still applies.
const SAFE_URL = /^(?:https?|mailto):/i;
const URL_ATTRS = new Set(["href", "xlink:href", "src", "srcset", "action", "formaction", "poster", "background", "cite", "ping"]);
DOMPurify.addHook("uponSanitizeAttribute", (_node, data) => {
  if (URL_ATTRS.has(data.attrName) && !SAFE_URL.test(data.attrValue.trim())) data.keepAttr = false;
});

DOMPurify.setConfig({
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

/**
 * `partial`: the source may end mid-construct (still streaming, or the run was stopped or
 * cut off), so unclosed display math at the tail renders as pending source, not an error.
 */
export function renderMarkdown(source: string, options?: { partial?: boolean }): string {
  return DOMPurify.sanitize(md.render(source, { partial: options?.partial === true }));
}
