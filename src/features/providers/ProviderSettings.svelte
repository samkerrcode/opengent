<script lang="ts">
  import { providerStore } from "./provider-store.svelte";
  import { describeError, type ProviderConfig, type ProviderInput, type ProviderTestResult, type TlsMode } from "$lib/ipc/commands";

  let { onclose }: { onclose: () => void } = $props();

  type Form = {
    id: string | null;
    name: string;
    base_url: string;
    default_model: string;
    api_key: string;
    key_touched: boolean;
    include_usage: boolean;
    context_window: string;
    tls_mode: TlsMode;
    tls_ca_pem: string;
    ca_touched: boolean;
  };

  const presets = [
    { label: "Ollama", base_url: "http://localhost:11434/v1", model: "llama3.1" },
    { label: "LM Studio", base_url: "http://localhost:1234/v1", model: "" },
    { label: "vLLM / llama.cpp", base_url: "http://localhost:8000/v1", model: "" },
    { label: "OpenRouter", base_url: "https://openrouter.ai/api/v1", model: "" },
  ];

  let editing = $state<Form | null>(null);
  let busy = $state(false);
  let notice = $state<{ ok: boolean; text: string } | null>(null);
  let testResult = $state<ProviderTestResult | null>(null);
  let dialog = $state<HTMLElement | null>(null);

  function blankForm(): Form {
    return { id: null, name: "", base_url: "", default_model: "", api_key: "", key_touched: false, include_usage: true, context_window: "", tls_mode: "system", tls_ca_pem: "", ca_touched: false };
  }

  function startNew() {
    editing = blankForm();
    notice = null;
    testResult = null;
  }

  function startEdit(p: ProviderConfig) {
    editing = {
      id: p.id,
      name: p.name,
      base_url: p.base_url,
      default_model: p.default_model ?? "",
      api_key: "",
      key_touched: false,
      include_usage: p.include_usage,
      context_window: p.context_window?.toString() ?? "",
      tls_mode: p.tls_mode,
      tls_ca_pem: p.tls_ca_pem ?? "",
      ca_touched: false,
    };
    notice = null;
    testResult = null;
  }

  function applyPreset(preset: (typeof presets)[number]) {
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
      kind: "open_ai_compatible",
      base_url: f.base_url,
      default_model: f.default_model.trim() || null,
      // Rust semantics: null = keep existing key, "" = clear, value = replace.
      api_key: f.key_touched ? f.api_key : null,
      include_usage: f.include_usage,
      context_window: Number.isFinite(ctx) && ctx !== null && ctx > 0 ? ctx : null,
      tls_mode: f.tls_mode,
      // Rust semantics: null = keep existing PEM, "" = clear, value = replace.
      tls_ca_pem: f.ca_touched || !f.id ? f.tls_ca_pem : null,
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
            <span class="item-sub">{p.kind === "simulated" ? "built-in" : p.base_url.replace(/^https?:\/\//, "")}</span>
          </button>
        {/each}
        <button class="btn-outline add" onclick={startNew}>+ Add provider</button>
      </aside>

      <div class="form">
        {#if editing}
          <div class="presets">
            {#each presets as preset (preset.label)}
              <button class="chip" type="button" onclick={() => applyPreset(preset)}>{preset.label}</button>
            {/each}
          </div>

          <label>
            <span>Name</span>
            <input type="text" bind:value={editing.name} placeholder="My local Ollama" maxlength="64" />
          </label>
          <label>
            <span>Base URL <small>(OpenAI-compatible, ending in /v1)</small></span>
            <input type="url" bind:value={editing.base_url} placeholder="http://localhost:11434/v1" />
          </label>
          <label>
            <span>Model</span>
            {#if testResult?.models.length}
              <select bind:value={editing.default_model}>
                <option value="">— choose —</option>
                {#each testResult.models as m (m)}<option value={m}>{m}</option>{/each}
              </select>
            {:else}
              <input type="text" bind:value={editing.default_model} placeholder="e.g. llama3.1, gpt-4o-mini" />
            {/if}
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
              placeholder="optional for local servers"
              autocomplete="off"
            />
          </label>
          <div class="row2">
            <label>
              <span>Context window <small>(tokens, optional)</small></span>
              <input type="text" inputmode="numeric" bind:value={editing.context_window} placeholder="8192" />
            </label>
            <label class="check">
              <input type="checkbox" bind:checked={editing.include_usage} />
              <span>Request token usage</span>
            </label>
          </div>
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
            <button class="btn-outline" onclick={() => save(true)} disabled={busy || !editing.base_url}>Save & test</button>
            <button class="btn-primary" onclick={() => save(false)} disabled={busy || !editing.base_url || !editing.name}>Save</button>
          </footer>
        {:else}
          <div class="placeholder">
            <p>Add a connection to any OpenAI-compatible server — local (Ollama, LM Studio, vLLM) or hosted.</p>
            <p class="muted">Native OpenAI, Anthropic, Google, Bedrock and Hugging Face adapters arrive in later phases.</p>
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
