<script lang="ts">
  import { mcpStore } from "$features/mcp/mcp-store.svelte";
  import ToolAccessBadge from "$features/mcp/ToolAccessBadge.svelte";
  import { chatStore } from "./chat-store.svelte";
  import type { ToolCallStatus } from "$lib/ipc/commands";
  import type { ToolSegment } from "$lib/state/run-reducer";

  let { segment, runId }: { segment: ToolSegment; runId: string | null } = $props();

  const labels: Record<ToolCallStatus, string> = {
    pending_approval: "Waiting for approval",
    running: "Running",
    completed: "Done",
    failed: "Failed",
    denied: "Denied",
    blocked: "Disabled",
    cancelled: "Cancelled",
  };

  const awaiting = $derived(segment.status === "pending_approval" && runId !== null);
  const isSearch = $derived(segment.call.name === "find_tools");
  const title = $derived(
    isSearch ? "find_tools — search available tools" : `${segment.call.server ?? segment.call.name} · ${segment.call.tool}`,
  );

  // Server-declared hints for the approval prompt: looked up live so they reflect the
  // current tool definition. Untrusted metadata — informational only.
  const info = $derived(
    segment.call.server
      ? mcpStore.servers
          .find((s) => s.name === segment.call.server)
          ?.tools.find((t) => t.name === segment.call.tool)
      : undefined,
  );

  function pretty(v: unknown): string {
    if (v == null) return "";
    try {
      return JSON.stringify(v, null, 2);
    } catch {
      return String(v);
    }
  }

  // Mirrors the Rust flattening: text parts joined, binary parts labeled.
  function resultText(content: unknown): string {
    if (typeof content === "string") return content;
    if (!Array.isArray(content)) return pretty(content);
    const parts = content
      .map((p) => {
        if (p?.type === "text") return p.text ?? "";
        if (p?.type === "loaded_tools") return `Loaded: ${(p.names ?? []).join(", ")}`;
        if (p?.type === "image" || p?.type === "audio") return `[${p.type} omitted]`;
        if (p?.type === "resource") return p.resource?.text ?? `[resource ${p.resource?.uri ?? ""}]`;
        return "";
      })
      .filter(Boolean);
    return parts.join("\n");
  }

  // The detail panel opens itself while approval is pending (users must see the
  // arguments they are approving) and stays closed otherwise.
  let manualOpen = $state<boolean | null>(null);
  const open = $derived(manualOpen ?? awaiting);
  // Argument/result formatting only runs when the panel is actually visible — results
  // can be tens of KB and re-stringifying them per streamed delta is wasteful.
  const argsText = $derived(open ? pretty(segment.call.arguments) : "");
  const output = $derived(open ? resultText(segment.content) : "");

  // "Always allow" is saved to the run's scope: the agent for agent chats, else this chat.
  const agent = $derived(chatStore.active ? chatStore.agentFor(chatStore.active) : null);
  const grantLabel = $derived(agent ? "Always allow for this agent" : "Always allow in this chat");
  const grantHint = $derived(
    agent
      ? `Saved to ${agent.name}'s tool permissions (change it in the agent editor or the ⚒ Tools menu).`
      : "Saved for this chat only. Defaults for every chat are set in Connections.",
  );

  function decide(decision: "allow_once" | "always_allow" | "deny") {
    if (runId) void mcpStore.approve(runId, segment.call.id, decision);
  }
</script>

<div class="tool-card" class:error={segment.isError} class:awaiting>
  <button class="head" onclick={() => (manualOpen = !open)} aria-expanded={open}>
    <span class="chev" aria-hidden="true">›</span>
    <span class="wrench" aria-hidden="true">⚒</span>
    <span class="title" title={segment.call.name}>{title}</span>
    {#if info}<ToolAccessBadge access={info.access} />{/if}
    <span class="status" data-status={segment.status}>{labels[segment.status]}</span>
  </button>

  {#if awaiting}
    <div class="approve" role="group" aria-label="Approve tool call">
      <span class="q">Allow this call?</span>
      <button class="btn-primary btn-sm" onclick={() => decide("allow_once")}>Allow</button>
      <button class="btn-outline btn-sm" onclick={() => decide("always_allow")} title={grantHint}>{grantLabel}</button>
      <button class="btn-ghost btn-sm" onclick={() => decide("deny")}>Deny</button>
    </div>
  {/if}

  {#if open}
    <div class="detail selectable">
      {#if argsText && argsText !== "null"}
        <div class="kv"><span class="k">Arguments</span><pre>{argsText}</pre></div>
      {/if}
      {#if output}
        <div class="kv"><span class="k">{segment.isError ? "Error" : "Result"}</span><pre>{output}</pre></div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .tool-card {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-sunken);
    font-size: 13px;
    overflow: hidden;
    max-width: 640px;
  }
  .tool-card.error {
    border-color: var(--danger);
  }
  .tool-card.awaiting {
    border-color: var(--warning);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    padding: 7px 10px;
    text-align: left;
    font-size: 12.5px;
  }
  .chev {
    color: var(--fg-faint);
    transition: transform var(--motion-fast) var(--ease);
    display: inline-block;
  }
  .head[aria-expanded="true"] .chev {
    transform: rotate(90deg);
  }
  .wrench {
    color: var(--fg-faint);
    font-size: 11px;
  }
  .title {
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .status {
    margin-left: auto;
    flex: none;
    font-size: 11px;
    color: var(--fg-faint);
  }
  .status[data-status="pending_approval"] {
    color: var(--warning);
  }
  .status[data-status="running"] {
    color: var(--accent);
  }
  .status[data-status="failed"],
  .status[data-status="denied"],
  .status[data-status="blocked"] {
    color: var(--danger);
  }
  .approve {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 8px 10px;
    border-top: 1px solid var(--border);
    background: var(--bg-elevated);
    flex-wrap: wrap;
  }
  .q {
    font-size: 12.5px;
    color: var(--fg-muted);
    margin-right: auto;
  }
  .detail {
    border-top: 1px solid var(--border);
    padding: 8px 10px;
    display: grid;
    gap: var(--space-2);
    max-height: 320px;
    overflow-y: auto;
  }
  .kv {
    display: grid;
    gap: 4px;
  }
  .k {
    font-size: 10.5px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-faint);
  }
  pre {
    margin: 0;
    font-size: 12px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 200px;
    overflow-y: auto;
  }
</style>
