<script lang="ts">
  import { onMount } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { commands, describeError, type AppInfo } from "$lib/ipc/commands";
  import { product } from "$lib/generated/product";
  import Sidebar from "$features/history/Sidebar.svelte";
  import ChatView from "$features/chat/ChatView.svelte";
  import ProviderSettings from "$features/providers/ProviderSettings.svelte";
  import ContextMenu, { type ContextMenuItem } from "$lib/components/ContextMenu.svelte";
  import { copyText } from "$lib/clipboard";
  import { chatStore } from "$features/chat/chat-store.svelte";
  import { providerStore } from "$features/providers/provider-store.svelte";

  let appInfo = $state<AppInfo | null>(null);
  let bootError = $state<string | null>(null);
  let settingsOpen = $state(false);

  onMount(async () => {
    try {
      appInfo = await commands.getAppInfo();
      await Promise.all([providerStore.load(), chatStore.loadList()]);
    } catch (e) {
      bootError = describeError(e);
    }
    const first = chatStore.chats[0];
    if (first) await chatStore.select(first.summary.id);
    else await chatStore.newChat();
  });

  function onKeydown(e: KeyboardEvent) {
    const mod = e.metaKey || e.ctrlKey;
    if (!mod) return;
    if (e.key.toLowerCase() === "n") {
      e.preventDefault();
      void chatStore.newChat();
    } else if (e.key === ",") {
      e.preventDefault();
      settingsOpen = !settingsOpen;
    }
  }

  let ctxMenu = $state<{ x: number; y: number; items: ContextMenuItem[] } | null>(null);

  // Suppress the webview's browser-style context menu (Reload / Inspect Element) so the app
  // feels native. Editable fields keep their native editing menu; .selectable regions (chat
  // messages) get a minimal app-styled menu of real text operations instead — the native
  // text menu can't be filtered and would still leak "Inspect Element" in dev builds.
  function onContextMenu(e: MouseEvent) {
    const el = e.target as HTMLElement | null;
    if (el?.closest("input, textarea, [contenteditable='true']")) {
      ctxMenu = null;
      return;
    }
    e.preventDefault();
    const region = el?.closest(".selectable");
    if (!region) {
      ctxMenu = null;
      return;
    }
    const selection = window.getSelection()?.toString() ?? "";
    ctxMenu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        {
          label: "Copy",
          disabled: !selection,
          action: () => void copyText(selection),
        },
        {
          label: "Select All",
          action: () => window.getSelection()?.selectAllChildren(region),
        },
      ],
    };
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

<svelte:window onkeydown={onKeydown} oncontextmenu={onContextMenu} />
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
    {#if chatStore.error}
      <div class="toast" role="alert">
        <span>{chatStore.error}</span>
        <button class="btn-ghost btn-sm" onclick={() => (chatStore.error = null)} aria-label="Dismiss">✕</button>
      </div>
    {/if}
  </main>
</div>

{#if settingsOpen}
  <ProviderSettings onclose={() => (settingsOpen = false)} />
{/if}

{#if ctxMenu}
  <ContextMenu x={ctxMenu.x} y={ctxMenu.y} items={ctxMenu.items} onclose={() => (ctxMenu = null)} />
{/if}

<style>
  .shell {
    display: grid;
    grid-template-columns: var(--sidebar-width) 1fr;
    /* Explicit bounded row: an implicit auto row can grow with the canvas's content past
       100% height, which scrolls the document and takes the sidebar with it. */
    grid-template-rows: minmax(0, 1fr);
    height: 100%;
    /* App shell never scrolls at the document level; inner regions scroll themselves. */
    overflow: hidden;
  }
  .canvas {
    position: relative;
    min-width: 0;
    /* Grid item min-height defaults to auto — without this it can force the row taller
       than the viewport and push the shell into document scroll. */
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  .toast {
    position: absolute;
    left: 50%;
    bottom: var(--space-4);
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 8px 10px 8px 14px;
    border-radius: var(--radius-lg);
    background: var(--bg-elevated);
    border: 1px solid var(--danger);
    color: var(--danger);
    box-shadow: var(--shadow-2);
    font-size: 13px;
    z-index: 5;
    max-width: 70%;
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
