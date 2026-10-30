<script lang="ts">
  import type { AssistantTurn } from "$lib/state/run-reducer";
  import { isTerminal } from "$lib/state/run-reducer";

  let { turn }: { turn: AssistantTurn } = $props();

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
</script>

<article class="assistant" aria-label="Assistant">
  <aside class="rail" aria-live="polite">
    <span class="phase" class:live={!isTerminal(turn.phase)} data-phase={turn.phase}>
      {phaseLabel[turn.phase]}
    </span>
    {#if turn.usage}
      <span class="usage">
        {turn.usage.input_tokens ?? "?"} in · {turn.usage.output_tokens ?? "?"} out
      </span>
    {/if}
    {#if turn.desynced}
      <span class="warn" title="Some events were missed">desynced</span>
    {/if}
  </aside>

  <div class="body selectable">
    {#if turn.text}
      <!-- Phase 2 swaps this for incremental, sanitized Markdown rendering. -->
      <pre class="text">{turn.text}</pre>
    {:else if !isTerminal(turn.phase)}
      <span class="cursor" aria-hidden="true"></span>
    {/if}
    {#if turn.error}
      <p class="error" role="alert">{turn.error}</p>
    {/if}
  </div>
</article>

<style>
  .assistant {
    display: grid;
    grid-template-columns: 1fr;
    gap: var(--space-2);
  }
  .rail {
    display: flex;
    gap: var(--space-3);
    align-items: center;
    font-size: 12px;
    color: var(--fg-faint);
  }
  .phase {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--bg-sunken);
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
  }
  .phase[data-phase="cancelled"] {
    color: var(--warning);
  }
  .warn {
    color: var(--warning);
  }
  .text {
    margin: 0;
    font-family: var(--font-sans);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: 15px;
    line-height: 1.65;
  }
  .cursor {
    display: inline-block;
    width: 8px;
    height: 1.1em;
    background: var(--accent);
    border-radius: 2px;
    animation: pulse 1s ease-in-out infinite;
    vertical-align: text-bottom;
  }
  .error {
    color: var(--danger);
    margin: var(--space-2) 0 0;
  }
  @keyframes pulse {
    50% {
      opacity: 0.3;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .phase.live::before,
    .cursor {
      animation: none;
    }
  }
</style>
