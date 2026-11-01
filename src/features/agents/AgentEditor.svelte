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
  import { describeError, type AgentServerAccess, type McpServerView, type ModelRef } from "$lib/ipc/commands";

  // Mounted inside {#key} by App, so the form initializes once per agent.
  let { agentId, onopenconnections }: { agentId: string | null; onopenconnections: () => void } = $props();

  // `initial` seeds the form once; `existing` tracks the stored agent (refreshed after saves).
  const initial = untrack(() => agentStore.byId(agentId));
  const existing = $derived(agentStore.byId(agentId));
  type ServerSel = { on: boolean; all: boolean; tools: string[] };

  function initialModel(): ModelRef | null {
    if (initial) return { ...initial.default_model };
    return providerStore.active ? { ...providerStore.active } : null;
  }

  let name = $state(initial?.name ?? "");
  let description = $state(initial?.description ?? "");
  let defaultModel = $state<ModelRef | null>(initialModel());
  let fallbackModel = $state<ModelRef | null>(initial?.fallback_model ? { ...initial.fallback_model } : null);
  let systemPrompt = $state(initial?.system_prompt ?? "");
  let selection = $state<Record<string, ServerSel>>(
    Object.fromEntries((initial?.servers ?? []).map((s) => [s.server_id, { on: true, all: s.tools === null, tools: [...(s.tools ?? [])] }])),
  );
  let busy = $state(false);
  let error = $state<string | null>(null);
  let savedNote = $state<string | null>(null);
  let openTools = $state<string | null>(null);

  const MAX_PROMPT = 32 * 1024;
  const canSave = $derived(name.trim().length > 0 && defaultModel !== null && !busy);

  function sel(id: string): ServerSel {
    return (selection[id] ??= { on: false, all: true, tools: [] });
  }

  function toggleServer(s: McpServerView, on: boolean) {
    const cur = sel(s.id);
    cur.on = on;
    if (on && openTools === null && !cur.all) openTools = s.id;
  }

  function toggleTool(serverId: string, tool: string, on: boolean) {
    const cur = sel(serverId);
    cur.tools = on ? [...new Set([...cur.tools, tool])] : cur.tools.filter((t) => t !== tool);
  }

  function selectedCount(s: McpServerView): string {
    const cur = selection[s.id];
    if (!cur?.on) return "";
    if (cur.all) return "all tools";
    return `${cur.tools.length} of ${s.tools.length || cur.tools.length} tools`;
  }

  function servers(): AgentServerAccess[] {
    return mcpStore.servers
      .filter((s) => selection[s.id]?.on)
      .map((s) => {
        const cur = selection[s.id]!;
        return { server_id: s.id, tools: cur.all ? null : [...cur.tools] };
      });
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
        <p class="hint">Choose which MCP servers and tools this agent may use. Tools set to <em>Ask</em> still require approval; disabled tools stay disabled.</p>
        <ul class="servers">
          {#each mcpStore.servers as s (s.id)}
            {@const cur = selection[s.id]}
            <li class="server" class:on={cur?.on}>
              <div class="srow">
                <label class="check">
                  <input type="checkbox" checked={cur?.on ?? false} onchange={(e) => toggleServer(s, e.currentTarget.checked)} />
                  <span class="sname">{s.name}</span>
                </label>
                <span class="sstatus">{s.status.kind === "connected" ? `${s.tools.length} tools` : s.enabled ? s.status.kind : "disabled"}</span>
                {#if cur?.on}
                  <span class="scount">{selectedCount(s)}</span>
                  <select class="mode" value={cur.all ? "all" : "some"} onchange={(e) => { cur.all = e.currentTarget.value === "all"; if (!cur.all) openTools = s.id; }} aria-label="Tool access for {s.name}">
                    <option value="all">All tools</option>
                    <option value="some">Selected tools</option>
                  </select>
                {/if}
              </div>
              {#if cur?.on && !cur.all}
                {@const names = [...new Set([...s.tools.map((t) => t.name), ...cur.tools])]}
                {#if openTools === s.id || cur.tools.length === 0}
                  <div class="tools">
                    {#each names as t (t)}
                      {@const info = s.tools.find((x) => x.name === t)}
                      <label class="tool" class:off={info?.policy === "block"} title={info?.description ?? (info ? t : "Not currently offered by the server")}>
                        <input type="checkbox" checked={cur.tools.includes(t)} onchange={(e) => toggleTool(s.id, t, e.currentTarget.checked)} />
                        <span class="tname">{info?.title ?? t}</span>
                        {#if !info}<span class="tag">unavailable</span>{:else if info.policy === "block"}<span class="tag">disabled</span>{:else if info.policy === "ask"}<span class="tag">asks</span>{/if}
                      </label>
                    {:else}
                      <p class="hint">Connect this server to list its tools.</p>
                    {/each}
                  </div>
                {:else}
                  <button type="button" class="btn-ghost btn-sm expand" onclick={() => (openTools = s.id)}>Edit tool selection…</button>
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
  .mode { width: auto; padding: 3px 8px; font-size: 12px; }
  .tools { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 2px 12px; padding: 6px 0 4px 24px; }
  .tool { display: flex; flex-direction: row; align-items: center; gap: 6px; font-size: 12.5px; color: var(--fg); min-width: 0; }
  .tool.off { opacity: 0.55; }
  .tname { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tag { font-size: 10.5px; color: var(--fg-faint); flex: none; }
  .expand { margin: 2px 0 2px 16px; }
  .result { margin: 0; padding: 8px 10px; border-radius: var(--radius-md); font-size: 13px; }
  .result.ok { background: var(--accent-soft); }
  .result.bad { background: var(--danger-soft); color: var(--danger); }
  .actions { display: flex; align-items: center; gap: var(--space-2); }
  .spacer { flex: 1; }
</style>
