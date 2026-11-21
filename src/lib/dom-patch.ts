// Incremental DOM update for streamed markdown. Replacing a whole rendered segment on
// every update rebuilds already-visible formulas, code blocks and their scroll offsets,
// which in WebKit can leave stale or missing paint while the reader scrolls (issue #37).
// Streaming only ever changes the tail, so keep the unchanged leading nodes and descend
// into the last changed one: a reasoning trace is often one long list, so a top-level
// diff alone would still rebuild almost everything. The result is always equal
// (isEqualNode) to `source`.

function sameShell(a: Element, b: Element): boolean {
  if (a.tagName !== b.tagName || a.attributes.length !== b.attributes.length) return false;
  for (const { name, value } of b.attributes) {
    if (a.getAttribute(name) !== value) return false;
  }
  return true;
}

/** Make `target`'s children equal to `source`'s, moving nodes out of `source` as needed. */
export function patchChildren(target: Node, source: Node): void {
  const cur = target.childNodes;
  const next = source.childNodes;
  let i = 0;
  while (i < cur.length && i < next.length && cur[i]!.isEqualNode(next[i]!)) i++;

  // One old node left over (plus the whitespace markdown-it puts between blocks): the
  // growing tail. Update it in place when it is the same kind of node; anything else (a
  // block changing type, a paragraph turning into a list) falls through to replacement.
  if (i < cur.length && i < next.length && onlyWhitespaceAfter(cur, i)) {
    const a = cur[i]!;
    const b = next[i]!;
    if (a.nodeType === Node.TEXT_NODE && b.nodeType === Node.TEXT_NODE) {
      (a as Text).data = (b as Text).data;
      i++;
    } else if (a.nodeType === Node.ELEMENT_NODE && b.nodeType === Node.ELEMENT_NODE && sameShell(a as Element, b as Element)) {
      patchChildren(a, b);
      i++;
    }
  }

  while (cur.length > i) target.lastChild!.remove();
  if (i < next.length) target.appendChild(nodesFrom(next, i));
}

function onlyWhitespaceAfter(list: NodeListOf<ChildNode>, index: number): boolean {
  for (let j = index + 1; j < list.length; j++) {
    const n = list[j]!;
    if (n.nodeType !== Node.TEXT_NODE || n.textContent!.trim()) return false;
  }
  return true;
}

function nodesFrom(list: NodeListOf<ChildNode>, start: number): DocumentFragment {
  const frag = (list[0]!.ownerDocument ?? document).createDocumentFragment();
  // `list` is live: appending to the fragment removes each node from it.
  while (list.length > start) frag.appendChild(list[start]!);
  return frag;
}
