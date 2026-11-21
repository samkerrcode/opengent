// Keeps a scroll container pinned to its bottom while content streams in, and lets the
// reader take over at once. Used by the chat scroller and by a streaming Thinking block.
//
// Unpinning follows user intent: a wheel/trackpad flick up, an upward key, a press on a
// classic scrollbar, or any scroll event that moves up away from the bottom. Wheel, key and
// pointer events fire with the gesture, before the next streamed update can snap the view
// back and swallow the resulting scroll event. Re-pinning stays position-based (back within
// `threshold` px of the bottom), so following resumes when the reader returns there. While
// unpinned the helper never writes scrollTop.

const UP_KEYS = new Set(["ArrowUp", "PageUp", "Home"]);

export class ScrollPin {
  pinned = $state(true);

  #el: HTMLElement | null = null;
  #threshold: number;
  // Set on programmatic scrolls so the resulting scroll events are not mistaken for the user
  // scrolling away (content growth between the write and the scroll event used to unpin).
  #expectedTop: number | null = null;
  #lastTop = 0;
  // A smooth jumpToBottom scrolls down through the unpinned zone; don't unpin mid-flight.
  #jumping = false;

  constructor({ threshold = 48 }: { threshold?: number } = {}) {
    this.#threshold = threshold;
  }

  /** Start tracking `el`; returns the teardown. */
  attach(el: HTMLElement): () => void {
    this.#el = el;
    this.#lastTop = el.scrollTop;
    const onScroll = () => this.#onScroll();
    const onWheel = (e: WheelEvent) => this.#onWheel(e);
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t?.isContentEditable || t?.closest("input, textarea, select")) return;
      if (UP_KEYS.has(e.key) || (e.key === " " && e.shiftKey)) this.#unpin();
    };
    const onPointer = (e: PointerEvent) => {
      // A press on a classic (space-taking) scrollbar, typical on Windows. Overlay scrollbars
      // can't be hit-tested; their drags still unpin through the scroll event.
      if (e.target === el && e.offsetX >= el.clientWidth) this.#unpin();
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    el.addEventListener("wheel", onWheel, { passive: true });
    el.addEventListener("keydown", onKey);
    el.addEventListener("pointerdown", onPointer);
    return () => {
      el.removeEventListener("scroll", onScroll);
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("keydown", onKey);
      el.removeEventListener("pointerdown", onPointer);
      if (this.#el === el) this.#el = null;
    };
  }

  /** Re-pin (explicit intent to follow, e.g. a sent message or a chat switch). */
  pin(): void {
    this.pinned = true;
  }

  /** Scroll to the bottom if pinned; call after each rendered update. */
  follow(): void {
    if (this.pinned) this.scrollToBottom();
  }

  scrollToBottom(): void {
    const el = this.#el;
    if (!el) return;
    this.#expectedTop = Math.max(0, el.scrollHeight - el.clientHeight);
    el.scrollTop = this.#expectedTop;
  }

  /** Re-pin and scroll smoothly to the bottom. */
  jumpToBottom(): void {
    this.pinned = true;
    this.#jumping = true;
    this.#el?.scrollTo({ top: this.#el.scrollHeight, behavior: "smooth" });
  }

  #unpin(): void {
    const el = this.#el;
    if (!el || el.scrollHeight <= el.clientHeight) return;
    this.#jumping = false;
    this.#expectedTop = null;
    this.pinned = false;
  }

  #onScroll(): void {
    const el = this.#el;
    if (!el) return;
    const top = el.scrollTop;
    const dist = el.scrollHeight - top - el.clientHeight;
    // Content shrinking (e.g. a Thinking block collapsing) clamps scrollTop down but leaves
    // the view at the bottom; only a decrease that leaves the bottom is the user.
    const scrolledUp = top < this.#lastTop - 1 && dist > 2;
    this.#lastTop = top;
    const nearBottom = dist < this.#threshold;
    if (!scrolledUp && this.#expectedTop !== null && Math.abs(top - this.#expectedTop) < 4) return;
    this.#expectedTop = null;
    if (scrolledUp) this.#jumping = false;
    else if (this.#jumping) {
      if (!nearBottom) return;
      this.#jumping = false;
    }
    this.pinned = !scrolledUp && nearBottom;
  }

  #onWheel(e: WheelEvent): void {
    const el = this.#el;
    if (!el || e.deltaY >= 0 || Math.abs(e.deltaX) > Math.abs(e.deltaY)) return;
    // Stay pinned when a nested scroller (e.g. a Thinking block inside the chat) consumes it.
    for (let n = e.target as HTMLElement | null; n && n !== el; n = n.parentElement) {
      if (n.scrollTop > 0 && n.scrollHeight > n.clientHeight && /auto|scroll/.test(getComputedStyle(n).overflowY)) return;
    }
    this.#unpin();
  }
}
