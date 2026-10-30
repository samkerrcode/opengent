<script lang="ts">
  import { providerStore } from "./provider-store.svelte";

  let { onopensettings }: { onopensettings: () => void } = $props();

  // Option value encodes provider + model so a single native <select> covers both.
  const SEP = "\u0000";
  const value = $derived(providerStore.active ? `${providerStore.active.provider_id}${SEP}${providerStore.active.model}` : "");

  const groups = $derived(
    providerStore.providers
      .map((p) => {
        const models = new Set<string>(p.models);
        if (p.default_model) models.add(p.default_model);
        if (providerStore.active?.provider_id === p.id) models.add(providerStore.active.model);
        return { provider: p, models: [...models].sort() };
      })
      .filter((g) => g.models.length > 0),
  );

  async function onchange(e: Event) {
    const v = (e.currentTarget as HTMLSelectElement).value;
    if (v === "__settings") {
      onopensettings();
      return;
    }
    const [providerId, model] = v.split(SEP);
    if (providerId && model) await providerStore.select(providerId, model);
  }
</script>

<label class="picker" title="Provider and model">
  <span class="visually-hidden">Model</span>
  <span class="dot" class:sim={providerStore.activeProvider?.kind === "simulated"} aria-hidden="true"></span>
  <select {value} {onchange}>
    {#if !providerStore.active}<option value="">Choose a model…</option>{/if}
    {#each groups as g (g.provider.id)}
      <optgroup label={g.provider.name}>
        {#each g.models as m (m)}
          <option value={`${g.provider.id}${SEP}${m}`}>{m}</option>
        {/each}
      </optgroup>
    {/each}
    <option disabled>──────────</option>
    <option value="__settings">Manage providers…</option>
  </select>
</label>

<style>
  .picker {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 2px 4px 2px 9px;
    border-radius: 999px;
    background: var(--bg-sunken);
    border: 1px solid transparent;
    max-width: 280px;
  }
  .picker:hover,
  .picker:focus-within {
    border-color: var(--border-strong);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--success);
    flex: none;
  }
  .dot.sim {
    background: var(--warning);
  }
  select {
    appearance: none;
    -webkit-appearance: none;
    border: none;
    background: transparent;
    font-size: 12.5px;
    padding: 2px 18px 2px 0;
    color: var(--fg);
    max-width: 240px;
    text-overflow: ellipsis;
    background-image: linear-gradient(45deg, transparent 50%, var(--fg-faint) 50%),
      linear-gradient(135deg, var(--fg-faint) 50%, transparent 50%);
    background-position:
      right 8px top 50%,
      right 4px top 50%;
    background-size: 4px 4px;
    background-repeat: no-repeat;
  }
  select:focus-visible {
    box-shadow: none;
  }
</style>
