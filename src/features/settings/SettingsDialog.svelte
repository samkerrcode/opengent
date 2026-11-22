<script lang="ts">
  import { trapFocus } from "$lib/focus-trap";
  import { nav } from "$lib/state/nav.svelte";
  import { settingsLinks, settingsPanes } from "./sections";

  let { onclose }: { onclose: () => void } = $props();

  const panes = $derived(settingsPanes());
  const links = settingsLinks();
  /** The requested section, or the first one when it isn't shown. */
  const current = $derived(panes.find((p) => p.id === nav.settingsSection) ?? panes[0]);

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }
</script>

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="app-settings-title" tabindex="-1" use:trapFocus onkeydown={onKeydown}>
    <header class="head">
      <h2 id="app-settings-title">Settings</h2>
      <button class="btn-ghost" onclick={onclose} aria-label="Close">✕</button>
    </header>

    <div class="body">
      <nav class="rail" aria-label="Settings sections">
        {#each panes as s (s.id)}
          <button class="item" class:active={s === current} aria-current={s === current ? "page" : undefined} onclick={() => nav.openSettings(s.id)}>
            <span class="ico" aria-hidden="true">{s.icon}</span>
            {s.label}
          </button>
        {/each}
        <hr />
        {#each links as s (s.id)}
          <button class="item" onclick={() => nav.openSettings(s.id)} title={s.description}>
            <span class="ico" aria-hidden="true">{s.icon}</span>
            {s.label}
            <span class="chev" aria-hidden="true">›</span>
          </button>
        {/each}
      </nav>

      <div class="pane">
        {#if current}
          {#key current.id}
            <div class="scroll">
              <header class="pane-head">
                <h3>{current.label}</h3>
                <p class="lede">{current.description}</p>
              </header>
              <current.component />
            </div>
          {/key}
        {/if}
      </div>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: grid;
    place-items: center;
    z-index: 50;
  }
  .dialog {
    width: min(760px, calc(100vw - 48px));
    height: min(540px, calc(100vh - 48px));
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-2);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    outline: none;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-4) var(--space-5);
    border-bottom: 1px solid var(--border);
  }
  h2 {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
  }
  .body {
    display: grid;
    grid-template-columns: 200px minmax(0, 1fr);
    flex: 1;
    min-height: 0;
  }
  .rail {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--space-3);
    border-right: 1px solid var(--border);
    background: var(--bg);
    overflow-y: auto;
  }
  .item {
    justify-content: flex-start;
    width: 100%;
    padding: 7px 10px;
    color: var(--fg-muted);
    text-align: left;
  }
  .item:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .item.active {
    background: var(--accent-soft);
    color: var(--fg);
  }
  .ico {
    width: 16px;
    text-align: center;
    color: var(--accent);
    font-size: 12px;
  }
  .chev {
    margin-left: auto;
    font-size: 16px;
    line-height: 1;
    color: var(--fg-faint);
  }
  hr {
    width: 100%;
    margin: var(--space-2) 0;
    border: 0;
    border-top: 1px solid var(--border);
  }
  .pane {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: var(--space-5);
    display: flex;
    flex-direction: column;
    gap: var(--space-5);
  }
  .pane-head {
    display: grid;
    gap: 2px;
  }
  h3 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }
  .lede {
    margin: 0;
    font-size: 13px;
    color: var(--fg-muted);
  }
</style>
