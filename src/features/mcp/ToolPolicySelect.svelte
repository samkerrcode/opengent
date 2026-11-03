<script lang="ts" module>
  import type { ToolPolicy } from "$lib/ipc/commands";

  export const policyOptions: { value: ToolPolicy; label: string; hint: string }[] = [
    { value: "always_allow", label: "Allow", hint: "run without asking" },
    { value: "ask", label: "Ask", hint: "approve each call" },
    { value: "block", label: "Disable", hint: "hidden from the model" },
  ];
</script>

<script lang="ts">
  let {
    value,
    onchange,
    label,
    disabled = false,
    allowDisable = true,
  }: {
    value: ToolPolicy;
    onchange: (p: ToolPolicy) => void;
    label: string;
    disabled?: boolean;
    /** Offer Disable (agents turn tools off with a checkbox instead). */
    allowDisable?: boolean;
  } = $props();

  const options = $derived(allowDisable ? policyOptions : policyOptions.filter((p) => p.value !== "block"));
</script>

<select
  class="policy"
  {value}
  {disabled}
  aria-label={label}
  onchange={(e) => onchange(e.currentTarget.value as ToolPolicy)}
>
  {#each options as p (p.value)}<option value={p.value} title={p.hint}>{p.label}</option>{/each}
</select>

<style>
  .policy {
    width: auto;
    padding: 1px 6px;
    font-size: 11px;
    border-radius: var(--radius-sm);
  }
</style>
