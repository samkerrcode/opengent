<script lang="ts">
  import { tick } from "svelte";
  import { chatStore, type Chat, type TurnItem } from "./chat-store.svelte";
  import Composer from "./Composer.svelte";
  import AssistantTurnView from "./AssistantTurn.svelte";
  import { attachmentDataUrl } from "$lib/attachments";
  import { product } from "$lib/generated/product";
  import { providerStore } from "$features/providers/provider-store.svelte";

  let { chat, onopensettings }: { chat: Chat; onopensettings: () => void } = $props();

  let scroller = $state<HTMLElement | null>(null);
  let composer = $state<Composer | null>(null);
  let pinnedToBottom = $state(true);
  // Set on programmatic scrolls so the resulting scroll events are not mistaken for the user
  // scrolling away (content growth between scrollTo and the scroll event used to unpin).
  let expectedTop: number | null = null;
  let editingTitle = $state(false);
  let titleDraft = $state("");

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
    void (last?.kind === "assistant" ? last.turn.text.length : items.length);
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
      pinnedToBottom = true;
    }
    if (pinnedToBottom) tick().then(scrollToBottom);
  });

  $effect(() => {
    void chat.summary.id;
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
  {#if chat.summary.model}<span class="topbar-meta">{chat.summary.model}</span>{/if}
</header>

<div class="scroller" bind:this={scroller} onscroll={onScroll}>
  <div class="column">
    {#if !chat.loaded}
      <p class="loading">Loading…</p>
    {:else if chat.items.length === 0}
      <section class="empty">
        <div class="mark" aria-hidden="true"></div>
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
        <AssistantTurnView turn={item.turn} model={item.model} onretry={() => chatStore.retry(chat)} />
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
  onsend={(text) => chatStore.send(chat, text)}
  onstop={() => chatStore.stop(chat)}
  onattach={() => chatStore.pickAttachments(chat)}
  onremoveattachment={(id) => chatStore.removePending(chat, id)}
  {onopensettings}
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
    background: conic-gradient(from 200deg, var(--accent), var(--accent-soft) 70%);
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
