<script lang="ts">
  import type { AssistantTurn } from "$lib/state/run-reducer";
  import { isTerminal } from "$lib/state/run-reducer";
  import { renderMarkdown } from "$lib/markdown";

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
  // Re-rendering the whole turn per coalesced update is fine at these sizes; chunked
  // reconciliation comes with the performance pass.
  const html = $derived(turn.text ? renderMarkdown(turn.text) : "");
  const usageText = $derived.by(() => {
    const u = turn.usage;
    if (!u) return null;
    const parts = [];
    if (u.input_tokens != null) parts.push(`${u.input_tokens} in`);
    if (u.output_tokens != null) parts.push(`${u.output_tokens} out`);
    if (u.cache_read_tokens) parts.push(`${u.cache_read_tokens} cached`);
    return parts.join(" · ");
  });
</script>

<article class="assistant" aria-label="Assistant">
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

    <div class="body selectable">
      {#if html && live}
        <div class="markdown streaming">{@html html}</div>
      {:else if html}
        <div class="markdown">{@html html}</div>
      {:else if live}
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
  }
  .markdown :global(.code-lang) {
    position: absolute;
    top: 6px;
    right: 10px;
    font-size: 11px;
    color: var(--fg-faint);
    font-family: var(--font-mono);
  }
  .markdown :global(pre) {
    margin: 0;
    padding: 12px 14px;
    background: var(--code-bg);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
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
  .markdown :global(table) {
    border-collapse: collapse;
    margin: 0.6em 0 1em;
    font-size: 0.95em;
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
</style>
