<script lang="ts">
  import { tick } from "svelte";
  import { chatStore, type Chat } from "./chat-store.svelte";
  import Composer from "./Composer.svelte";
  import AssistantTurnView from "./AssistantTurn.svelte";
  import { product } from "$lib/generated/product";

  let { chat }: { chat: Chat } = $props();

  let scroller = $state<HTMLElement | null>(null);
  let pinnedToBottom = $state(true);

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

  function jumpToLatest() {
    scroller?.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" });
    pinnedToBottom = true;
  }
</script>

<div class="scroller" bind:this={scroller} onscroll={onScroll}>
  <div class="column">
    {#if chat.items.length === 0}
      <section class="empty">
        <h1>What are we working on?</h1>
        <p>
          {product.displayName} streams responses from whichever model provider you configure. This build runs a
          simulated provider so you can try the interface without an account.
        </p>
      </section>
    {/if}

    {#each chat.items as item (item.id)}
      {#if item.kind === "user"}
        <article class="user selectable" aria-label="You">
          <p>{item.text}</p>
        </article>
      {:else}
        <AssistantTurnView turn={item.turn} />
      {/if}
    {/each}
  </div>
</div>

{#if !pinnedToBottom}
  <button class="btn-outline jump" onclick={jumpToLatest}>Jump to latest ↓</button>
{/if}

<Composer
  busy={chat.activeRunId !== null}
  onsend={(text) => chatStore.send(chat, text)}
  onstop={() => chatStore.stop(chat)}
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
    margin-top: 18vh;
    color: var(--fg-muted);
  }
  .empty h1 {
    font-size: 26px;
    font-weight: 600;
    color: var(--fg);
    margin: 0 0 var(--space-3);
    letter-spacing: -0.01em;
  }
  .user {
    align-self: flex-end;
    max-width: 85%;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: var(--space-3) var(--space-4);
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
    bottom: 124px;
    background: var(--bg-elevated);
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.18);
  }
</style>
