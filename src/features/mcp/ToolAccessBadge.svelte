<script lang="ts" module>
  import type { ToolAccess } from "$lib/ipc/commands";

  // Server-declared MCP annotation hints — untrusted, informational only.
  export const accessLabels: Record<ToolAccess, { label: string; hint: string }> = {
    read_only: { label: "Read-only", hint: "The server marks this tool as read-only (not verified)." },
    write: { label: "Write", hint: "The server marks this tool as making changes, but not destructive ones (not verified)." },
    destructive: { label: "Destructive", hint: "The server marks this tool as potentially destructive — it may delete or overwrite data." },
    unannotated: {
      label: "Unannotated",
      hint: "The server gave no read-only or destructive hint. The MCP spec treats such tools as potentially destructive.",
    },
  };
</script>

<script lang="ts">
  let { access }: { access: ToolAccess } = $props();
</script>

<span class="badge {access}" title={accessLabels[access].hint}>{accessLabels[access].label}</span>

<style>
  .badge {
    flex: none;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    padding: 0 4px;
    color: var(--fg-faint);
    white-space: nowrap;
  }
  .write {
    color: var(--accent);
    border-color: var(--accent);
  }
  .destructive {
    color: var(--danger);
    border-color: var(--danger);
  }
  .unannotated {
    color: var(--warning);
    border-color: var(--warning);
    border-style: dashed;
  }
</style>
