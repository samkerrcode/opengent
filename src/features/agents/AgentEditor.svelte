<script lang="ts">
  import { untrack } from "svelte";
  import { agentStore } from "./agent-store.svelte";
  import ModelSelect from "./ModelSelect.svelte";
  import SchedulesPanel from "./SchedulesPanel.svelte";
  import { nav } from "$lib/state/nav.svelte";
  import { chatStore } from "$features/chat/chat-store.svelte";
  import { mcpStore } from "$features/mcp/mcp-store.svelte";
  import { providerStore } from "$features/providers/provider-store.svelte";
  import { product } from "$lib/generated/product";
  import ToolAccessBadge from "$features/mcp/ToolAccessBadge.svelte";
  import ToolPolicySelect from "$features/mcp/ToolPolicySelect.svelte";
  import { allowedSummary, ownAccess, snapshotAccess, toolOn, toolPolicy, withTool } from "$features/mcp/permissions";
  import {
    describeError,
    type Agent,
    type AgentServerAccess,
    type McpServerView,
    type ModelRef,
    type ToolInfo,
    type ToolPolicy,
  } from "$lib/ipc/commands";

  // Mounted inside {#key} by App, so the form initializes once per agent.
  let { agentId, onopenconnections }: { agentId: string | null; onopenconnections: () => void } = $props();

  // `initial` seeds the form once; `existing` tracks the stored agent (refreshed after saves).
  const initial = untrack(() => agentStore.byId(agentId));
  const existing = $derived(agentStore.byId(agentId));
  // The agent's own access per server; `on: false` keeps edits if the server is re-ticked.
  type ServerSel = { on: boolean; access: AgentServerAccess };

  function initialModel(): ModelRef | null {
    if (initial) return { ...initial.default_model };
    return providerStore.active ? { ...providerStore.active } : null;
  }

  let name = $state(initial?.name ?? "");
  let description = $state(initial?.description ?? "");
  let defaultModel = $state<ModelRef | null>(initialModel());
  let fallbackModel = $state<ModelRef | null>(initial?.fallback_model ? { ...initial.fallback_model } : null);
  let systemPrompt = $state(initial?.system_prompt ?? "");
  // Legacy agents (no own permissions yet) are converted from the connection's current
  // permissions on open; saving persists the conversion.
  function seed(agent: Agent | null): Record<string, ServerSel> {
    return Object.fromEntries(
      (agent?.servers ?? []).map((a) => [
        a.server_id,
        { on: true, access: ownAccess(a, mcpStore.servers.find((s) => s.id === a.server_id)) },
      ]),
    );
  }

  let selection = $state<Record<string, ServerSel>>(seed(initial));
  let toolsDirty = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let savedNote = $state<string | null>(null);
  let openTools = $state<string | null>(null);

  // Tool edits made elsewhere (an agent chat's ⚒ Tools menu, approval grants) refresh this
  // card while it has no unsaved changes of its own.
  $effect(() => {
    const agent = existing;
    if (agent && !untrack(() => toolsDirty)) selection = seed(agent);
  });

  const MAX_PROMPT = 32 * 1024;
  const canSave = $derived(name.trim().length > 0 && defaultModel !== null && !busy);

  function toggleServer(s: McpServerView, on: boolean) {
    toolsDirty = true;
    const cur = selection[s.id];
    if (cur) cur.on = on;
    else selection[s.id] = { on, access: snapshotAccess(s) };
    if (on && openTools === null) openTools = s.id;
  }

  function editTool(s: McpServerView, t: ToolInfo, policy: ToolPolicy | "on") {
    const cur = selection[s.id];
    if (!cur) return;
    toolsDirty = true;
    cur.access = withTool(cur.access, t, policy);
  }

  function setNewTools(s: McpServerView, policy: ToolPolicy) {
    const cur = selection[s.id];
    if (!cur) return;
    toolsDirty = true;
    cur.access = { ...cur.access, default_policy: policy };
  }

  function resetServer(s: McpServerView) {
    toolsDirty = true;
    selection[s.id] = { on: true, access: snapshotAccess(s) };
  }

  function selectedCount(s: McpServerView, a: AgentServerAccess): string {
    const usable = s.tools.filter((t) => t.policy !== "block");
    const on = usable.filter((t) => toolOn(a, t.name)).length;
    return usable.length ? `${on} of ${usable.length} tools on` : "";
  }

  function servers(): AgentServerAccess[] {
    return mcpStore.servers
      .filter((s) => selection[s.id]?.on)
      .map((s) => ({ ...selection[s.id]!.access, policies: { ...selection[s.id]!.access.policies } }));
  }

  async function save(thenChat: boolean) {
    if (!defaultModel) return;
    busy = true;
    error = null;
    savedNote = null;
    try {
      const saved = await agentStore.save({
        id: existing?.id ?? agentId,
        name,
        description,
        default_model: defaultModel,
        fallback_model: fallbackModel,
        system_prompt: systemPrompt,
        servers: servers(),
      });
      toolsDirty = false;
      if (thenChat) {
        if (await chatStore.newChat(saved.id)) nav.showChat();
      } else if (!existing) {
        // Stay in the editor for the new agent so schedules can be added.
        nav.editAgent(saved.id);
      } else {
        savedNote = "Saved.";
      }
    } catch (e) {
      error = describeError(e);
    } finally {
      busy = false;
    }
  }

  async function remove() {
    if (!existing) return;
    if (!confirm(`Delete agent "${existing.name}"? Its schedules are removed; its chats are kept as regular chats.`)) return;
    busy = true;
    try {
      await agentStore.remove(existing.id);
      await chatStore.loadList();
      nav.showAgents();
    } catch (e) {
      error = describeError(e);
      busy = false;
    }
  }
</script>

<header class="topbar">
  <button class="btn-ghost btn-sm back" onclick={() => nav.showAgents()}>← Agents</button>
  <h1>{existing ? existing.name : "New agent"}</h1>
  {#if existing}
    <button class="btn-outline btn-sm push" onclick={() => save(true)} disabled={!canSave}>Chat with agent</button>
  {/if}
</header>

<div class="scroller">
  <form class="column" onsubmit={(e) => (e.preventDefault(), save(false))}>
    <section class="card">
      <h2>Identity</h2>
      <label>
        <span>Name</span>
        <input type="text" bind:value={name} maxlength="64" placeholder="e.g. Release notes scout" />
      </label>
      <label>
        <span>Description <small>(what it's for — also tells the agent its purpose)</small></span>
        <input type="text" bind:value={description} maxlength="500" placeholder="Summarizes new releases of the libraries I use" />
      </label>
    </section>

    <section class="card">
      <h2>Models</h2>
      <div class="row2">
        <label>
          <span>Default model</span>
          <ModelSelect bind:value={defaultModel} />
        </label>
        <label>
          <span>Fallback model <small>(optional)</small></span>
          <ModelSelect bind:value={fallbackModel} allowNone noneLabel="No fallback" />
        </label>
      </div>
      <p class="hint">If the default model fails before it starts answering (offline, auth error, rate limit), the run switches to the fallback automatically.</p>
    </section>

    <section class="card">
      <h2>Instructions</h2>
      <label>
        <span>System prompt</span>
        <textarea rows="9" bind:value={systemPrompt} maxlength={MAX_PROMPT} spellcheck="true"
          placeholder={"You are a concise research assistant.\n- Always cite sources with links.\n- Answer in bullet points."}></textarea>
      </label>
      <p class="hint">
        Appended to {product.displayName}'s built-in agent prompt, which already covers identity, accuracy, safe handling of
        attachments and tool results, and unattended scheduled runs. Focus on role, scope, tone and output format.
        <span class="count">{systemPrompt.length.toLocaleString()} / {MAX_PROMPT.toLocaleString()}</span>
      </p>
    </section>

    <section class="card">
      <div class="h2row">
        <h2>Tools</h2>
        <button type="button" class="btn-ghost btn-sm" onclick={onopenconnections}>Manage connections…</button>
      </div>
      {#if mcpStore.servers.length === 0}
        <p class="hint">No MCP servers are connected yet. Add one under Connections to give this agent tools — or leave it as a chat-only agent.</p>
      {:else}
        <p class="hint">
          Choose the connections and tools this agent may use, and whether each runs without asking. Permissions here apply
          only to this agent. Adding a connection copies its defaults from Connections — read-only tools keep <em>Allow</em>,
          everything else starts at <em>Ask</em> because agents can run unattended. Tools disabled in Connections stay disabled.
        </p>
        <ul class="servers">
          {#each mcpStore.servers as s (s.id)}
            {@const cur = selection[s.id]}
            {@const sum = cur ? allowedSummary(cur.access, s) : null}
            <li class="server" class:on={cur?.on}>
              <div class="srow">
                <label class="check">
                  <input type="checkbox" checked={cur?.on ?? false} onchange={(e) => toggleServer(s, e.currentTarget.checked)} />
                  <span class="sname">{s.name}</span>
                </label>
                <span class="sstatus">
                  {#if !s.enabled}
                    disabled in Connections
                  {:else}
                    {s.status.kind === "connected" ? `${s.tools.length} tools` : s.tools.length ? `${s.status.kind} · last seen ${s.tools.length} tools` : s.status.kind}
                  {/if}
                </span>
                {#if cur?.on}
                  <span class="scount">{selectedCount(s, cur.access)}</span>
                  <label class="newtools">
                    <span>New tools</span>
                    <select value={cur.access.default_policy === "block" ? "block" : "ask"} onchange={(e) => setNewTools(s, e.currentTarget.value as ToolPolicy)} aria-label="New tools on {s.name}">
                      <option value="ask">Ask</option>
                      <option value="block">Off</option>
                    </select>
                  </label>
                {/if}
              </div>
              {#if cur?.on}
                {#if !s.enabled}
                  <p class="hint indent">
                    This connection is disabled, so the agent can't use it. <button type="button" class="linkish" onclick={onopenconnections}>Enable it in Connections…</button>
                  </p>
                {/if}
                {#if sum?.allowed}
                  <p class="hint indent" class:warn={sum.risky > 0}>
                    {sum.allowed} tool{sum.allowed === 1 ? "" : "s"} run without asking{sum.risky ? ` — ${sum.risky} of them can make changes` : ""}.
                  </p>
                {/if}
                {#if openTools === s.id}
                  <div class="tools">
                    {#each s.tools as t (t.name)}
                      {@const on = toolOn(cur.access, t.name)}
                      <div class="tool" class:off={t.policy === "block" || !on} title={t.description ?? t.name}>
                        {#if t.policy === "block"}
                          <span class="tname">{t.title ?? t.name}</span>
                          <ToolAccessBadge access={t.access} />
                          <span class="tag" title="Disabled in Connections — it stays disabled for every chat and agent">disabled in Connections</span>
                        {:else}
                          <input type="checkbox" checked={on} aria-label="Use {t.name}" onchange={(e) => editTool(s, t, e.currentTarget.checked ? "on" : "block")} />
                          <span class="tname">{t.title ?? t.name}</span>
                          <ToolAccessBadge access={t.access} />
                          {#if on}
                            <ToolPolicySelect value={toolPolicy(cur.access, t.name)} allowDisable={false} label="Permission for {t.name}" onchange={(p) => editTool(s, t, p)} />
                          {/if}
                        {/if}
                      </div>
                    {:else}
                      <p class="hint">Connect this server (Connections → Save &amp; test) to list its tools.</p>
                    {/each}
                  </div>
                  <div class="toolacts">
                    <button type="button" class="btn-ghost btn-sm" onclick={() => resetServer(s)} title="Copy the current permissions from Connections again">Reset to connection defaults</button>
                    <button type="button" class="btn-ghost btn-sm" onclick={() => (openTools = null)}>Done</button>
                  </div>
                {:else}
                  <button type="button" class="btn-ghost btn-sm expand" onclick={() => (openTools = s.id)}>Edit tools &amp; permissions…</button>
                {/if}
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </section>

    {#if error}<p class="result bad" role="alert">{error}</p>{/if}
    {#if savedNote}<p class="result ok" role="status">{savedNote}</p>{/if}

    <footer class="actions">
      {#if existing}
        <button type="button" class="btn-danger btn-sm" onclick={remove} disabled={busy}>Delete agent</button>
      {/if}
      <span class="spacer"></span>
      <button type="button" class="btn-ghost" onclick={() => nav.showAgents()}>Cancel</button>
      <button type="button" class="btn-outline" onclick={() => save(true)} disabled={!canSave}>Save & start chat</button>
      <button type="submit" class="btn-primary" disabled={!canSave}>{existing ? "Save" : "Create agent"}</button>
    </footer>

    {#if existing}
      <SchedulesPanel agent={existing} />
    {:else}
      <section class="card muted-card">
        <h2>Schedules</h2>
        <p class="hint">Create the agent first, then schedule it to run a preset prompt automatically.</p>
      </section>
    {/if}
  </form>
</div>

<style>
  .topbar { display: flex; align-items: center; gap: var(--space-2); padding: 8px var(--space-6); border-bottom: 1px solid var(--border); min-height: 44px; }
  .back { margin-left: -9px; }
  h1 { margin: 0; font-size: 14px; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .push { margin-left: auto; }
  .scroller { flex: 1; overflow-y: auto; min-height: 0; }
  .column { max-width: 760px; margin: 0 auto; padding: var(--space-6) var(--space-6) var(--space-8); display: grid; gap: var(--space-4); }
  .card { border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--bg-elevated); padding: var(--space-4); display: grid; gap: var(--space-3); }
  .muted-card { opacity: 0.75; }
  h2 { margin: 0; font-size: 13px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.06em; color: var(--fg-muted); }
  .h2row { display: flex; align-items: center; justify-content: space-between; }
  label { display: grid; gap: 6px; font-size: 13px; color: var(--fg-muted); }
  label small { color: var(--fg-faint); }
  .row2 { display: grid; grid-template-columns: 1fr 1fr; gap: var(--space-3); }
  textarea { width: 100%; resize: vertical; min-height: 140px; padding: 8px 10px; border: 1px solid var(--border-strong); border-radius: var(--radius-md); background: var(--bg); color: var(--fg); line-height: 1.5; user-select: text; -webkit-user-select: text; }
  textarea:focus-visible { border-color: var(--accent); box-shadow: none; }
  .hint { margin: 0; font-size: 12px; color: var(--fg-faint); line-height: 1.5; }
  .count { float: right; }
  .servers { list-style: none; margin: 0; padding: 0; display: grid; gap: 6px; }
  .server { border: 1px solid var(--border); border-radius: var(--radius-md); padding: 6px 10px; }
  .server.on { border-color: var(--accent); background: var(--accent-soft); }
  .srow { display: flex; align-items: center; gap: var(--space-2); min-height: 30px; }
  .check { display: inline-flex; align-items: center; gap: 8px; color: var(--fg); min-width: 0; }
  .check input, .tool input { accent-color: var(--accent); margin: 0; }
  .sname { font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sstatus { font-size: 11.5px; color: var(--fg-faint); }
  .scount { margin-left: auto; font-size: 11.5px; color: var(--fg-muted); }
  .newtools { display: inline-flex; flex-direction: row; align-items: center; gap: 6px; font-size: 11.5px; color: var(--fg-faint); }
  .newtools select { width: auto; padding: 3px 8px; font-size: 12px; }
  .indent { padding-left: 24px; }
  .hint.warn { color: var(--warning); }
  .linkish { padding: 0; font-size: 12px; color: var(--accent); text-decoration: underline; }
  .tools { display: grid; gap: 2px; padding: 6px 0 4px 24px; max-height: 320px; overflow-y: auto; }
  .tool { display: flex; flex-direction: row; align-items: center; gap: 6px; font-size: 12.5px; color: var(--fg); min-width: 0; }
  .tool.off .tname { opacity: 0.55; }
  .toolacts { display: flex; justify-content: space-between; padding-left: 16px; }
  .tname { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tag { font-size: 10.5px; color: var(--fg-faint); flex: none; }
  .expand { margin: 2px 0 2px 16px; }
  .result { margin: 0; padding: 8px 10px; border-radius: var(--radius-md); font-size: 13px; }
  .result.ok { background: var(--accent-soft); }
  .result.bad { background: var(--danger-soft); color: var(--danger); }
  .actions { display: flex; align-items: center; gap: var(--space-2); }
  .spacer { flex: 1; }
</style>
