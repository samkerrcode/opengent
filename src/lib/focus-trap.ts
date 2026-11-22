// Modal focus handling as a Svelte action: Tab and Shift+Tab cycle within the dialog, and
// focus returns to whatever had it before the dialog opened once the dialog goes away.

const FOCUSABLE = "a[href], button, input, select, textarea, [tabindex]";

function focusables(root: HTMLElement): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
    (el) => el.tabIndex >= 0 && !(el as HTMLButtonElement).disabled && !el.closest("[hidden], [inert]"),
  );
}

export function trapFocus(node: HTMLElement) {
  const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  if (!node.contains(document.activeElement)) node.focus();

  function onKeydown(e: KeyboardEvent) {
    if (e.key !== "Tab") return;
    const items = focusables(node);
    if (!items.length) {
      e.preventDefault();
      node.focus();
      return;
    }
    const first = items[0]!;
    const last = items[items.length - 1]!;
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

  node.addEventListener("keydown", onKeydown);
  return {
    destroy() {
      node.removeEventListener("keydown", onKeydown);
      // Another dialog may have taken focus already (e.g. Settings → Providers).
      const active = document.activeElement;
      if (previous?.isConnected && (!active || active === document.body || node.contains(active))) previous.focus();
    },
  };
}
