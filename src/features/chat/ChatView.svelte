<script lang="ts">
  import { tick } from "svelte";
  import { chatStore, type Chat, type TurnItem } from "./chat-store.svelte";
  import Composer from "./Composer.svelte";
  import AssistantTurnView from "./AssistantTurn.svelte";
  import { attachmentDataUrl } from "$lib/attachments";
  import { product } from "$lib/generated/product";
  import BrandMark from "$lib/components/BrandMark.svelte";
  import { providerStore } from "$features/providers/provider-store.svelte";
  import { nav } from "$lib/state/nav.svelte";

  let { chat, onopensettings, onopenconnections }: { chat: Chat; onopensettings: () => void; onopenconnections: () => void } = $props();

  let scroller = $state<HTMLElement | null>(null);
  let composer = $state<Composer | null>(null);
  let pinnedToBottom = $state(true);
  // Set on programmatic scrolls so the resulting scroll events are not mistaken for the user
  // scrolling away (content growth between scrollTo and the scroll event used to unpin).
  let expectedTop: number | null = null;
  let editingTitle = $state(false);
  let titleDraft = $state("");

  const agent = $derived(chatStore.agentFor(chat));

  const suggestions = [
    "Explain the difference between threads and async tasks",
    "Draft a concise project status update",
    "Summarize the attached file in five bullets",
  ];

  function scrollToBottom() {
    if (!scroller) return;
    expectedTop = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
    scroller.scrollTop = expectedTop;
  }

  function onScroll() {
    if (!scroller) return;
    if (expectedTop !== null && Math.abs(scroller.scrollTop - expectedTop) < 4) return;
    expectedTop = null;
    pinnedToBottom = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 48;
  }

  // Follow new output while the user is pinned to the bottom. Sending a message always
  // re-pins — it is explicit intent to watch the response, regardless of scroll position.
  let seenUserId: string | null = null;
  $effect(() => {
    const items = chat.items;
    const last = items[items.length - 1];
    const tail = last?.kind === "assistant" ? last.turn.segments.at(-1) : undefined;
    void (tail && tail.kind !== "tool" ? tail.text.length : last?.kind === "assistant" ? last.turn.segments.length : items.length);
    let lastUser: TurnItem | null = null;
    for (let i = items.length - 1; i >= 0; i--) {
      const item = items[i];
      if (item?.kind === "user") {
        lastUser = item;
        break;
      }
    }
    if (lastUser && lastUser.id !== seenUserId) {
      seenUserId = lastUser.id;
      // Only a freshly sent message (still carrying its optimistic local- id) counts as
      // explicit intent to follow. The later swap to the real message id — or a refetch
      // replacing items — must not override a user who scrolled up.
      if (lastUser.id.startsWith("local-")) pinnedToBottom = true;
    }
    // Re-check inside the callback: the scroll may resolve after the user scrolled away.
    if (pinnedToBottom) tick().then(() => { if (pinnedToBottom) scrollToBottom(); });
  });

  // Runs on chat switch only. refresh()/loadList() reassign chat.summary, so a bare
  // `chat.summary.id` dependency would re-fire — re-pinning and snapping to the bottom —
  // every time a run completes.
  let viewedChatId: string | null = null;
  $effect(() => {
    const id = chat.summary.id;
    if (id === viewedChatId) return;
    viewedChatId = id;
    pinnedToBottom = true;
    seenUserId = null;
    editingTitle = false;
    tick().then(() => {
      scrollToBottom();
      composer?.focus();
    });
  });

  function jumpToLatest() {
    pinnedToBottom = true;
    scroller?.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" });
  }

  function startRename() {
    titleDraft = chat.summary.title;
    editingTitle = true;
  }
  async function commitRename() {
    editingTitle = false;
    const t = titleDraft.trim();
    if (t && t !== chat.summary.title) await chatStore.rename(chat, t);
  }
</script>

<header class="topbar">
  {#if editingTitle}
    <input
      class="title-input"
      type="text"
      bind:value={titleDraft}
      maxlength="120"
      onblur={commitRename}
      onkeydown={(e) => (e.key === "Enter" ? commitRename() : e.key === "Escape" ? (editingTitle = false) : null)}
      aria-label="Chat title"
    />
  {:else}
    <button class="title" onclick={startRename} title="Rename">{chat.summary.title}</button>
  {/if}
  {#if agent}
    <button class="agent-badge" onclick={() => nav.editAgent(agent.id)} title="Configure {agent.name}">
      <span class="agent-dot" aria-hidden="true">{agent.name.slice(0, 1).toUpperCase()}</span>{agent.name}
    </button>
    {#if chat.summary.schedule_id}<span class="topbar-meta" title="Started by a schedule">⏱ scheduled</span>{/if}
  {/if}
  {#if chat.summary.model}<span class="topbar-meta">{chat.summary.model}</span>{/if}
</header>

<div class="scroller" bind:this={scroller} onscroll={onScroll}>
  <div class="column">
    {#if !chat.loaded}
      <p class="loading">Loading…</p>
    {:else if chat.items.length === 0 && agent}
      <section class="empty">
        <div class="mark agent-mark" aria-hidden="true">{agent.name.slice(0, 1).toUpperCase()}</div>
        <h1>{agent.name}</h1>
        {#if agent.description}<p>{agent.description}</p>{/if}
        <p class="agent-meta">
          Answers with <strong>{agent.default_model.model}</strong>{#if agent.fallback_model}, falling back to <strong>{agent.fallback_model.model}</strong>{/if}
          · {agent.servers.length ? `${agent.servers.length} tool server${agent.servers.length === 1 ? "" : "s"}` : "no tools"}
        </p>
        <button class="btn-ghost btn-sm" onclick={() => nav.editAgent(agent.id)}>Configure agent</button>
      </section>
    {:else if chat.items.length === 0}
      <section class="empty">
        <div class="mark"><BrandMark size={40} /></div>
        <h1>What are we working on?</h1>
        {#if providerStore.activeProvider?.kind === "simulated"}
          <p>
            You're on the built-in simulated model, so replies are synthetic. Connect a real provider to get
            started — local (Ollama, LM Studio) or hosted.
          </p>
          <button class="btn-primary" onclick={onopensettings}>Connect a provider</button>
        {:else if providerStore.active}
          <p>Talking to <strong>{providerStore.active.model}</strong> via {providerStore.activeProvider?.name}.</p>
        {:else}
          <p>{product.displayName} works with any model provider. Pick one to begin.</p>
          <button class="btn-primary" onclick={onopensettings}>Choose a provider</button>
        {/if}
        <ul class="suggestions">
          {#each suggestions as s (s)}
            <li><button class="suggestion" onclick={() => chatStore.send(chat, s)}>{s}</button></li>
          {/each}
        </ul>
      </section>
    {/if}

    {#each chat.items as item (item.id)}
      {#if item.kind === "user"}
        <article class="user selectable" aria-label="You">
          {#if item.attachments.length}
            <ul class="user-files">
              {#each item.attachments as a (a.id)}
                {#if a.mime.startsWith("image/")}
                  {#await attachmentDataUrl(a.id) then url}
                    {#if url}
                      <li class="user-image"><img src={url} alt={a.name} title={a.name} /></li>
                    {:else}
                      <li title={a.name}>▤ {a.name}</li>
                    {/if}
                  {/await}
                {:else}
                  <li title="{a.char_count} characters">▤ {a.name}</li>
                {/if}
              {/each}
            </ul>
          {/if}
          {#if item.text}<p>{item.text}</p>{/if}
        </article>
      {:else}
        <AssistantTurnView turn={item.turn} model={item.turn.switchedModel ?? item.model} onretry={() => chatStore.retry(chat)} />
      {/if}
    {/each}
  </div>
</div>

{#if !pinnedToBottom}
  <button class="btn-outline jump" onclick={jumpToLatest}>Jump to latest ↓</button>
{/if}

<Composer
  bind:this={composer}
  busy={chat.activeRunId !== null}
  pending={chat.pending}
  {agent}
  supportsImages={chatStore.supportsImages(chat)}
  onopenagent={() => agent && nav.editAgent(agent.id)}
  onsend={(text) => chatStore.send(chat, text)}
  onstop={() => chatStore.stop(chat)}
  onattach={() => chatStore.pickAttachments(chat)}
  onremoveattachment={(id) => chatStore.removePending(chat, id)}
  {onopensettings}
  {onopenconnections}
/>

<style>
  .topbar {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: 10px var(--space-6);
    border-bottom: 1px solid var(--border);
    min-height: 44px;
  }
  .title {
    font-weight: 600;
    padding: 4px 8px;
    margin-left: -8px;
    max-width: 60ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    display: block;
  }
  .title:hover {
    background: var(--bg-hover);
  }
  .title-input {
    max-width: 60ch;
    padding: 4px 8px;
    margin-left: -8px;
    font-weight: 600;
  }
  .agent-badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 2px 10px 2px 3px;
    border-radius: 999px;
    background: var(--accent-soft);
    font-size: 12.5px;
    font-weight: 500;
    flex: none;
  }
  .agent-badge:hover {
    border-color: var(--accent);
  }
  .agent-dot {
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--accent);
    color: var(--accent-fg);
    font-size: 10.5px;
    font-weight: 700;
    display: grid;
    place-items: center;
  }
  .agent-mark {
    display: grid;
    place-items: center;
    background: var(--accent);
    color: var(--accent-fg);
    font-weight: 700;
    font-size: 18px;
  }
  .agent-meta {
    font-size: 13px;
  }
  .topbar-meta {
    font-size: 12px;
    color: var(--fg-faint);
  }
  .scroller {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    min-height: 0;
  }
  .column {
    max-width: var(--measure);
    margin: 0 auto;
    padding: var(--space-8) var(--space-6) var(--space-6);
    display: flex;
    flex-direction: column;
    gap: var(--space-6);
  }
  .loading {
    color: var(--fg-faint);
    text-align: center;
    margin-top: 20vh;
  }
  .empty {
    margin-top: 10vh;
    color: var(--fg-muted);
    display: grid;
    gap: var(--space-3);
    justify-items: start;
  }
  .mark {
    width: 40px;
    height: 40px;
    border-radius: 50%;
    margin-bottom: var(--space-2);
  }
  .empty h1 {
    font-size: 28px;
    font-weight: 600;
    color: var(--fg);
    margin: 0;
    letter-spacing: -0.015em;
  }
  .empty p {
    margin: 0;
    max-width: 56ch;
  }
  .suggestions {
    list-style: none;
    padding: 0;
    margin: var(--space-4) 0 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .suggestion {
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    border-radius: var(--radius-lg);
    padding: 8px 12px;
    color: var(--fg-muted);
    font-size: 13px;
    text-align: left;
  }
  .suggestion:hover {
    border-color: var(--border-strong);
    color: var(--fg);
  }
  .user {
    align-self: flex-end;
    max-width: 80%;
    min-width: 0;
    background: var(--accent-soft);
    border-radius: var(--radius-xl) var(--radius-xl) var(--radius-sm) var(--radius-xl);
    padding: 10px 14px;
    font-size: 15px;
    display: grid;
    gap: 6px;
  }
  .user p {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .user-files {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    font-size: 12.5px;
  }
  .user-files li {
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
  }
  .user-files .user-image {
    padding: 0;
    border: none;
    background: transparent;
  }
  .user-image img {
    display: block;
    max-width: 240px;
    max-height: 180px;
    border-radius: var(--radius-lg);
    border: 1px solid var(--border);
  }
  .jump {
    position: absolute;
    left: 50%;
    transform: translateX(-50%);
    bottom: 140px;
    border-radius: 999px;
    box-shadow: var(--shadow-2);
    z-index: 2;
  }
  /* Keep the pill opaque on hover: btn-outline's hover swaps in the translucent --bg-hover
     wash, which lets dense content underneath (KaTeX math, code) bleed through. */
  .jump.btn-outline:not(:disabled):hover {
    background: var(--bg-elevated);
    border-color: var(--accent);
  }
</style>
