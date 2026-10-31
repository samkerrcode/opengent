<script lang="ts">
  import { mcpStore } from "./mcp-store.svelte";
  import type { McpServerView, ToolLoading, ToolPolicy } from "$lib/ipc/commands";

  let { onopenconnections, onclose }: { onopenconnections: () => void; onclose: () => void } = $props();

  let menuEl = $state<HTMLElement>();
  let openServer = $state<string | null>(null);

  const policies: { value: ToolPolicy; label: string; hint: string }[] = [
    { value: "always_allow", label: "Allow", hint: "run without asking" },
    { value: "ask", label: "Ask", hint: "approve each call" },
    { value: "block", label: "Block", hint: "hidden from the model" },
  ];

  const loadings: { value: ToolLoading; label: string }[] = [
    { value: "auto", label: "Auto — all tools when the set is small, on-demand when large" },
    { value: "all", label: "Always load all tool definitions" },
    { value: "on_demand", label: "On-demand — the model searches and loads tools" },
  ];

  function dotClass(s: McpServerView) {
    if (!s.enabled) return "off";
    switch (s.status.kind) {
      case "connected": return "ok";
      case "connecting": return "busy";
      case "failed": return "bad";
      default: return "off";
    }
  }

  function statusText(s: McpServerView): string {
    switch (s.status.kind) {
      case "connected": return `${s.status.tool_count} tools`;
      case "connecting": return "connecting…";
      case "failed": return "failed";
      case "disabled": return "off";
      default: return "idle";
    }
  }

  function onDown(e: MouseEvent) {
    if (menuEl && !menuEl.contains(e.target as Node)) onclose();
  }
  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }
</script>

<svelte:window onmousedown={onDown} onkeydown={onKey} />

<div class="menu" bind:this={menuEl} role="menu" aria-label="Tools and connections">
  <header class="head">
    <span class="h">Connections</span>
    {#if mcpStore.toolCount}<span class="n">{mcpStore.toolCount} tools</span>{/if}
  </header>

  <div class="list">
    {#each mcpStore.servers as s (s.id)}
      <div class="server">
        <div class="srow">
          <button
            class="expand"
            class:open={openServer === s.id}
            onclick={() => (openServer = openServer === s.id ? null : s.id)}
            disabled={s.tools.length === 0}
            aria-label="Tool permissions for {s.name}"
          >›</button>
          <span class="dot {dotClass(s)}" aria-hidden="true"></span>
          <span class="sname" title={s.name}>{s.name}</span>
          <span class="scount">{statusText(s)}</span>
          <label class="toggle" title={s.enabled ? "Disable server" : "Enable server"}>
            <input
              type="checkbox"
              checked={s.enabled}
              onchange={(e) => mcpStore.setEnabled(s.id, e.currentTarget.checked)}
            />
          </label>
        </div>
        {#if s.status.kind === "failed"}
          <p class="err" title={s.status.error}>{s.status.error}</p>
        {/if}
        {#if openServer === s.id && s.tools.length}
          <div class="sub">
            <label class="def">
              <span>New tools default to</span>
              <select value={s.default_policy} onchange={(e) => mcpStore.setDefaultPolicy(s.id, e.currentTarget.value as ToolPolicy)}>
                {#each policies as p (p.value)}<option value={p.value}>{p.label}</option>{/each}
              </select>
            </label>
            {#each s.tools as t (t.name)}
              <div class="tool" title={t.description ?? t.name}>
                <span class="tname">{t.title ?? t.name}</span>
                <select
                  class="policy"
                  value={t.policy}
                  aria-label="Permission for {t.name}"
                  onchange={(e) => mcpStore.setToolPolicy(s.id, t.name, e.currentTarget.value as ToolPolicy)}
                >
                  {#each policies as p (p.value)}<option value={p.value}>{p.label}</option>{/each}
                </select>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {:else}
      <p class="empty">No tool servers yet.</p>
    {/each}
  </div>

  <footer class="foot">
    <label class="loading">
      <span>Tool loading</span>
      <select value={mcpStore.settings.loading} onchange={(e) => mcpStore.setLoading(e.currentTarget.value as ToolLoading)}>
        {#each loadings as l (l.value)}<option value={l.value}>{l.label}</option>{/each}
      </select>
    </label>
    <button class="manage" onclick={() => { onclose(); onopenconnections(); }}>Manage connections…</button>
  </footer>
</div>

<style>
  .menu {
    position: absolute;
    bottom: calc(100% + 8px);
    left: 0;
    width: 340px;
    max-height: 420px;
    display: flex;
    flex-direction: column;
    background: var(--bg-elevated);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-2);
    z-index: 40;
    overflow: hidden;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 10px 12px 8px;
    font-size: 12px;
    font-weight: 600;
    color: var(--fg-muted);
  }
  .n {
    margin-left: auto;
    font-weight: normal;
    color: var(--fg-faint);
  }
  .list {
    overflow-y: auto;
    min-height: 0;
    padding: 0 6px 6px;
  }
  .server {
    border-radius: var(--radius-md);
  }
  .server:hover {
    background: var(--bg-hover);
  }
  .srow {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 5px 6px;
  }
  .expand {
    width: 18px;
    padding: 0;
    justify-content: center;
    color: var(--fg-faint);
    transition: transform var(--motion-fast) var(--ease);
    display: inline-flex;
  }
  .expand.open {
    transform: rotate(90deg);
  }
  .expand:disabled {
    visibility: hidden;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex: none;
    background: var(--fg-faint);
  }
  .dot.ok { background: var(--success); }
  .dot.busy { background: var(--warning); animation: pulse 1.2s ease-in-out infinite; }
  .dot.bad { background: var(--danger); }
  .dot.off { background: var(--border-strong); }
  .sname {
    font-size: 13px;
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .scount {
    margin-left: auto;
    font-size: 11px;
    color: var(--fg-faint);
    flex: none;
  }
  .toggle input {
    accent-color: var(--accent);
    margin: 0;
  }
  .err {
    margin: 0 6px 4px 31px;
    font-size: 11.5px;
    color: var(--danger);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sub {
    margin: 0 6px 6px 31px;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg);
    display: grid;
    gap: 4px;
  }
  .def {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11.5px;
    color: var(--fg-faint);
    padding-bottom: 4px;
    border-bottom: 1px solid var(--border);
    margin-bottom: 2px;
  }
  .def select { width: auto; padding: 2px 6px; font-size: 11.5px; }
  .tool {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
  }
  .tname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .policy {
    width: auto;
    padding: 1px 6px;
    font-size: 11px;
    border-radius: var(--radius-sm);
  }
  .empty {
    margin: 0;
    padding: 12px;
    text-align: center;
    color: var(--fg-faint);
    font-size: 12.5px;
  }
  .foot {
    border-top: 1px solid var(--border);
    padding: 8px 12px;
    display: grid;
    gap: 8px;
  }
  .loading {
    display: grid;
    gap: 4px;
    font-size: 11.5px;
    color: var(--fg-faint);
  }
  .loading select {
    font-size: 12px;
    padding: 4px 8px;
  }
  .manage {
    font-size: 12px;
    color: var(--fg-muted);
    justify-content: flex-start;
    padding: 4px 8px;
    margin: 0 -8px;
  }
  .manage:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
</style>
