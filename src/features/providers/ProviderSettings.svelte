<script lang="ts">
  import { providerStore } from "./provider-store.svelte";
  import { describeError, type ImageSupport, type KindSpec, type ProviderConfig, type ProviderInput, type ProviderKind, type ProviderPreset, type ProviderTestResult, type TlsMode } from "$lib/ipc/commands";
  import { product } from "$lib/generated/product";

  let { onclose }: { onclose: () => void } = $props();

  type Form = {
    id: string | null;
    kind: ProviderKind;
    name: string;
    base_url: string;
    default_model: string;
    api_key: string;
    key_touched: boolean;
    include_usage: boolean;
    context_window: string;
    /** Per-model context window overrides; select values, "" = provider default. */
    model_context: Record<string, string>;
    tls_mode: TlsMode;
    tls_ca_pem: string;
    ca_touched: boolean;
    image_support: ImageSupport;
  };

  // Common context sizes offered when the server can't report them. Doubling steps keep the
  // list short; the provider default covers models without an override.
  const CONTEXT_SIZES = [4096, 8192, 16384, 32768, 65536, 131072, 262144, 524288, 1048576];
  const fmtCtx = (n: number) => (n >= 1048576 ? `${n / 1048576}M` : `${n / 1024}K`);

  let editing = $state<Form | null>(null);
  /** Rust-owned description of the kind being edited: labels, defaults, presets. */
  const spec = $derived<KindSpec | undefined>(editing ? providerStore.kindSpec(editing.kind) : undefined);
  const baseUrlReady = $derived(!!editing && (!!editing.base_url.trim() || !!spec?.default_base_url));
  /** Kinds with a built-in endpoint tuck Base URL and TLS behind an "Advanced" toggle. */
  let advanced = $state(false);
  const endpointVisible = $derived(!spec?.default_base_url || advanced);
  let busy = $state(false);
  let notice = $state<{ ok: boolean; text: string } | null>(null);
  let testResult = $state<ProviderTestResult | null>(null);
  let dialog = $state<HTMLElement | null>(null);

  // Models a context window can be set on: discovered by the last test, cached on the
  // saved provider, the typed default, or already carrying an override.
  const ctxModels = $derived.by(() => {
    if (!editing) return [] as string[];
    const names = new Set<string>();
    for (const m of testResult?.models ?? []) names.add(m);
    for (const m of providerStore.providers.find((p) => p.id === editing?.id)?.models ?? []) names.add(m);
    const d = editing.default_model.trim();
    if (d) names.add(d);
    for (const m of Object.keys(editing.model_context)) names.add(m);
    return [...names];
  });

  /** Kinds with a built-in endpoint start with a usable name (their label) and URL. */
  function blankForm(kind: ProviderKind): Form {
    const s = providerStore.kindSpec(kind);
    const base_url = s?.default_base_url ?? "";
    const name = s?.default_base_url ? s.label : "";
    return { id: null, kind, name, base_url, default_model: "", api_key: "", key_touched: false, include_usage: true, context_window: "", model_context: {}, tls_mode: "system", tls_ca_pem: "", ca_touched: false, image_support: "auto" };
  }

  function startNew() {
    editing = blankForm(providerStore.kinds[0]?.kind ?? "open_ai_compatible");
    advanced = false;
    notice = null;
    testResult = null;
  }

  /** Switching type on a new provider resets kind-specific fields; a typed name is kept. */
  function changeKind(kind: ProviderKind) {
    if (!editing || editing.id) return;
    const typed = editing.name.trim() && editing.name !== spec?.label;
    const next = blankForm(kind);
    editing = typed ? { ...next, name: editing.name } : next;
    advanced = false;
  }

  /** List subtitle: the endpoint host, or the kind label when using the kind's default endpoint. */
  function subtitle(p: ProviderConfig): string {
    if (p.kind === "simulated") return "built-in";
    const s = providerStore.kindSpec(p.kind);
    if (s?.default_base_url && p.base_url === s.default_base_url) return s.label;
    return p.base_url.replace(/^https?:\/\//, "");
  }

  function startEdit(p: ProviderConfig) {
    editing = {
      id: p.id,
      kind: p.kind,
      name: p.name,
      base_url: p.base_url,
      default_model: p.default_model ?? "",
      api_key: "",
      key_touched: false,
      include_usage: p.include_usage,
      context_window: p.context_window?.toString() ?? "",
      model_context: Object.fromEntries(Object.entries(p.model_context_windows).map(([m, w]) => [m, String(w)])),
      tls_mode: p.tls_mode,
      tls_ca_pem: p.tls_ca_pem ?? "",
      ca_touched: false,
      image_support: p.image_support,
    };
    const s = providerStore.kindSpec(p.kind);
    advanced = !!s?.default_base_url && (p.base_url !== s.default_base_url || p.tls_mode !== "system");
    notice = null;
    testResult = null;
  }

  function applyPreset(preset: ProviderPreset) {
    if (!editing) return;
    editing.base_url = preset.base_url;
    if (!editing.name) editing.name = preset.label;
    if (!editing.default_model && preset.model) editing.default_model = preset.model;
  }

  function toInput(f: Form): ProviderInput {
    const ctx = f.context_window.trim() ? Number.parseInt(f.context_window, 10) : null;
    return {
      id: f.id,
      name: f.name,
      kind: f.kind,
      base_url: f.base_url,
      default_model: f.default_model.trim() || null,
      // Rust semantics: null = keep existing key, "" = clear, value = replace.
      api_key: f.key_touched ? f.api_key : null,
      include_usage: f.include_usage,
      context_window: Number.isFinite(ctx) && ctx !== null && ctx > 0 ? ctx : null,
      model_context_windows: Object.fromEntries(
        Object.entries(f.model_context)
          .map(([m, w]) => [m.trim(), Number.parseInt(w, 10)] as const)
          .filter(([m, w]) => m && Number.isFinite(w) && w > 0),
      ),
      tls_mode: f.tls_mode,
      // Rust semantics: null = keep existing PEM, "" = clear, value = replace.
      tls_ca_pem: f.ca_touched || !f.id ? f.tls_ca_pem : null,
      image_support: f.image_support,
    };
  }

  async function save(andTest: boolean) {
    if (!editing) return;
    busy = true;
    notice = null;
    try {
      const saved = await providerStore.save(toInput(editing));
      editing.id = saved.id;
      editing.api_key = "";
      editing.key_touched = false;
      if (andTest) {
        testResult = await providerStore.test(saved.id);
        if (testResult.ok && !editing.default_model && testResult.models[0]) editing.default_model = testResult.models[0];
        if (testResult.ok && editing.default_model) await providerStore.save(toInput(editing));
      } else {
        notice = { ok: true, text: "Saved." };
      }
      if (!providerStore.active || providerStore.active.provider_id === saved.id) {
        const model = editing.default_model || testResult?.models[0];
        if (model) await providerStore.select(saved.id, model);
      }
    } catch (e) {
      notice = { ok: false, text: describeError(e) };
    } finally {
      busy = false;
    }
  }

  async function remove(p: ProviderConfig) {
    if (!confirm(`Remove "${p.name}"? Its API key will be deleted from the keychain.`)) return;
    busy = true;
    try {
      await providerStore.remove(p.id);
      if (editing?.id === p.id) editing = null;
    } catch (e) {
      notice = { ok: false, text: describeError(e) };
    } finally {
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }

  $effect(() => {
    dialog?.focus();
  });
</script>

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby="settings-title"
    tabindex="-1"
    bind:this={dialog}
    onkeydown={onKeydown}
  >
    <header class="head">
      <h2 id="settings-title">Model providers</h2>
      <button class="btn-ghost" onclick={onclose} aria-label="Close">✕</button>
    </header>

    <div class="body">
      <aside class="list">
        {#each providerStore.providers as p (p.id)}
          <button
            class="item"
            class:active={editing?.id === p.id}
            onclick={() => (p.kind === "simulated" ? null : startEdit(p))}
            disabled={p.kind === "simulated"}
          >
            <span class="item-name">{p.name}</span>
            <span class="item-sub">{subtitle(p)}</span>
          </button>
        {/each}
        <button class="btn-outline add" onclick={startNew}>+ Add provider</button>
      </aside>

      <div class="form">
        {#if editing}
          {#if !editing.id && providerStore.kinds.length > 1}
            <label>
              <span>Type</span>
              <select value={editing.kind} onchange={(e) => changeKind(e.currentTarget.value as ProviderKind)}>
                {#each providerStore.kinds as k (k.kind)}<option value={k.kind}>{k.label}</option>{/each}
              </select>
              {#if spec}<small class="field-help">{spec.description}</small>{/if}
            </label>
          {/if}
          {#if spec?.presets.length}
            <div class="presets">
              {#each spec.presets as preset (preset.label)}
                <button class="chip" type="button" onclick={() => applyPreset(preset)}>{preset.label}</button>
              {/each}
            </div>
          {/if}

          <label>
            <span>Name</span>
            <input type="text" bind:value={editing.name} placeholder={spec?.label ?? "My provider"} maxlength="64" />
          </label>
          <label>
            <span>
              API key
              {#if editing.id && !editing.key_touched}
                <small>{providerStore.providers.find((p) => p.id === editing?.id)?.has_api_key ? "(stored in keychain — leave blank to keep)" : "(none)"}</small>
              {/if}
            </span>
            <input
              type="password"
              bind:value={editing.api_key}
              oninput={() => editing && (editing.key_touched = true)}
              placeholder={spec?.requires_api_key ? "required" : "optional for local servers"}
              autocomplete="off"
            />
            {#if spec?.api_key_hint}<small class="field-help">{spec.api_key_hint}</small>{/if}
          </label>
          <label>
            <span>Default model</span>
            {#if testResult?.models.length}
              <select bind:value={editing.default_model}>
                <option value="">— choose —</option>
                {#each testResult.models as m (m)}<option value={m}>{m}</option>{/each}
              </select>
            {:else}
              <input type="text" bind:value={editing.default_model} placeholder={spec?.model_placeholder || "model name"} />
            {/if}
          </label>
          {#if spec?.default_base_url}
            <button class="disclosure" type="button" aria-expanded={advanced} onclick={() => (advanced = !advanced)}>
              <span class="caret" class:open={advanced}>▸</span> Advanced — proxy or custom endpoint
            </button>
          {/if}
          {#if endpointVisible}
            <label>
              <span>Base URL <small>({spec ? (spec.default_base_url ? `${spec.base_url_hint}; leave as is unless you use a proxy` : spec.base_url_hint) : "endpoint URL"})</small></span>
              <input type="url" bind:value={editing.base_url} placeholder={spec?.default_base_url ?? spec?.base_url_placeholder} />
            </label>
            {#if editing.base_url.trim().startsWith("https://")}
              <label>
                <span>TLS certificate</span>
                <select bind:value={editing.tls_mode}>
                  <option value="system">Verify with the OS trust store (recommended)</option>
                  <option value="custom_ca">Trust a specific certificate (self-signed)</option>
                  <option value="skip_verification">Skip verification — insecure, like curl -k</option>
                </select>
              </label>
              {#if editing.tls_mode === "custom_ca"}
                <label>
                  <span>Server certificate <small>(PEM — openssl s_client -connect host:port | openssl x509)</small></span>
                  <textarea
                    rows="5"
                    spellcheck="false"
                    bind:value={editing.tls_ca_pem}
                    oninput={() => editing && (editing.ca_touched = true)}
                    placeholder="-----BEGIN CERTIFICATE-----&#10;…&#10;-----END CERTIFICATE-----"
                  ></textarea>
                </label>
              {/if}
              {#if editing.tls_mode === "skip_verification"}
                <p class="tls-warning">Connection stays encrypted but the server is not authenticated. Only use on a network you trust.</p>
              {/if}
            {/if}
          {/if}
          <div class="row2">
            <label>
              <span>Context window <small>(optional)</small></span>
              <select bind:value={editing.context_window}>
                <option value="">Reported by the provider, else 8K</option>
                {#each CONTEXT_SIZES as n (n)}<option value={String(n)}>{fmtCtx(n)} tokens</option>{/each}
              </select>
              <small class="field-help">
                Caps how much of the conversation is sent each turn. When unset, {product.displayName}
                uses the window the provider reports for each model, or assumes 8K tokens.
              </small>
            </label>
            {#if spec?.usage_option}
              <label class="check">
                <input type="checkbox" bind:checked={editing.include_usage} />
                <span>Request token usage</span>
              </label>
            {/if}
          </div>
          {#if ctxModels.length}
            <div class="ctx-overrides">
              <span>Per-model context window <small>(overrides the provider default)</small></span>
              {#each ctxModels as m (m)}
                <div class="ctx-row">
                  <span class="ctx-name" title={m}>{m}</span>
                  <select bind:value={editing.model_context[m]}>
                    <option value="">Provider default</option>
                    {#each CONTEXT_SIZES as n (n)}<option value={String(n)}>{fmtCtx(n)} tokens</option>{/each}
                  </select>
                </div>
              {/each}
            </div>
          {/if}
          <label>
            <span>Image input <small>(whether models can see attached images)</small></span>
            <select bind:value={editing.image_support}>
              <option value="auto">Auto-detect — server reports it, or known vision model names</option>
              <option value="always">Supported — every model on this provider accepts images</option>
              <option value="never">Not supported — image attachments are rejected</option>
            </select>
          </label>

          {#if testResult}
            <p class="result" class:ok={testResult.ok} class:bad={!testResult.ok} role="status">{testResult.message}</p>
          {/if}
          {#if notice}
            <p class="result" class:ok={notice.ok} class:bad={!notice.ok} role="status">{notice.text}</p>
          {/if}

          <footer class="actions">
            {#if editing.id}
              {@const current = providerStore.providers.find((p) => p.id === editing?.id)}
              {#if current}
                <button class="btn-danger btn-sm" onclick={() => remove(current)} disabled={busy}>Remove</button>
              {/if}
            {/if}
            <span class="spacer"></span>
            <button class="btn-outline" onclick={() => save(true)} disabled={busy || !baseUrlReady}>Save & test</button>
            <button class="btn-primary" onclick={() => save(false)} disabled={busy || !baseUrlReady || !editing.name}>Save</button>
          </footer>
        {:else}
          <div class="placeholder">
            <p>Add a model provider connection.</p>
            {#each providerStore.kinds as k (k.kind)}<p class="muted">{k.label}: {k.description}</p>{/each}
            <button class="btn-primary" onclick={startNew}>Add provider</button>
          </div>
        {/if}
      </div>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: grid;
    place-items: center;
    z-index: 50;
  }
  .dialog {
    width: min(860px, calc(100vw - 48px));
    height: min(600px, calc(100vh - 48px));
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-2);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    outline: none;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-4) var(--space-5);
    border-bottom: 1px solid var(--border);
  }
  h2 {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
  }
  .body {
    display: grid;
    grid-template-columns: 260px 1fr;
    flex: 1;
    min-height: 0;
  }
  .list {
    border-right: 1px solid var(--border);
    padding: var(--space-3);
    display: flex;
    flex-direction: column;
    gap: 4px;
    overflow-y: auto;
    background: var(--bg);
  }
  .item {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    padding: 8px 10px;
    text-align: left;
    width: 100%;
  }
  .item:not(:disabled):hover {
    background: var(--bg-hover);
  }
  .item.active {
    background: var(--accent-soft);
  }
  .item:disabled {
    opacity: 0.7;
  }
  .item-name {
    font-weight: 500;
  }
  .item-sub {
    font-size: 11.5px;
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 100%;
  }
  .add {
    margin-top: auto;
    justify-content: center;
  }
  .form {
    padding: var(--space-5);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    overflow-y: auto;
  }
  .presets {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  label {
    display: grid;
    gap: 6px;
    font-size: 13px;
    color: var(--fg-muted);
  }
  label small {
    color: var(--fg-faint);
    font-weight: normal;
  }
  .row2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-3);
    align-items: end;
  }
  .field-help {
    font-size: 12px;
    line-height: 1.4;
  }
  .ctx-overrides {
    display: grid;
    gap: 6px;
    font-size: 13px;
    color: var(--fg-muted);
  }
  .ctx-overrides > span small {
    color: var(--fg-faint);
    font-weight: normal;
  }
  .ctx-row {
    display: grid;
    grid-template-columns: 1fr 150px;
    gap: var(--space-2);
    align-items: center;
  }
  .ctx-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .check {
    grid-template-columns: auto 1fr;
    align-items: center;
    padding-bottom: 8px;
  }
  .result {
    margin: 0;
    padding: 8px 10px;
    border-radius: var(--radius-md);
    font-size: 13px;
  }
  .result.ok {
    background: var(--accent-soft);
  }
  .result.bad {
    background: var(--danger-soft);
    color: var(--danger);
  }
  .actions {
    display: flex;
    gap: var(--space-2);
    align-items: center;
    margin-top: auto;
    padding-top: var(--space-3);
  }
  .spacer {
    flex: 1;
  }
  .placeholder {
    margin: auto;
    max-width: 40ch;
    text-align: center;
    display: grid;
    gap: var(--space-3);
    justify-items: center;
  }
  .placeholder p {
    margin: 0;
  }
  .muted {
    color: var(--fg-faint);
    font-size: 13px;
  }
  .disclosure {
    align-self: flex-start;
    padding: 2px 0;
    font-size: 13px;
    color: var(--fg-muted);
    background: none;
    border: 0;
  }
  .disclosure:hover {
    color: var(--fg);
  }
  .caret {
    display: inline-block;
    transition: transform 0.15s ease;
  }
  .caret.open {
    transform: rotate(90deg);
  }
  .tls-warning {
    margin: 0;
    padding: 8px 10px;
    border-radius: var(--radius-md);
    background: var(--danger-soft);
    color: var(--danger);
    font-size: 12.5px;
  }
  textarea {
    font-family: var(--font-mono);
    font-size: 12px;
    resize: vertical;
  }
</style>
