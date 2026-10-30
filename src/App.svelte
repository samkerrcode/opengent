<script lang="ts">
  import { onMount } from "svelte";
  import { commands, describeError, type AppInfo } from "$lib/ipc/commands";
  import { product } from "$lib/generated/product";
  import Sidebar from "$features/history/Sidebar.svelte";
  import ChatView from "$features/chat/ChatView.svelte";
  import { chatStore } from "$features/chat/chat-store.svelte";

  let appInfo = $state<AppInfo | null>(null);
  let bootError = $state<string | null>(null);

  onMount(async () => {
    try {
      appInfo = await commands.getAppInfo();
    } catch (e) {
      bootError = describeError(e);
    }
    if (chatStore.chats.length === 0) chatStore.newChat();
  });

  function onKeydown(e: KeyboardEvent) {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key.toLowerCase() === "n") {
      e.preventDefault();
      chatStore.newChat();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="shell">
  <Sidebar {appInfo} />
  <main class="canvas" aria-label="Conversation">
    {#if bootError}
      <div class="boot-error" role="alert">
        <strong>{product.displayName} could not start.</strong>
        <span>{bootError}</span>
      </div>
    {:else if chatStore.active}
      <ChatView chat={chatStore.active} />
    {/if}
  </main>
</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: var(--sidebar-width) 1fr;
    height: 100%;
  }
  .canvas {
    position: relative;
    min-width: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  .boot-error {
    margin: auto;
    display: grid;
    gap: var(--space-2);
    padding: var(--space-6);
    border: 1px solid var(--danger);
    border-radius: var(--radius-lg);
    max-width: 48ch;
  }
</style>
