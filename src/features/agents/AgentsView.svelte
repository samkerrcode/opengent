<script lang="ts">
  import { agentStore } from "./agent-store.svelte";
  import { fmtWhen } from "./schedule-format";
  import { nav } from "$lib/state/nav.svelte";
  import { chatStore } from "$features/chat/chat-store.svelte";
  import type { Agent } from "$lib/ipc/commands";
  import BrandMark from "$lib/components/BrandMark.svelte";

  async function chatWith(agent: Agent) {
    if (await chatStore.newChat(agent.id)) nav.showChat();
  }

  function toolSummary(a: Agent): string {
    if (a.servers.length === 0) return "No tools";
    const n = a.servers.length;
    return `${n} tool server${n === 1 ? "" : "s"}`;
  }
</script>

<header class="topbar">
  <h1>Agents</h1>
  {#if agentStore.agents.length}
    <button class="btn-primary btn-sm" onclick={() => nav.editAgent(null)}>+ New agent</button>
  {/if}
</header>

<div class="scroller">
  <div class="column">
    {#if !agentStore.loaded}
      <p class="muted center">Loading…</p>
    {:else if agentStore.agents.length === 0}
      <section class="intro">
        <BrandMark size={48} />
        <h2>Build agents that work your way</h2>
        <p class="lede">
          An agent is a reusable assistant with its own purpose, models, instructions and tools. Chat with it any time, or
          let it run on a schedule.
        </p>
        <ul class="features">
          <li>
            <span class="ico" aria-hidden="true">✎</span>
            <div><strong>Give it a role</strong><span>Write instructions once — they're layered on top of a built-in agent prompt.</span></div>
          </li>
          <li>
            <span class="ico" aria-hidden="true">⇄</span>
            <div><strong>Pick models</strong><span>A default model plus an optional fallback if the default is unavailable.</span></div>
          </li>
          <li>
            <span class="ico" aria-hidden="true">⚒</span>
            <div><strong>Choose its tools</strong><span>Limit it to specific MCP servers and tools. Your permission rules still apply.</span></div>
          </li>
          <li>
            <span class="ico" aria-hidden="true">⏱</span>
            <div><strong>Schedule it</strong><span>Run a preset prompt on a timetable, and decide what happens to runs missed while away.</span></div>
          </li>
        </ul>
        <button class="btn-primary create" onclick={() => nav.editAgent(null)}>Create your first agent</button>
      </section>
    {:else}
      {#if agentStore.error}<p class="error" role="alert">{agentStore.error}</p>{/if}
      <ul class="grid">
        {#each agentStore.agents as a (a.id)}
          {@const next = agentStore.nextRun(a.id)}
          <li class="card">
            <button class="card-main" onclick={() => nav.editAgent(a.id)} title="Edit {a.name}">
              <span class="avatar" aria-hidden="true">{a.name.slice(0, 1).toUpperCase()}</span>
              <span class="card-text">
                <span class="name">{a.name}</span>
                <span class="desc">{a.description || "No description"}</span>
              </span>
            </button>
            <div class="meta">
              <span class="chip" title={a.fallback_model ? `Fallback: ${a.fallback_model.model}` : "No fallback model"}>
                {a.default_model.model}{a.fallback_model ? " +1" : ""}
              </span>
              <span class="chip">{toolSummary(a)}</span>
              {#if next !== null}<span class="chip accent" title="Next scheduled run">⏱ {fmtWhen(next)}</span>{/if}
            </div>
            <div class="actions">
              <button class="btn-ghost btn-sm" onclick={() => nav.editAgent(a.id)}>Configure</button>
              <button class="btn-outline btn-sm" onclick={() => chatWith(a)}>Chat</button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</div>

<style>
  .topbar { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); padding: 10px var(--space-6); border-bottom: 1px solid var(--border); min-height: 44px; }
  h1 { margin: 0; font-size: 14px; font-weight: 600; }
  .scroller { flex: 1; overflow-y: auto; min-height: 0; }
  .column { max-width: 960px; margin: 0 auto; padding: var(--space-8) var(--space-6); }
  .muted { color: var(--fg-faint); }
  .center { text-align: center; margin-top: 20vh; }
  .error { color: var(--danger); background: var(--danger-soft); padding: 8px 10px; border-radius: var(--radius-md); margin: 0 0 var(--space-4); }
  .intro { max-width: 600px; margin: 6vh auto 0; display: grid; gap: var(--space-4); justify-items: center; text-align: center; }
  .intro h2 { margin: 0; font-size: 26px; font-weight: 600; letter-spacing: -0.015em; }
  .lede { margin: 0; color: var(--fg-muted); max-width: 52ch; }
  .features { list-style: none; padding: 0; margin: var(--space-2) 0; display: grid; grid-template-columns: 1fr 1fr; gap: var(--space-3); text-align: left; width: 100%; }
  .features li { display: flex; gap: var(--space-3); padding: var(--space-3); border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--bg-elevated); }
  .features div { display: grid; gap: 2px; font-size: 13px; }
  .features span:not(.ico) { color: var(--fg-muted); }
  .ico { flex: none; width: 28px; height: 28px; border-radius: var(--radius-md); background: var(--accent-soft); color: var(--accent); display: grid; place-items: center; }
  .create { padding: 9px 18px; border-radius: 999px; }
  .grid { list-style: none; padding: 0; margin: 0; display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: var(--space-3); }
  .card { border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--bg-elevated); padding: var(--space-3); display: grid; gap: var(--space-3); }
  .card:hover { border-color: var(--border-strong); }
  .card-main { display: flex; align-items: flex-start; gap: var(--space-3); padding: 0; text-align: left; width: 100%; }
  .avatar { flex: none; width: 34px; height: 34px; border-radius: var(--radius-md); background: var(--accent-soft); color: var(--accent); font-weight: 650; display: grid; place-items: center; }
  .card-text { display: grid; gap: 2px; min-width: 0; }
  .name { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .desc { font-size: 12.5px; color: var(--fg-muted); display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .meta { display: flex; flex-wrap: wrap; gap: 6px; }
  .meta .chip { max-width: 100%; overflow: hidden; text-overflow: ellipsis; }
  .actions { display: flex; justify-content: flex-end; gap: 6px; }
</style>
