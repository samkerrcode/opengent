// One render path for streamed markdown (replies and Thinking blocks). Text arrives in
// bursts faster than the webview can lay out a long, math-heavy segment, so while a segment
// streams its renders are coalesced to at most one per animation frame, always of the
// newest text. Frame callbacks pause while the window is in the background, so a segment
// that stops streaming (deferred: false) renders synchronously and drops any queued frame.
import { renderMarkdown } from "./markdown";
import { applyHtml } from "./stream-cursor";

export type MarkdownViewOptions = {
  /** The source may end mid-construct; see renderMarkdown. */
  partial: boolean;
  /** Show the streaming caret. */
  caret?: boolean;
  /** Coalesce renders to animation frames (while streaming). */
  deferred?: boolean;
  /** Called after each render that reached the DOM. */
  onrender?: (() => void) | undefined;
};

type Request = { source: string; options: MarkdownViewOptions };

const latest = new WeakMap<HTMLElement, Request>();
const frames = new WeakMap<HTMLElement, number>();
// What each node currently shows, so a re-run with the same input (the attachment re-runs
// whenever anything it reads changes) costs nothing instead of a full render.
const shown = new WeakMap<HTMLElement, Request>();

const sameOutput = (a: Request | undefined, b: Request) =>
  a !== undefined && a.source === b.source && a.options.partial === b.options.partial && !!a.options.caret === !!b.options.caret;

function render(node: HTMLElement, request: Request): void {
  const { source, options } = request;
  applyHtml(node, renderMarkdown(source, { partial: options.partial }), options.caret === true);
  shown.set(node, request);
  options.onrender?.();
}

/** Svelte attachment: render `source` as markdown into the node. */
export function markdownView(source: string, options: MarkdownViewOptions) {
  return (node: HTMLElement) => {
    const request = { source, options };
    if (!options.deferred) {
      const frame = frames.get(node);
      if (frame !== undefined) cancelAnimationFrame(frame);
      frames.delete(node);
      latest.delete(node);
      if (!sameOutput(shown.get(node), request)) render(node, request);
      return;
    }
    if (!frames.has(node) && sameOutput(shown.get(node), request)) return;
    latest.set(node, request);
    if (frames.has(node)) return;
    frames.set(
      node,
      requestAnimationFrame(() => {
        frames.delete(node);
        const next = latest.get(node);
        latest.delete(node);
        if (next && node.isConnected) render(node, next);
      }),
    );
  };
}
