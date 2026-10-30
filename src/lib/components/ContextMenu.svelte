<script module lang="ts">
  export type ContextMenuItem = {
    label: string;
    action: () => void;
    disabled?: boolean;
  };
</script>

<script lang="ts">
  let {
    x,
    y,
    items,
    onclose,
  }: {
    x: number;
    y: number;
    items: ContextMenuItem[];
    onclose: () => void;
  } = $props();

  let menuEl = $state<HTMLElement>();

  // Flip the menu toward the middle of the window when it opens in the outer half of an
  // axis, so it stays on screen without measuring.
  const flipX = $derived(x > window.innerWidth / 2);
  const flipY = $derived(y > window.innerHeight / 2);

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }
  function onDown(e: MouseEvent) {
    if (menuEl && !menuEl.contains(e.target as Node)) onclose();
  }
</script>

<svelte:window onkeydown={onKey} onmousedown={onDown} onblur={onclose} onresize={onclose} />

<div
  class="ctx-menu"
  bind:this={menuEl}
  style="left:{x}px;top:{y}px;transform:translate({flipX ? "-100%" : "0"},{flipY ? "-100%" : "0"})"
  role="menu"
>
  {#each items as item (item.label)}
    <button
      class="ctx-item"
      role="menuitem"
      disabled={item.disabled}
      onclick={() => {
        item.action();
        onclose();
      }}
    >
      {item.label}
    </button>
  {/each}
</div>

<style>
  .ctx-menu {
    position: fixed;
    z-index: 50;
    min-width: 148px;
    padding: 4px;
    background: var(--bg-elevated);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-2);
    display: flex;
    flex-direction: column;
  }
  .ctx-item {
    justify-content: flex-start;
    padding: 5px 10px;
    font-size: 13px;
    border-radius: var(--radius-sm);
    color: var(--fg);
  }
  .ctx-item:not(:disabled):hover {
    background: var(--accent);
    color: var(--accent-fg);
  }
  .ctx-item:disabled {
    color: var(--fg-faint);
  }
</style>
