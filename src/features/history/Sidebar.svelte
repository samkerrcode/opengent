<script lang="ts">
  import { product } from "$lib/generated/product";
  import type { AppInfo } from "$lib/ipc/commands";
  import { chatStore } from "$features/chat/chat-store.svelte";

  let { appInfo, onopensettings, onopenconnections }: { appInfo: AppInfo | null; onopensettings: () => void; onopenconnections: () => void } = $props();

  const groups = $derived.by(() => {
    const now = Date.now();
    const startOfDay = new Date().setHours(0, 0, 0, 0);
    const week = startOfDay - 6 * 86_400_000;
    const buckets: { label: string; chats: typeof chatStore.chats }[] = [
      { label: "Today", chats: [] },
      { label: "Previous 7 days", chats: [] },
      { label: "Older", chats: [] },
    ];
    for (const c of chatStore.chats) {
      const t = Math.min(c.summary.updated_at, now);
      (t >= startOfDay ? buckets[0] : t >= week ? buckets[1] : buckets[2])!.chats.push(c);
    }
    return buckets.filter((g) => g.chats.length > 0);
  });
</script>

<nav class="sidebar" aria-label="Chats">
  <header class="brand">
    <span class="logo" aria-hidden="true"></span>
    <span class="name">{product.displayName}</span>
  </header>

  <button class="new-chat" onclick={() => chatStore.newChat()} title="New chat (⌘N / Ctrl+N)">
    <span class="plus" aria-hidden="true">+</span> New chat
    <kbd>⌘N</kbd>
  </button>

  <label class="search">
    <span class="visually-hidden">Search chats</span>
    <input type="text" placeholder="Search chats" value={chatStore.query} oninput={(e) => chatStore.setQuery(e.currentTarget.value)} />
  </label>

  <div class="history">
    {#each groups as group (group.label)}
      <div class="group-label" id="group-{group.label}">{group.label}</div>
      <ul class="group" aria-labelledby="group-{group.label}">
        {#each group.chats as chat (chat.summary.id)}
          <li class="row" class:active={chat.summary.id === chatStore.activeId}>
            <button
              class="chat-item"
              aria-current={chat.summary.id === chatStore.activeId ? "page" : undefined}
              onclick={() => chatStore.select(chat.summary.id)}
            >
              {#if chat.activeRunId}<span class="dot" role="img" aria-label="generating"></span>{/if}
              <span class="chat-title">{chat.summary.title}</span>
            </button>
            <button
              class="delete"
              aria-label="Delete chat"
              title="Delete"
              onclick={() => confirm(`Delete "${chat.summary.title}"? This cannot be undone.`) && chatStore.remove(chat.summary.id)}
            >✕</button>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="none">{chatStore.query ? "No matching chats." : "No chats yet."}</p>
    {/each}
  </div>

  <footer class="foot">
    <button class="settings" onclick={onopensettings} title="Providers & settings (⌘,)">
      <span aria-hidden="true">⚙</span> Providers
    </button>
    <button class="settings" onclick={onopenconnections} title="MCP tool servers">
      <span aria-hidden="true">⚒</span> Connections
    </button>
    {#if appInfo}
      <div class="status" title={appInfo.data_root}>
        v{appInfo.product.version} · {appInfo.secret_backend}
      </div>
    {/if}
  </footer>
</nav>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4) var(--space-3) var(--space-3);
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
    font-size: 15px;
  }
  .logo {
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: conic-gradient(from 200deg, var(--accent), var(--accent-soft) 70%);
  }
  .new-chat {
    justify-content: flex-start;
    padding: 8px 10px;
    border: 1px solid var(--border);
    background: var(--bg);
    font-weight: 500;
  }
  .new-chat:hover {
    border-color: var(--border-strong);
  }
  .plus {
    font-size: 16px;
    line-height: 1;
    color: var(--accent);
  }
  kbd {
    margin-left: auto;
    font: inherit;
    font-size: 11px;
    color: var(--fg-faint);
  }
  .search input {
    padding: 6px 10px;
    font-size: 13px;
    background: var(--bg);
    border-color: transparent;
  }
  .search input:hover,
  .search input:focus-visible {
    border-color: var(--border-strong);
  }
  .history {
    flex: 1;
    overflow-y: auto;
    min-height: 0;
  }
  .group-label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--fg-faint);
    padding: var(--space-3) var(--space-2) var(--space-1);
  }
  .group {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .row {
    display: flex;
    align-items: center;
    border-radius: var(--radius-md);
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.active {
    background: var(--accent-soft);
  }
  .chat-item {
    flex: 1;
    min-width: 0;
    justify-content: flex-start;
    padding: 7px 10px;
    color: var(--fg-muted);
    border-radius: var(--radius-md);
  }
  .row.active .chat-item,
  .row:hover .chat-item {
    color: var(--fg);
  }
  .chat-title {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .delete {
    padding: 4px 8px;
    color: var(--fg-faint);
    opacity: 0;
    border-radius: var(--radius-sm);
  }
  .row:hover .delete,
  .delete:focus-visible {
    opacity: 1;
  }
  .delete:hover {
    color: var(--danger);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    animation: pulse 1.2s ease-in-out infinite;
    flex: none;
  }
  .none {
    color: var(--fg-faint);
    font-size: 13px;
    padding: var(--space-3) var(--space-2);
  }
  .foot {
    display: grid;
    gap: var(--space-2);
    border-top: 1px solid var(--border);
    padding-top: var(--space-3);
  }
  .settings {
    justify-content: flex-start;
    color: var(--fg-muted);
  }
  .settings:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .status {
    font-size: 11.5px;
    color: var(--fg-faint);
    padding: 0 var(--space-2);
  }
  @media (prefers-reduced-motion: reduce) {
    .dot {
      animation: none;
    }
  }
</style>
