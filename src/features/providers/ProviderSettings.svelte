<script lang="ts">
  import { tick } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { providerStore } from "./provider-store.svelte";
  import ProviderIcon from "./ProviderIcon.svelte";
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

  /** `null` = the provider picker; otherwise the form for a new or saved provider. */
  let editing = $state<Form | null>(null);
  /** Serialized form as last loaded or saved; differs from the live form when there are unsaved edits. */
  let pristine = $state("");
  /** Rust-owned description of the kind being edited: labels, defaults, presets. */
  const spec = $derived<KindSpec | undefined>(editing ? providerStore.kindSpec(editing.kind) : undefined);
  const baseUrlReady = $derived(!!editing && (!!editing.base_url.trim() || !!spec?.default_base_url));
  /** Kinds with a built-in endpoint tuck Base URL behind "Advanced settings"; others show it up front. */
  const endpointInAdvanced = $derived(!!spec?.default_base_url);
  let advanced = $state(false);
  let showKey = $state(false);
  let busy = $state<"save" | "test" | null>(null);
  let notice = $state<{ ok: boolean; text: string } | null>(null);
  /** Confirmation shown on the picker after a successful save. */
  let flash = $state<string | null>(null);
  let testResult = $state<ProviderTestResult | null>(null);
  let dialog = $state<HTMLElement | null>(null);
  let pane = $state<HTMLElement | null>(null);

  const dirty = $derived(!!editing && serialize(editing) !== pristine);
  const current = $derived(editing?.id ? providerStore.providers.find((p) => p.id === editing?.id) : undefined);

  /** Advanced settings that differ from their defaults (shown as a hint while collapsed). */
  const customized = $derived.by(() => {
    if (!editing) return false;
    const f = editing;
    return (
      (endpointInAdvanced && f.base_url.trim() !== spec?.default_base_url) ||
      f.tls_mode !== "system" ||
      !!f.context_window ||
      Object.values(f.model_context).some(Boolean) ||
      f.image_support !== "auto" ||
      (!!spec?.usage_option && !f.include_usage)
    );
  });

  // Models a context window can be set on: discovered by the last test, cached on the
  // saved provider, the typed default, or already carrying an override.
  const ctxModels = $derived.by(() => {
    if (!editing) return [] as string[];
    const names = new Set<string>();
    for (const m of testResult?.models ?? []) names.add(m);
    for (const m of current?.models ?? []) names.add(m);
    const d = editing.default_model.trim();
    if (d) names.add(d);
    for (const m of Object.keys(editing.model_context)) names.add(m);
    return [...names];
  });

  /** Models offered in the default-model dropdown once a test or earlier discovery listed some. */
  const knownModels = $derived(testResult?.models.length ? testResult.models : (current?.models ?? []));

  function serialize(f: Form): string {
    return JSON.stringify(toInput(f));
  }

  /** Kinds with a built-in endpoint start with a usable name (their label) and URL. */
  function blankForm(kind: ProviderKind): Form {
    const s = providerStore.kindSpec(kind);
    const base_url = s?.default_base_url ?? "";
    const name = s?.default_base_url ? s.label : "";
    return { id: null, kind, name, base_url, default_model: "", api_key: "", key_touched: false, include_usage: true, context_window: "", model_context: {}, tls_mode: "system", tls_ca_pem: "", ca_touched: false, image_support: "auto" };
  }

  async function open(form: Form) {
    editing = form;
    pristine = serialize(form);
    advanced = customized;
    showKey = false;
    notice = null;
    flash = null;
    testResult = null;
    await tick();
    pane?.scrollTo(0, 0);
    pane?.querySelector<HTMLElement>("input:not([type=checkbox]), select")?.focus();
  }

  /** Opens a URL in the system browser; a denied open shows up as a form error, not silence. */
  function openLink(url: string) {
    void openUrl(url).catch((e) => (notice = { ok: false, text: `Could not open the link: ${describeError(e)}` }));
  }

  /** Unsaved edits need confirmation before the form is left. */
  function mayLeave(): boolean {
    return !dirty || confirm("Discard unsaved changes to this provider?");
  }

  function startNew(kind: ProviderKind) {
    if (editing && !mayLeave()) return;
    void open(blankForm(kind));
  }

  function startEdit(p: ProviderConfig) {
    if (editing?.id === p.id || (editing && !mayLeave())) return;
    const s = providerStore.kindSpec(p.kind);
    void open({
      id: p.id,
      kind: p.kind,
      name: p.name,
      base_url: s?.default_base_url && p.base_url === s.default_base_url ? s.default_base_url : p.base_url,
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
    });
  }

  /** Returns to the picker, focusing the tile of the kind just left. */
  async function backToPicker(message: string | null = null, force = false) {
    if (!editing || (!force && !mayLeave())) return;
    const kind = editing.kind;
    editing = null;
    notice = null;
    testResult = null;
    flash = message;
    await tick();
    dialog?.querySelector<HTMLElement>(`.tile[data-kind="${kind}"]`)?.focus();
  }

  function requestClose() {
    if (mayLeave()) onclose();
  }

  /** List subtitle: the endpoint host, or the kind label when using the kind's default endpoint. */
  function subtitle(p: ProviderConfig): string {
    if (p.kind === "simulated") return "Built-in demo";
    const s = providerStore.kindSpec(p.kind);
    if (s?.default_base_url && p.base_url === s.default_base_url) return s.label;
    return p.base_url.replace(/^https?:\/\//, "");
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

  /** Makes a saved provider the chat default when nothing else is selected yet. */
  async function adoptAsActive(id: string, model: string) {
    if (model && (!providerStore.active || providerStore.active.provider_id === id)) await providerStore.select(id, model);
  }

  /** Persists the form; on success the form's key/CA fields reset so they read as unchanged. */
  async function persist(f: Form): Promise<ProviderConfig> {
    const saved = await providerStore.save(toInput(f));
    f.id = saved.id;
    f.api_key = "";
    f.key_touched = false;
    f.ca_touched = false;
    pristine = serialize(f);
    return saved;
  }

  // Save returns to the picker. Save & test returns only when the connection works and a
  // default model is set; otherwise it stays so the user can fix the problem or pick a model.
  async function save(andTest: boolean) {
    if (!editing) return;
    const f = editing;
    busy = andTest ? "test" : "save";
    notice = null;
    try {
      const saved = await persist(f);
      if (!andTest) {
        await adoptAsActive(saved.id, f.default_model.trim());
        return void backToPicker(`${saved.name} saved.`, true);
      }
      testResult = await providerStore.test(saved.id);
      const n = testResult.models.length;
      if (!testResult.ok) {
        notice = { ok: false, text: `Provider saved, but the connection test failed: ${testResult.message}` };
      } else if (!f.default_model.trim()) {
        notice = n
          ? { ok: true, text: `Connected · ${n} model${n === 1 ? "" : "s"} found. Choose a default model, then Save.` }
          : { ok: true, text: "Connected, but the server listed no models. Enter a model name, then Save." };
      } else {
        await adoptAsActive(saved.id, f.default_model.trim());
        const models = n ? ` · ${n} model${n === 1 ? "" : "s"} available` : "";
        return void backToPicker(`${saved.name} saved and connected${models}.`, true);
      }
    } catch (e) {
      notice = { ok: false, text: describeError(e) };
    } finally {
      busy = null;
    }
  }

  async function remove(p: ProviderConfig) {
    if (!confirm(`Remove "${p.name}"? Its API key will be deleted from the keychain.`)) return;
    busy = "save";
    try {
      await providerStore.remove(p.id);
      if (editing?.id === p.id) await backToPicker(`${p.name} removed.`, true);
    } catch (e) {
      notice = { ok: false, text: describeError(e) };
    } finally {
      busy = null;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") requestClose();
  }

  $effect(() => {
    dialog?.focus();
  });
</script>

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && requestClose()}>
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
      <button class="btn-ghost" onclick={requestClose} aria-label="Close">✕</button>
    </header>

    <div class="body">
      <aside class="list" aria-label="Your providers">
        <h3 class="list-title">Your providers</h3>
        {#each providerStore.providers as p (p.id)}
          <button
            class="item"
            class:active={editing?.id === p.id}
            class:builtin={p.kind === "simulated"}
            onclick={() => (p.kind === "simulated" ? null : startEdit(p))}
            disabled={p.kind === "simulated"}
          >
            <span class="item-icon"><ProviderIcon kind={p.kind} size={16} /></span>
            <span class="item-text">
              <span class="item-name">{p.name}</span>
              <span class="item-sub">{subtitle(p)}</span>
            </span>
          </button>
        {/each}
        <button class="btn-outline add" class:active={!editing} onclick={() => backToPicker()}>+ Add provider</button>
      </aside>

      {#if editing}
        <div class="pane">
          <div class="scroll" bind:this={pane}>
            <button class="btn-ghost btn-sm back" type="button" onclick={() => backToPicker()}>← All providers</button>
            <div class="form-head">
              <span class="badge lg"><ProviderIcon kind={editing.kind} size={26} /></span>
              <div>
                <h3>{editing.id ? (current?.name ?? editing.name) : `Add ${spec?.label ?? "provider"}`}</h3>
                {#if spec}<p class="lede">{editing.id ? `${spec.label} · ` : ""}{spec.description}</p>{/if}
              </div>
            </div>

            {#if !endpointInAdvanced}
              {#if spec?.presets.length}
                <div class="presets" role="group" aria-label="Quick setup">
                  <span class="presets-label">Quick setup</span>
                  {#each spec.presets as preset (preset.label)}
                    <button class="chip" class:accent={editing.base_url === preset.base_url} type="button" onclick={() => applyPreset(preset)}>{preset.label}</button>
                  {/each}
                </div>
              {/if}
              <label>
                <span>Base URL <small>({spec?.base_url_hint ?? "endpoint URL"})</small></span>
                <input type="url" bind:value={editing.base_url} placeholder={spec?.base_url_placeholder} />
              </label>
            {/if}

            <label>
              <span>Name</span>
              <input type="text" bind:value={editing.name} placeholder={spec?.label ?? "My provider"} maxlength="64" />
            </label>
            <div class="field">
              <label for="api-key">
                <span>
                  API key
                  {#if editing.id && !editing.key_touched}
                    <small>{current?.has_api_key ? "(stored in keychain — leave blank to keep)" : "(none)"}</small>
                  {/if}
                </span>
              </label>
              <div class="key-row">
                <input
                  id="api-key"
                  type={showKey ? "text" : "password"}
                  bind:value={editing.api_key}
                  oninput={() => editing && (editing.key_touched = true)}
                  placeholder={spec?.requires_api_key ? "required" : "optional for local servers"}
                  autocomplete="off"
                  spellcheck="false"
                />
                <button class="btn-ghost btn-sm" type="button" onclick={() => (showKey = !showKey)} aria-pressed={showKey}>{showKey ? "Hide" : "Show"}</button>
              </div>
              {#if spec?.api_key_hint || spec?.api_key_url}
                <small class="field-help">
                  {spec.api_key_hint}
                  {#if spec.api_key_url}<button class="link" type="button" onclick={() => openLink(spec.api_key_url)}>Get an API key ↗</button>{/if}
                </small>
              {/if}
            </div>
            <label>
              <span>Default model</span>
              {#if knownModels.length}
                <select bind:value={editing.default_model}>
                  <option value="">— choose —</option>
                  {#if editing.default_model && !knownModels.includes(editing.default_model)}<option value={editing.default_model}>{editing.default_model}</option>{/if}
                  {#each knownModels as m (m)}<option value={m}>{m}</option>{/each}
                </select>
              {:else}
                <input type="text" bind:value={editing.default_model} placeholder={spec?.model_placeholder || "model name"} />
                <small class="field-help">Or use Save & test to list the models this provider offers.</small>
              {/if}
            </label>

            <button class="disclosure" type="button" aria-expanded={advanced} onclick={() => (advanced = !advanced)}>
              <span class="caret" class:open={advanced}>▸</span> Advanced settings
              {#if customized && !advanced}<span class="chip">customized</span>{/if}
            </button>
            {#if advanced}
              <div class="advanced">
                {#if endpointInAdvanced}
                  <label>
                    <span>Base URL <small>({spec?.base_url_hint}; leave as is unless you use a proxy)</small></span>
                    <input type="url" bind:value={editing.base_url} placeholder={spec?.default_base_url ?? ""} />
                  </label>
                {/if}
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
                <div class="row2">
                  <label>
                    <span>Context window</span>
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
              </div>
            {/if}

            {#if notice}
              <p class="result" class:ok={notice.ok} class:bad={!notice.ok} role="status">{notice.text}</p>
            {/if}
          </div>

          <footer class="actions">
            {#if current}
              <button class="btn-danger btn-sm" onclick={() => remove(current)} disabled={!!busy}>Remove</button>
            {/if}
            <span class="spacer"></span>
            <button class="btn-outline" onclick={() => save(true)} disabled={!!busy || !baseUrlReady || !editing.name.trim()}>{busy === "test" ? "Testing…" : "Save & test"}</button>
            <button class="btn-primary" onclick={() => save(false)} disabled={!!busy || !baseUrlReady || !editing.name.trim()}>{busy === "save" ? "Saving…" : "Save"}</button>
          </footer>
        </div>
      {:else}
        <div class="pane">
          <div class="scroll picker">
            <div class="picker-head">
              <h3>Add a provider</h3>
              <p class="lede">Choose a service to connect. You can add more than one connection of each type.</p>
            </div>
            {#if flash}<p class="result ok" role="status">{flash}</p>{/if}
            <div class="tiles">
              {#each providerStore.kinds as k (k.kind)}
                <button class="tile" data-kind={k.kind} onclick={() => startNew(k.kind)}>
                  <span class="badge"><ProviderIcon kind={k.kind} size={22} /></span>
                  <span class="tile-text">
                    <span class="tile-name">{k.label}</span>
                    {#if k.tagline}<span class="tile-sub">{k.tagline}</span>{/if}
                  </span>
                  <span class="chev" aria-hidden="true">›</span>
                </button>
              {/each}
            </div>
          </div>
        </div>
      {/if}
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
  h3 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    color: var(--fg);
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
  .list-title {
    font-size: 11.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--fg-faint);
    padding: 2px 10px 6px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 10px;
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
    opacity: 1;
  }
  .item.builtin {
    color: var(--fg-muted);
  }
  .item-icon {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    flex: none;
  }
  .item-text {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .item-name {
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
  .add.active {
    border-color: var(--accent);
    color: var(--accent);
  }
  .pane {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
  }
  .scroll {
    flex: 1;
    min-height: 0;
    padding: var(--space-5);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    overflow-y: auto;
  }
  .lede {
    margin: 2px 0 0;
    font-size: 13px;
    line-height: 1.45;
    color: var(--fg-muted);
  }
  .picker {
    gap: var(--space-4);
    padding-top: var(--space-6);
  }
  .tiles {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: var(--space-3);
  }
  .tile {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-4);
    text-align: left;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
    min-height: 76px;
  }
  .tile:hover {
    background: var(--bg-hover);
    border-color: var(--border-strong);
  }
  .tile:focus-visible {
    border-radius: var(--radius-lg);
  }
  .tile:hover .chev {
    color: var(--fg);
    transform: translateX(2px);
  }
  .badge {
    display: grid;
    place-items: center;
    width: 40px;
    height: 40px;
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    flex: none;
  }
  .badge.lg {
    width: 48px;
    height: 48px;
    border-radius: var(--radius-lg);
  }
  .tile-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }
  .tile-name {
    font-weight: 600;
  }
  .tile-sub {
    font-size: 12.5px;
    color: var(--fg-muted);
  }
  .chev {
    font-size: 20px;
    line-height: 1;
    color: var(--fg-faint);
    transition: transform var(--motion-fast) var(--ease), color var(--motion-fast) var(--ease);
  }
  .back {
    align-self: flex-start;
    margin: -6px 0 0 -9px;
  }
  .form-head {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding-bottom: var(--space-3);
    margin-bottom: var(--space-1);
    border-bottom: 1px solid var(--border);
  }
  .presets {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    align-items: center;
  }
  .presets-label {
    font-size: 13px;
    color: var(--fg-muted);
    margin-right: 2px;
  }
  label,
  .field {
    display: grid;
    gap: 6px;
    font-size: 13px;
    color: var(--fg-muted);
  }
  label small {
    color: var(--fg-faint);
    font-weight: normal;
  }
  .key-row {
    display: flex;
    gap: var(--space-2);
    align-items: center;
  }
  .link {
    padding: 0;
    border: 0;
    font-size: inherit;
    color: var(--accent);
    display: inline;
  }
  .link:hover {
    text-decoration: underline;
  }
  .advanced {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding-left: var(--space-3);
    border-left: 2px solid var(--border);
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
    color: var(--fg-faint);
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
    padding: var(--space-3) var(--space-5);
    border-top: 1px solid var(--border);
    background: var(--bg-elevated);
  }
  .spacer {
    flex: 1;
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
