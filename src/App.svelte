<script lang="ts">
  import { onMount } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { commands, describeError, type AppInfo } from "$lib/ipc/commands";
  import { product } from "$lib/generated/product";
  import Sidebar from "$features/history/Sidebar.svelte";
  import ChatView from "$features/chat/ChatView.svelte";
  import ProviderSettings from "$features/providers/ProviderSettings.svelte";
  import { chatStore } from "$features/chat/chat-store.svelte";
  import { providerStore } from "$features/providers/provider-store.svelte";

  let appInfo = $state<AppInfo | null>(null);
  let bootError = $state<string | null>(null);
  let settingsOpen = $state(false);

  onMount(async () => {
    try {
      appInfo = await commands.getAppInfo();
      await providerStore.load();
    } catch (e) {
      bootError = describeError(e);
    }
    if (chatStore.chats.length === 0) chatStore.newChat();
  });

  function onKeydown(e: KeyboardEvent) {
    const mod = e.metaKey || e.ctrlKey;
    if (!mod) return;
    if (e.key.toLowerCase() === "n") {
      e.preventDefault();
      chatStore.newChat();
    } else if (e.key === ",") {
      e.preventDefault();
      settingsOpen = !settingsOpen;
    }
  }

  // Links in rendered Markdown open in the system browser; the webview never navigates.
  function onClick(e: MouseEvent) {
    const anchor = (e.target as HTMLElement | null)?.closest("a[href]");
    if (!anchor) return;
    e.preventDefault();
    const href = anchor.getAttribute("href") ?? "";
    if (/^https?:\/\//i.test(href)) void openUrl(href);
  }
</script>

<svelte:window onkeydown={onKeydown} />
<svelte:document onclick={onClick} />

<div class="shell">
  <Sidebar {appInfo} onopensettings={() => (settingsOpen = true)} />
  <main class="canvas" aria-label="Conversation">
    {#if bootError}
      <div class="boot-error" role="alert">
        <strong>{product.displayName} could not start.</strong>
        <span>{bootError}</span>
      </div>
    {:else if chatStore.active}
      <ChatView chat={chatStore.active} onopensettings={() => (settingsOpen = true)} />
    {/if}
  </main>
</div>

{#if settingsOpen}
  <ProviderSettings onclose={() => (settingsOpen = false)} />
{/if}

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
