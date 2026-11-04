// Streaming caret placement for rendered markdown. The caret has to sit where the next token
// will appear: at the end of the deepest trailing text, not after the last top-level block. A
// CSS `::after` on the last block only works when that block holds inline content; on lists,
// tables, and blockquotes it lands below the block. So we walk the trailing edge of the
// rendered DOM (O(depth)) and insert a span there.

const CURSOR_CLASS = "stream-cursor";

// Elements whose content must not receive the caret: void elements and rendered math (KaTeX
// boxes are absolutely laid out; a caret inside would float in the formula). The caret goes
// right after them instead.
const ATOMIC = new Set(["BR", "HR", "IMG", "BUTTON", "SVG", "MATH"]);
const isAtomic = (el: Element) =>
  ATOMIC.has(el.tagName.toUpperCase()) || el.classList.contains("katex") || el.classList.contains("katex-block");

// Last child that renders something; markdown-it emits "\n" text nodes between blocks.
function lastMeaningful(node: Node): ChildNode | null {
  let child = node.lastChild;
  while (child) {
    if (child.nodeType === Node.ELEMENT_NODE || (child.nodeType === Node.TEXT_NODE && child.textContent?.trim())) {
      return child;
    }
    child = child.previousSibling;
  }
  return null;
}

export function placeStreamCursor(root: HTMLElement): void {
  const cursor = root.ownerDocument.createElement("span");
  cursor.className = CURSOR_CLASS;
  cursor.setAttribute("aria-hidden", "true");

  let parent: Node = root;
  for (;;) {
    const last = lastMeaningful(parent);
    if (!last) {
      parent.appendChild(cursor);
      return;
    }
    if (last.nodeType === Node.TEXT_NODE) {
      const text = last as Text;
      // Fenced code ends with "\n"; keep the caret on the last code line, not the next one.
      const trailing = /\n+$/.exec(text.data);
      const after = trailing ? text.splitText(trailing.index) : text.nextSibling;
      parent.insertBefore(cursor, after);
      return;
    }
    const el = last as Element;
    if (isAtomic(el)) {
      parent.insertBefore(cursor, el.nextSibling);
      return;
    }
    parent = el;
  }
}

/** Svelte attachment: render sanitized HTML into the node, with a caret while streaming. */
export function markdownHtml(html: string, streaming: boolean) {
  return (node: HTMLElement) => {
    node.innerHTML = html;
    if (streaming) placeStreamCursor(node);
  };
}
