// Modal focus handling as a Svelte action: Tab and Shift+Tab cycle within the dialog, and
// focus returns to whatever had it before the dialog opened once the dialog goes away.

const FOCUSABLE = "a[href], button, input, select, textarea, summary, [tabindex]";

function tabbable(el: HTMLElement, root: HTMLElement): boolean {
  if (el.tabIndex < 0 || (el as HTMLButtonElement).disabled) return false;
  if (el.closest("[hidden], [inert], details:not([open]) > :not(summary)")) return false;
  if (getComputedStyle(el).visibility === "hidden") return false;
  for (let n: HTMLElement | null = el; n && n !== root; n = n.parentElement) {
    if (getComputedStyle(n).display === "none") return false;
  }
  return true;
}

/** The first and last tabbable controls, checking from each end only as far as needed. */
function ends(root: HTMLElement): [HTMLElement, HTMLElement] | null {
  const all = root.querySelectorAll<HTMLElement>(FOCUSABLE);
  let i = 0;
  while (i < all.length && !tabbable(all[i]!, root)) i++;
  if (i === all.length) return null;
  let j = all.length - 1;
  while (j > i && !tabbable(all[j]!, root)) j--;
  return [all[i]!, all[j]!];
}

export function trapFocus(node: HTMLElement) {
  const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  if (!node.contains(document.activeElement)) node.focus();

  function onKeydown(e: KeyboardEvent) {
    if (e.key !== "Tab") return;
    const found = ends(node);
    if (!found) {
      e.preventDefault();
      node.focus();
      return;
    }
    const [first, last] = found;
    const active = document.activeElement;
    const inside = node.contains(active) && active !== node;
    if (e.shiftKey && (!inside || active === first)) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && (!inside || active === last)) {
      e.preventDefault();
      first.focus();
    }
  }

  // On the document, not the dialog: focus falls to <body> when the focused control is
  // removed or disabled, and Tab from there must come back in rather than reach the page.
  document.addEventListener("keydown", onKeydown);
  return {
    destroy() {
      document.removeEventListener("keydown", onKeydown);
      // Another dialog may have taken focus already (e.g. Settings → Providers).
      const active = document.activeElement;
      if (previous?.isConnected && (!active || active === document.body || node.contains(active))) previous.focus();
    },
  };
}
