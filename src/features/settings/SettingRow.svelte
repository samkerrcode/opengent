<script lang="ts">
  import type { Snippet } from "svelte";

  // One setting: label and help on the left, the control (or a read-only value) on the
  // right. `error` is the inline message for a write that failed and was reverted.
  // `controlId` ties the label to a form control; `stacked` puts the control below.
  let {
    label,
    description,
    error = null,
    controlId,
    stacked = false,
    children,
  }: {
    label: string;
    description?: string;
    error?: string | null;
    controlId?: string;
    stacked?: boolean;
    children: Snippet;
  } = $props();
</script>

<div class="row" class:stacked>
  <div class="text">
    {#if controlId}<label class="label" for={controlId}>{label}</label>{:else}<span class="label">{label}</span>{/if}
    {#if description}<span class="desc">{description}</span>{/if}
  </div>
  <div class="control">{@render children()}</div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</div>

<style>
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: var(--space-2) var(--space-4);
    align-items: center;
    padding: var(--space-3) var(--space-4);
  }
  .row.stacked {
    grid-template-columns: minmax(0, 1fr);
  }
  .text {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .label {
    font-size: 13.5px;
    color: var(--fg);
  }
  .desc {
    font-size: 12.5px;
    line-height: 1.4;
    color: var(--fg-muted);
  }
  .control {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    font-size: 13px;
    color: var(--fg-muted);
  }
  .error {
    grid-column: 1 / -1;
    margin: 0;
    padding: 6px 10px;
    border-radius: var(--radius-md);
    background: var(--danger-soft);
    color: var(--danger);
    font-size: 12.5px;
  }
</style>
