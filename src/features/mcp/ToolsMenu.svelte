<script lang="ts">
  import { mcpStore } from "./mcp-store.svelte";
  import ToolAccessBadge from "./ToolAccessBadge.svelte";
  import ToolPolicySelect from "./ToolPolicySelect.svelte";
  import { ownAccess, snapshotAccess, toolOn, toolPolicy, withTool } from "./permissions";
  import { agentStore } from "$features/agents/agent-store.svelte";
  import {
    describeError,
    type Agent,
    type AgentServerAccess,
    type McpServerView,
    type PermissionScope,
    type ToolInfo,
    type ToolLoading,
    type ToolPolicy,
  } from "$lib/ipc/commands";

  // Plain chats edit this chat's overrides; agent chats edit the agent. Nothing here
  // enables/disables a connection — that lives in Connections.
  let {
    scope,
    agent = null,
    onopenconnections,
    onclose,
  }: { scope: PermissionScope; agent?: Agent | null; onopenconnections: () => void; onclose: () => void } = $props();

  let menuEl = $state<HTMLElement>();
  let openServer = $state<string | null>(null);
  let error = $state<string | null>(null);

  const loadings: { value: ToolLoading; label: string }[] = [
    { value: "auto", label: "Auto — all tools when the set is small, on-demand when large" },
    { value: "all", label: "Always load all tool definitions" },
    { value: "on_demand", label: "On-demand — the model searches and loads tools" },
  ];

  const isAgent = $derived(scope.kind === "agent");
  const views = $derived(mcpStore.scopedFor(scope));

  const view = (id: string) => views?.find((v) => v.server_id === id);
  const scopedTool = (s: McpServerView, name: string) => view(s.id)?.tools.find((t) => t.name === name);
  const accessFor = (s: McpServerView): AgentServerAccess | null => agent?.servers.find((a) => a.server_id === s.id) ?? null;
  const inScope = (s: McpServerView) => (isAgent ? accessFor(s) !== null : (view(s.id)?.in_scope ?? true));

  function dotClass(s: McpServerView) {
    if (!s.enabled || !inScope(s)) return "off";
    switch (s.status.kind) {
      case "connected": return "ok";
      case "connecting": return "busy";
      case "failed": return "bad";
      default: return "off";
    }
  }

  function activeIn(s: McpServerView): number {
    const v = view(s.id);
    return v ? v.tools.filter((t) => t.in_scope && t.policy !== "block").length : 0;
  }

  function statusText(s: McpServerView): string {
    if (!s.enabled) return "off in Connections";
    if (!inScope(s)) return isAgent ? "not used by agent" : "off in this chat";
    switch (s.status.kind) {
      case "connected": {
        const on = activeIn(s);
        return on === s.tools.length ? `${on} tools` : `${on} of ${s.tools.length} tools`;
      }
      case "connecting": return "connecting…";
      case "failed": return "failed";
      default: return "idle";
    }
  }

  async function act(p: Promise<unknown>) {
    error = null;
    try {
      await p;
      if (isAgent) await agentStore.load();
    } catch (e) {
      error = describeError(e);
    }
  }

  function saveAccess(s: McpServerView, access: AgentServerAccess | null) {
    if (agent) void act(mcpStore.setAgentServerAccess(agent.id, s.id, access));
  }

  function toggleServer(s: McpServerView, on: boolean) {
    if (scope.kind === "chat") return void act(mcpStore.setChatServerEnabled(scope.id, s.id, on));
    const cur = accessFor(s);
    saveAccess(s, on ? (cur ? ownAccess(cur, s) : snapshotAccess(s)) : null);
  }

  function setChatTool(s: McpServerView, name: string, policy: ToolPolicy | null) {
    if (scope.kind === "chat") void act(mcpStore.setChatToolPolicy(scope.id, s.id, name, policy));
  }

  function setAgentTool(s: McpServerView, t: ToolInfo, policy: ToolPolicy | "on") {
    const cur = accessFor(s);
    if (cur) saveAccess(s, withTool(ownAccess(cur, s), t, policy));
  }

  function setAgentDefault(s: McpServerView, policy: ToolPolicy) {
    const cur = accessFor(s);
    if (cur) saveAccess(s, { ...ownAccess(cur, s), default_policy: policy });
  }

  function reset(s: McpServerView) {
    if (scope.kind === "chat") void act(mcpStore.resetChatToolPermissions(scope.id, s.id));
    else saveAccess(s, snapshotAccess(s));
  }

  const hasOverrides = (s: McpServerView) => isAgent || (view(s.id)?.tools.some((t) => t.overridden) ?? false);

  function onDown(e: MouseEvent) {
    if (menuEl && !menuEl.contains(e.target as Node)) onclose();
  }
  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }
  function openConnections() {
    onclose();
    onopenconnections();
  }
</script>

<svelte:window onmousedown={onDown} onkeydown={onKey} />

<div class="menu" bind:this={menuEl} role="menu" aria-label="Tools and connections">
  <header class="head">
    <span class="h">{isAgent && agent ? `Tools · ${agent.name}` : "Tools · this chat"}</span>
    {#if mcpStore.servers.length}<span class="n">{mcpStore.activeToolCountFor(scope)} active</span>{/if}
  </header>
  <p class="scope-hint">
    {isAgent ? "Changes are saved to the agent and apply to all its chats and schedules." : "Changes apply to this chat only. Defaults for new chats are set in Connections."}
  </p>
  {#if error}<p class="err top">{error}</p>{/if}

  <div class="list">
    {#each mcpStore.servers as s (s.id)}
      {@const on = inScope(s)}
      {@const access = accessFor(s)}
      {@const own = access ? ownAccess(access, s) : null}
      <div class="server">
        <div class="srow">
          <button
            class="expand"
            class:open={openServer === s.id}
            onclick={() => (openServer = openServer === s.id ? null : s.id)}
            disabled={s.tools.length === 0 || !s.enabled || !on}
            aria-label="Tool permissions for {s.name}"
          >›</button>
          <span class="dot {dotClass(s)}" aria-hidden="true"></span>
          <span class="sname" title={s.name}>{s.name}</span>
          <span class="scount">{statusText(s)}</span>
          {#if s.enabled}
            <label class="toggle" title={on ? (isAgent ? "Stop using in this agent" : "Turn off for this chat") : (isAgent ? "Use in this agent" : "Turn on for this chat")}>
              <input type="checkbox" checked={on} onchange={(e) => toggleServer(s, e.currentTarget.checked)} />
            </label>
          {:else}
            <button class="enable" onclick={openConnections} title="Connections are enabled in Connections">Enable…</button>
          {/if}
        </div>
        {#if s.enabled && on && s.status.kind === "failed"}
          <p class="err" title={s.status.error}>{s.status.error}</p>
        {/if}
        {#if openServer === s.id && s.tools.length && s.enabled && on}
          <div class="sub">
            {#if isAgent && own}
              <div class="def">
                <span>New tools</span>
                <select value={own.default_policy === "block" ? "block" : "ask"} onchange={(e) => setAgentDefault(s, e.currentTarget.value as ToolPolicy)} aria-label="New tools on {s.name}">
                  <option value="ask">Ask</option>
                  <option value="block">Off</option>
                </select>
              </div>
            {/if}
            {#each s.tools as t (t.name)}
              {@const st = scopedTool(s, t.name)}
              {@const agentOn = own ? toolOn(own, t.name) : false}
              <div class="tool" class:off={t.policy === "block" || (isAgent ? !agentOn : st?.policy === "block")} title={t.description ?? t.name}>
                {#if isAgent && t.policy !== "block"}
                  <input class="tcheck" type="checkbox" checked={agentOn} aria-label="Use {t.name}" onchange={(e) => setAgentTool(s, t, e.currentTarget.checked ? "on" : "block")} />
                {/if}
                <span class="tname">{t.title ?? t.name}</span>
                <ToolAccessBadge access={t.access} />
                {#if st?.changed}
                  <span class="warn" title="This tool's definition changed since it was allowed — it will ask for approval again">changed</span>
                {/if}
                {#if t.policy === "block"}
                  <span class="ceil" title="Disabled in Connections — no chat or agent can turn it on">off in Connections</span>
                {:else if isAgent && own}
                  {#if agentOn}
                    <ToolPolicySelect value={toolPolicy(own, t.name)} allowDisable={false} label="Permission for {t.name}" onchange={(p) => setAgentTool(s, t, p)} />
                  {/if}
                {:else if st}
                  {#if st.overridden}
                    <button class="ovr" title="Set for this chat — click to use the connection default" onclick={() => setChatTool(s, t.name, null)}>●</button>
                  {/if}
                  <ToolPolicySelect value={st.policy} label="Permission for {t.name}" onchange={(p) => setChatTool(s, t.name, p)} />
                {/if}
              </div>
            {/each}
            {#if hasOverrides(s)}
              <button class="reset" onclick={() => reset(s)}>{isAgent ? "Reset to connection defaults" : "Reset to defaults"}</button>
            {/if}
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
    <button class="manage" onclick={openConnections}>Manage connections…</button>
  </footer>
</div>

<style>
  .menu {
    position: absolute;
    bottom: calc(100% + 8px);
    left: 0;
    width: 400px;
    max-height: 460px;
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
    padding: 10px 12px 2px;
    font-size: 12px;
    font-weight: 600;
    color: var(--fg-muted);
  }
  .n {
    margin-left: auto;
    font-weight: normal;
    color: var(--fg-faint);
  }
  .scope-hint {
    margin: 0;
    padding: 0 12px 8px;
    font-size: 11px;
    color: var(--fg-faint);
    line-height: 1.4;
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
  .toggle input,
  .tcheck {
    accent-color: var(--accent);
    margin: 0;
  }
  .enable {
    font-size: 11px;
    padding: 1px 6px;
    color: var(--accent);
  }
  .err {
    margin: 0 6px 4px 31px;
    font-size: 11.5px;
    color: var(--danger);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .err.top {
    margin: 0 12px 6px;
    white-space: normal;
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
  .def select {
    width: auto;
    padding: 1px 6px;
    font-size: 11px;
  }
  .tool {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
  }
  .tool.off .tname {
    opacity: 0.5;
  }
  .tname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .warn {
    flex: none;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--warning);
    border: 1px solid var(--warning);
    border-radius: var(--radius-sm);
    padding: 0 4px;
  }
  .ceil {
    flex: none;
    font-size: 11px;
    color: var(--fg-faint);
  }
  .ovr {
    flex: none;
    padding: 0 2px;
    font-size: 9px;
    color: var(--accent);
  }
  .reset {
    justify-self: start;
    font-size: 11px;
    padding: 2px 4px;
    color: var(--fg-muted);
  }
  .reset:hover {
    color: var(--fg);
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
