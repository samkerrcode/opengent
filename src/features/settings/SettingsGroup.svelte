<script lang="ts">
  import type { Snippet } from "svelte";

  // A titled card of `SettingRow`s inside a Settings section.
  let { title, description, children }: { title: string; description?: string; children: Snippet } = $props();
  const id = $props.id();
</script>

<section class="group" aria-labelledby={id}>
  <h4 {id}>{title}</h4>
  {#if description}<p class="desc">{description}</p>{/if}
  <div class="card">{@render children()}</div>
</section>

<style>
  .group {
    display: grid;
    gap: var(--space-2);
  }
  h4 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--fg-faint);
  }
  .desc {
    margin: 0;
    font-size: 12.5px;
    color: var(--fg-muted);
  }
  .card {
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }
  .card > :global(* + *) {
    border-top: 1px solid var(--border);
  }
</style>
