<script lang="ts">
  import { product } from "$lib/generated/product";
  import type { AppInfo } from "$lib/ipc/commands";
  import { chatStore } from "$features/chat/chat-store.svelte";

  let { appInfo }: { appInfo: AppInfo | null } = $props();

  const groups = $derived.by(() => {
    const today: typeof chatStore.chats = [];
    const earlier: typeof chatStore.chats = [];
    const startOfDay = new Date().setHours(0, 0, 0, 0);
    for (const c of chatStore.chats) (c.createdAt >= startOfDay ? today : earlier).push(c);
    return [
      { label: "Today", chats: today },
      { label: "Earlier", chats: earlier },
    ].filter((g) => g.chats.length > 0);
  });
</script>

<nav class="sidebar" aria-label="Chats">
  <header class="brand">
    <span class="logo" aria-hidden="true"></span>
    <span class="name">{product.displayName}</span>
  </header>

  <button class="btn-outline new-chat" onclick={() => chatStore.newChat()} title="New chat (⌘N / Ctrl+N)">
    <span aria-hidden="true">+</span> New chat
  </button>

  <div class="history">
    {#each groups as group (group.label)}
      <div class="group-label" id="group-{group.label}">{group.label}</div>
      <ul class="group" aria-labelledby="group-{group.label}">
        {#each group.chats as chat (chat.id)}
          <li>
            <button
              class="chat-item"
              class:active={chat.id === chatStore.activeId}
              aria-current={chat.id === chatStore.activeId ? "page" : undefined}
              onclick={() => chatStore.select(chat.id)}
            >
              <span class="chat-title">{chat.title}</span>
              {#if chat.activeRunId}<span class="dot" role="img" aria-label="generating"></span>{/if}
            </button>
          </li>
        {/each}
      </ul>
    {/each}
  </div>

  <footer class="status">
    {#if appInfo}
      <div>v{appInfo.product.version} · {appInfo.platform}</div>
      <div title={appInfo.data_root}>Data: {appInfo.product.data_dir_name}</div>
      <div>Secrets: {appInfo.secret_backend}</div>
    {:else}
      <div>Connecting to host…</div>
    {/if}
  </footer>
</nav>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4) var(--space-3);
    background: var(--bg-elevated);
    border-right: 1px solid var(--border);
    min-height: 0;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 0 var(--space-2);
    font-weight: 650;
    letter-spacing: 0.01em;
  }
  .logo {
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: conic-gradient(from 200deg, var(--accent), transparent 70%);
  }
  .new-chat {
    text-align: left;
    padding: 8px 12px;
  }
  .history {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-height: 0;
  }
  .group-label {
    font-size: 11.5px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-faint);
    padding: var(--space-3) var(--space-2) var(--space-1);
  }
  .group {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .chat-item {
    width: 100%;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    text-align: left;
    padding: 7px 10px;
    border-radius: var(--radius-md);
    color: var(--fg-muted);
  }
  .chat-item:hover {
    background: var(--bg-sunken);
    color: var(--fg);
  }
  .chat-item.active {
    background: var(--accent-soft);
    color: var(--fg);
  }
  .chat-title {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    animation: pulse 1.2s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dot {
      animation: none;
    }
  }
  .status {
    font-size: 12px;
    color: var(--fg-faint);
    padding: var(--space-2);
    border-top: 1px solid var(--border);
    display: grid;
    gap: 2px;
  }
</style>
