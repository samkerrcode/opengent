<script lang="ts">
  import { providerStore } from "$features/providers/provider-store.svelte";
  import type { ModelRef } from "$lib/ipc/commands";

  let {
    value = $bindable(),
    id,
    allowNone = false,
    noneLabel = "None",
  }: { value: ModelRef | null; id?: string; allowNone?: boolean; noneLabel?: string } = $props();

  // Provider ids are UUIDs or "simulated", so "::" never appears in them.
  const SEP = "::";
  const encode = (m: ModelRef | null) => (m ? `${m.provider_id}${SEP}${m.model}` : "");

  const groups = $derived(
    providerStore.providers
      .map((p) => {
        const models = new Set<string>(p.models);
        if (p.default_model) models.add(p.default_model);
        // Keep a saved choice visible even if the provider no longer lists it.
        if (value?.provider_id === p.id) models.add(value.model);
        return { provider: p, models: [...models].sort() };
      })
      .filter((g) => g.models.length > 0),
  );

  const known = $derived(!value || providerStore.providers.some((p) => p.id === value?.provider_id));

  function onChange(e: Event) {
    const raw = (e.currentTarget as HTMLSelectElement).value;
    const i = raw.indexOf(SEP);
    value = i < 0 ? null : { provider_id: raw.slice(0, i), model: raw.slice(i + SEP.length) };
  }
</script>

<select {id} value={encode(value)} onchange={onChange}>
  {#if allowNone || !value}
    <option value="">{allowNone ? noneLabel : "Choose a model…"}</option>
  {/if}
  {#if !known && value}
    <option value={encode(value)}>{value.model} (provider removed)</option>
  {/if}
  {#each groups as g (g.provider.id)}
    <optgroup label={g.provider.name}>
      {#each g.models as m (m)}
        <option value={encode({ provider_id: g.provider.id, model: m })}>{m}</option>
      {/each}
    </optgroup>
  {/each}
</select>
