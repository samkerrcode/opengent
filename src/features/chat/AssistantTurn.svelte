<script lang="ts">
  import type { AssistantTurn, TurnSegment } from "$lib/state/run-reducer";
  import { isTerminal, turnText, turnThinking } from "$lib/state/run-reducer";
  import { renderMarkdown } from "$lib/markdown";
  import { copyText } from "$lib/clipboard";
  import ToolCallCard from "./ToolCallCard.svelte";

  let { turn, model, onretry }: { turn: AssistantTurn; model: string; onretry: () => void } = $props();

  const phaseLabel: Record<AssistantTurn["phase"], string> = {
    queued: "Queued",
    preparing: "Preparing",
    generating: "Generating",
    awaiting_approval: "Awaiting approval",
    calling_tools: "Calling tool",
    completed: "Done",
    cancelled: "Stopped",
    failed: "Failed",
  };

  const live = $derived(!isTerminal(turn.phase));
  const fullText = $derived(turnText(turn));
  const thinking = $derived(turnThinking(turn));
  // Thinking is done as soon as the model moves on to text or a tool call — collapsing
  // then, instead of at run completion, keeps the response in view while it streams.
  const thinkingLive = $derived(live && turn.segments.at(-1)?.kind === "thinking");
  let thinkingEl = $state<HTMLElement>();
  // The body is height-capped and scrolls internally; follow the tail while thinking
  // streams, and reset to the top once it collapses so a reopened trace reads from
  // the start.
  $effect(() => {
    const el = thinkingEl;
    void thinking.length;
    if (!el) return;
    el.scrollTop = thinkingLive ? el.scrollHeight : 0;
  });
  const truncated = $derived(turn.stopReason === "max_tokens");
  const usageText = $derived.by(() => {
    const u = turn.usage;
    if (!u) return null;
    const parts = [];
    if (u.input_tokens != null) parts.push(`${u.input_tokens} in`);
    if (u.output_tokens != null) parts.push(`${u.output_tokens} out`);
    if (u.cache_read_tokens) parts.push(`${u.cache_read_tokens} cached`);
    return parts.join(" · ");
  });

  let copied = $state(false);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  // Copies the raw markdown source of the response (the streamed text is markdown already).
  async function copyResponse() {
    if (await copyText(fullText)) {
      copied = true;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => (copied = false), 1400);
    }
  }

  // Code-block copy buttons live inside {@html} output, so handle their clicks with one
  // delegated listener on the article (imperative, to avoid an a11y warning on a div).
  function onCopyClick(e: MouseEvent) {
    const btn = (e.target as HTMLElement | null)?.closest<HTMLButtonElement>("button.code-copy");
    if (!btn) return;
    const code = btn.closest(".code-block")?.querySelector("pre code")?.textContent ?? "";
    void copyText(code).then((ok) => {
      if (!ok) return;
      btn.classList.add("copied");
      setTimeout(() => btn.classList.remove("copied"), 1400);
    });
  }

  let articleEl = $state<HTMLElement>();
  $effect(() => {
    const el = articleEl;
    if (!el) return;
    el.addEventListener("click", onCopyClick);
    return () => el.removeEventListener("click", onCopyClick);
  });
</script>

<article class="assistant" aria-label="Assistant" bind:this={articleEl}>
  <div class="avatar" class:live aria-hidden="true"></div>

  <div class="content">
    <header class="rail" aria-live="polite">
      <span class="phase" class:live data-phase={turn.phase}>{phaseLabel[turn.phase]}</span>
      {#if model}<span class="meta">{model}</span>{/if}
      {#if usageText}<span class="meta">{usageText}</span>{/if}
      {#if turn.desynced}<span class="meta warn" title="Some events were missed">desynced</span>{/if}
    </header>

    {#each turn.notices as notice (notice)}
      <p class="notice">{notice}</p>
    {/each}
    {#if truncated}<p class="notice">Response was cut off at the output token limit.</p>{/if}

    {#if thinking}
      <details class="thinking-block" open={thinkingLive}>
        <summary>{thinkingLive ? "Thinking…" : "Thinking"}</summary>
        <div class="markdown thinking-body" bind:this={thinkingEl}>{@html renderMarkdown(thinking)}</div>
      </details>
    {/if}

    <div class="body selectable">
      {#each turn.segments as seg, i (i)}
        {#if seg.kind === "text"}
          <div class="markdown" class:streaming={live && i === turn.segments.length - 1}>{@html renderMarkdown(seg.text)}</div>
        {:else if seg.kind === "tool"}
          <ToolCallCard segment={seg} runId={turn.runId} />
        {/if}
      {/each}
      {#if live && !fullText && turn.segments.every((s) => s.kind !== "tool")}
        <div class="thinking" aria-hidden="true"><span></span><span></span><span></span></div>
      {/if}
      {#if turn.error}
        <div class="error" role="alert">
          <p>{turn.error}</p>
          <button class="btn-outline btn-sm" onclick={onretry}>Try again</button>
        </div>
      {:else if turn.phase === "cancelled"}
        <div class="stopped">
          <span>Stopped.</span>
          <button class="btn-ghost btn-sm" onclick={onretry}>Regenerate</button>
        </div>
      {/if}
    </div>

    {#if fullText}
      <div class="actions">
        <button
          class="icon-btn"
          class:copied
          onclick={copyResponse}
          title={copied ? "Copied" : "Copy response"}
          aria-label="Copy response"
        >
          {#if copied}
            <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M2.5 8.5 6 12 13.5 4"/></svg>
          {:else}
            <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 5.5v-2A1.5 1.5 0 0 0 9 2H3.5A1.5 1.5 0 0 0 2 3.5V9a1.5 1.5 0 0 0 1.5 1.5h2"/></svg>
          {/if}
        </button>
      </div>
    {/if}
  </div>
</article>

<style>
  .assistant {
    display: grid;
    grid-template-columns: 28px 1fr;
    gap: var(--space-3);
    align-items: start;
  }
  .avatar {
    width: 28px;
    height: 28px;
    border-radius: 50%;
    background: conic-gradient(from 200deg, var(--accent), var(--accent-soft) 70%);
    margin-top: 2px;
  }
  .avatar.live {
    animation: spin 2.4s linear infinite;
  }
  .content {
    min-width: 0;
    display: grid;
    gap: var(--space-2);
  }
  .rail {
    display: flex;
    gap: var(--space-2);
    align-items: center;
    font-size: 12px;
    color: var(--fg-faint);
    min-height: 22px;
  }
  .phase {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--bg-sunken);
    color: var(--fg-muted);
  }
  .phase.live::before {
    content: "";
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    animation: pulse 1.2s ease-in-out infinite;
  }
  .phase[data-phase="failed"] {
    color: var(--danger);
    background: var(--danger-soft);
  }
  .phase[data-phase="cancelled"] {
    color: var(--warning);
  }
  .meta::before {
    content: "·";
    margin-right: var(--space-2);
  }
  .warn {
    color: var(--warning);
  }
  .notice {
    margin: 0;
    font-size: 12.5px;
    color: var(--fg-muted);
    padding: 6px 10px;
    border-left: 2px solid var(--warning);
    background: var(--bg-sunken);
    border-radius: 0 var(--radius-sm) var(--radius-sm) 0;
  }
  .body {
    font-size: 15px;
    line-height: 1.65;
    min-width: 0;
  }
  /* Long unbroken tokens (URLs, identifiers) must wrap instead of overflowing. */
  .markdown {
    overflow-wrap: break-word;
    word-break: break-word;
    min-width: 0;
  }
  /* Streaming cursor: rendered inside the last block element so it sits at the end of the
     last line instead of wrapping to a fresh line below the block. */
  .markdown.streaming > :global(:last-child:not(.code-block))::after,
  .markdown.streaming > :global(.code-block:last-child > pre)::after {
    content: "";
    display: inline-block;
    width: 8px;
    height: 1em;
    background: var(--accent);
    border-radius: 2px;
    animation: pulse 1s ease-in-out infinite;
    vertical-align: -0.12em;
    margin-left: 2px;
  }
  .thinking {
    display: inline-flex;
    gap: 5px;
    padding: 8px 0;
  }
  .thinking span {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--fg-faint);
    animation: pulse 1.2s ease-in-out infinite;
  }
  .thinking span:nth-child(2) {
    animation-delay: 0.2s;
  }
  .thinking span:nth-child(3) {
    animation-delay: 0.4s;
  }
  .error {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: 10px 12px;
    border-radius: var(--radius-md);
    background: var(--danger-soft);
    color: var(--danger);
    font-size: 13.5px;
    margin-top: var(--space-2);
  }
  .error p {
    margin: 0;
    flex: 1;
  }
  .stopped {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--fg-faint);
    font-size: 13px;
    margin-top: var(--space-2);
  }
  .thinking-block {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-sunken);
    font-size: 13px;
  }
  .thinking-block > summary {
    padding: 6px 10px;
    color: var(--fg-faint);
    cursor: default;
    user-select: none;
    list-style-position: inside;
  }
  .thinking-body {
    padding: 2px 10px 8px;
    color: var(--fg-muted);
    border-top: 1px solid var(--border);
    max-height: min(320px, 40vh);
    overflow-y: auto;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .avatar.live,
    .phase.live::before,
    .thinking span {
      animation: none;
    }
    .markdown.streaming > :global(:last-child:not(.code-block))::after,
    .markdown.streaming > :global(.code-block:last-child > pre)::after {
      animation: none;
    }
  }

  /* Markdown output (global because the HTML is injected). */
  .markdown :global(> :first-child) {
    margin-top: 0;
  }
  .markdown :global(> :last-child) {
    margin-bottom: 0;
  }
  .markdown :global(p),
  .markdown :global(ul),
  .markdown :global(ol),
  .markdown :global(blockquote) {
    margin: 0 0 0.9em;
  }
  .markdown :global(h1),
  .markdown :global(h2),
  .markdown :global(h3) {
    margin: 1.2em 0 0.5em;
    line-height: 1.3;
    font-weight: 600;
  }
  .markdown :global(h1) {
    font-size: 1.35em;
  }
  .markdown :global(h2) {
    font-size: 1.2em;
  }
  .markdown :global(h3) {
    font-size: 1.05em;
  }
  .markdown :global(a) {
    color: var(--accent);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .markdown :global(code) {
    background: var(--code-bg);
    padding: 1px 5px;
    border-radius: 4px;
  }
  .markdown :global(.code-block) {
    position: relative;
    margin: 0.6em 0 1em;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--code-bg);
    /* Clip the header strip and pre to the block's rounded corners; pre scrolls internally. */
    overflow: hidden;
  }
  .markdown :global(.code-head) {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 3px 6px 3px 12px;
    background: var(--bg-sunken);
    border-bottom: 1px solid var(--border);
    font-size: 11px;
    color: var(--fg-faint);
    font-family: var(--font-mono);
    min-height: 26px;
  }
  /* The icon is a -webkit-mask data-URI on a ::before so the glyph renders in currentColor
     and no SVG markup is needed in sanitized HTML (DOMPurify strips SVG geometry attrs). */
  .markdown :global(.code-copy) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .markdown :global(.code-copy::before) {
    content: "";
    width: 13px;
    height: 13px;
    background-color: currentColor;
    mask-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16' fill='none' stroke='black' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'%3E%3Crect x='5.5' y='5.5' width='8' height='8' rx='1.5'/%3E%3Cpath d='M10.5 5.5v-2A1.5 1.5 0 0 0 9 2H3.5A1.5 1.5 0 0 0 2 3.5V9a1.5 1.5 0 0 0 1.5 1.5h2'/%3E%3C/svg%3E");
    -webkit-mask-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16' fill='none' stroke='black' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'%3E%3Crect x='5.5' y='5.5' width='8' height='8' rx='1.5'/%3E%3Cpath d='M10.5 5.5v-2A1.5 1.5 0 0 0 9 2H3.5A1.5 1.5 0 0 0 2 3.5V9a1.5 1.5 0 0 0 1.5 1.5h2'/%3E%3C/svg%3E");
    mask-position: center;
    -webkit-mask-position: center;
    mask-repeat: no-repeat;
    -webkit-mask-repeat: no-repeat;
    mask-size: contain;
    -webkit-mask-size: contain;
  }
  .markdown :global(.code-copy:hover) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .markdown :global(.code-copy.copied) {
    color: var(--success);
  }
  .markdown :global(.code-copy.copied::before) {
    mask-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16' fill='none' stroke='black' stroke-width='1.8' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M2.5 8.5 6 12 13.5 4'/%3E%3C/svg%3E");
    -webkit-mask-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16' fill='none' stroke='black' stroke-width='1.8' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M2.5 8.5 6 12 13.5 4'/%3E%3C/svg%3E");
  }
  .markdown :global(pre) {
    margin: 0;
    padding: 12px 14px;
    border: none;
    border-radius: 0;
    background: transparent;
    overflow-x: auto;
    font-size: 13px;
    line-height: 1.5;
  }
  .markdown :global(pre code) {
    background: none;
    padding: 0;
  }
  .markdown :global(blockquote) {
    border-left: 3px solid var(--border-strong);
    padding-left: 12px;
    color: var(--fg-muted);
  }
  /* display:block lets a too-wide table scroll inside itself instead of widening the page. */
  .markdown :global(table) {
    display: block;
    overflow-x: auto;
    border-collapse: collapse;
    margin: 0.6em 0 1em;
    font-size: 0.95em;
    max-width: 100%;
  }
  .markdown :global(th),
  .markdown :global(td) {
    border: 1px solid var(--border);
    padding: 5px 10px;
    text-align: left;
  }
  .markdown :global(hr) {
    border: none;
    border-top: 1px solid var(--border);
    margin: 1.2em 0;
  }
  .markdown :global(.katex) {
    font-size: 1.06em;
  }
  /* Wide display math scrolls inside itself instead of widening the page (same fix as tables). */
  .markdown :global(.katex-block) {
    overflow-x: auto;
    overflow-y: hidden;
    margin: 0.6em 0 1em;
    padding: 4px 0;
  }
  .markdown :global(.katex-error) {
    font-family: var(--font-mono);
    font-size: 0.9em;
    color: var(--danger, #c0392b);
  }
  .actions {
    display: flex;
    gap: 2px;
    margin-top: 2px;
  }
  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    padding: 0;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .icon-btn:hover {
    background: var(--bg-hover);
    color: var(--fg-muted);
  }
  .icon-btn.copied {
    color: var(--success);
  }
</style>
