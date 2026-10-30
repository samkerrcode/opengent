<script lang="ts">
  import { tick } from "svelte";
  import { chatStore, type Chat } from "./chat-store.svelte";
  import Composer from "./Composer.svelte";
  import AssistantTurnView from "./AssistantTurn.svelte";
  import { product } from "$lib/generated/product";
  import { providerStore } from "$features/providers/provider-store.svelte";

  let { chat, onopensettings }: { chat: Chat; onopensettings: () => void } = $props();

  let scroller = $state<HTMLElement | null>(null);
  let composer = $state<Composer | null>(null);
  let pinnedToBottom = $state(true);

  const suggestions = [
    "Explain the difference between threads and async tasks",
    "Draft a concise project status update",
    "Review this function for edge cases",
  ];

  function onScroll() {
    if (!scroller) return;
    pinnedToBottom = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 48;
  }

  // Follow new output only while the user is already at the bottom.
  $effect(() => {
    const last = chat.items.at(-1);
    void (last?.kind === "assistant" ? last.turn.text.length : chat.items.length);
    if (pinnedToBottom) tick().then(() => scroller?.scrollTo({ top: scroller.scrollHeight }));
  });

  $effect(() => {
    void chat.id;
    pinnedToBottom = true;
    tick().then(() => composer?.focus());
  });

  function jumpToLatest() {
    scroller?.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" });
    pinnedToBottom = true;
  }
</script>

<div class="scroller" bind:this={scroller} onscroll={onScroll}>
  <div class="column">
    {#if chat.items.length === 0}
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
          <p>{item.text}</p>
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
  onsend={(text) => chatStore.send(chat, text)}
  onstop={() => chatStore.stop(chat)}
  {onopensettings}
/>

<style>
  .scroller {
    flex: 1;
    overflow-y: auto;
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
  .empty {
    margin-top: 12vh;
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
    background: var(--accent-soft);
    border-radius: var(--radius-xl) var(--radius-xl) var(--radius-sm) var(--radius-xl);
    padding: 10px 14px;
    font-size: 15px;
  }
  .user p {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
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
</style>
