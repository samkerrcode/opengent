<script lang="ts">
  import { mcpStore } from "./mcp-store.svelte";
  import {
    describeError,
    type KeyValueInput,
    type McpServerInput,
    type McpServerView,
    type McpTransport,
    type ToolLoading,
    type ToolPolicy,
  } from "$lib/ipc/commands";

  let { onclose }: { onclose: () => void } = $props();

  type Form = {
    id: string | null;
    name: string;
    transport: McpTransport;
    command: string;
    cwd: string;
    env: KeyValueInput[];
    url: string;
    headers: KeyValueInput[];
    default_policy: ToolPolicy;
    call_timeout_secs: string;
  };

  const policies: { value: ToolPolicy; label: string }[] = [
    { value: "always_allow", label: "Always allow — tools run without asking" },
    { value: "ask", label: "Ask — approve each call in the chat" },
    { value: "block", label: "Disable — tools are hidden from the model" },
  ];

  let editing = $state<Form | null>(null);
  let busy = $state(false);
  let notice = $state<{ ok: boolean; text: string } | null>(null);
  let dialog = $state<HTMLElement | null>(null);
  let jsonImport = $state("");
  let showImport = $state(false);

  function blankForm(): Form {
    return { id: null, name: "", transport: "stdio", command: "", cwd: "", env: [], url: "", headers: [], default_policy: "ask", call_timeout_secs: "120" };
  }

  function startNew() {
    editing = blankForm();
    notice = null;
  }

  function joinCommand(s: McpServerView) {
    return [s.command, ...s.args].join(" ");
  }

  function startEdit(s: McpServerView) {
    editing = {
      id: s.id,
      name: s.name,
      transport: s.transport,
      command: s.transport === "stdio" ? joinCommand(s) : "",
      cwd: s.cwd ?? "",
      env: s.env.map((e) => ({ name: e.name, value: e.secret ? "" : e.value, secret: e.secret })),
      url: s.url,
      headers: s.headers.map((e) => ({ name: e.name, value: e.secret ? "" : e.value, secret: e.secret })),
      default_policy: s.default_policy,
      call_timeout_secs: String(s.call_timeout_secs),
    };
    notice = null;
    showImport = false;
    jsonImport = "";
  }

  function toInput(f: Form): McpServerInput {
    const timeout = Number.parseInt(f.call_timeout_secs, 10);
    return {
      id: f.id,
      name: f.name,
      transport: f.transport,
      command: f.transport === "stdio" ? f.command : "",
      env: f.transport === "stdio" ? f.env.filter((e) => e.name.trim()) : [],
      cwd: f.transport === "stdio" ? f.cwd.trim() || null : null,
      url: f.transport === "streamable_http" ? f.url : "",
      headers: f.transport === "streamable_http" ? f.headers.filter((e) => e.name.trim()) : [],
      default_policy: f.default_policy,
      call_timeout_secs: Number.isFinite(timeout) && timeout > 0 ? timeout : 120,
    };
  }

  // Heuristic for marking imported env/header values as keychain secrets.
  const SECRETISH = /key|token|secret|auth|pass|credential|cookie/i;

  // Quotes a value so Rust's split_command parses it back as one arg (round-trips
  // spaces, quotes and backslashes).
  const shellQuote = (s: string) => (/[\s"'\\]/.test(s) ? '"' + s.replace(/["\\]/g, "\\$1") + '"' : s);

  // Accepts the standard {"mcpServers": {name: {command,args,env} | {url,headers}}} shape.
  function applyImport() {
    if (!editing) return;
    try {
      const parsed = JSON.parse(jsonImport);
      const servers = parsed.mcpServers ?? parsed;
      const first = Object.entries(servers)[0];
      if (!first) throw new Error("no servers found");
      const [name, cfg] = first as [string, Record<string, unknown>];
      if (typeof cfg.url === "string" && cfg.url) {
        editing.transport = "streamable_http";
        editing.url = cfg.url;
        editing.headers = Object.entries((cfg.headers as Record<string, string>) ?? {}).map(([n, v]) => ({ name: n, value: v, secret: SECRETISH.test(n) }));
      } else {
        editing.transport = "stdio";
        const cmd = [cfg.command, ...((cfg.args as string[]) ?? [])].filter(Boolean).map((a) => shellQuote(String(a))).join(" ");
        editing.command = cmd;
        editing.env = Object.entries((cfg.env as Record<string, string>) ?? {}).map(([n, v]) => ({ name: n, value: v, secret: SECRETISH.test(n) }));
      }
      if (!editing.name) editing.name = name;
      showImport = false;
      notice = { ok: true, text: `Imported "${name}" — review and save.` };
    } catch (e) {
      notice = { ok: false, text: `Could not parse JSON: ${describeError(e)}` };
    }
  }

  async function save(andConnect: boolean) {
    if (!editing) return;
    busy = true;
    notice = null;
    try {
      await mcpStore.save(toInput(editing));
      const id = editing.id;
      // save() connects enabled servers automatically; offer explicit reconnect too.
      notice = { ok: true, text: andConnect ? "Saved; connecting…" : "Saved." };
      if (andConnect && id) await mcpStore.reconnect(id);
    } catch (e) {
      notice = { ok: false, text: describeError(e) };
    } finally {
      busy = false;
    }
  }

  async function remove(s: McpServerView) {
    if (!confirm(`Remove "${s.name}"? Its secrets will be deleted from the keychain.`)) return;
    busy = true;
    try {
      await mcpStore.remove(s.id);
      if (editing?.id === s.id) editing = null;
    } catch (e) {
      notice = { ok: false, text: describeError(e) };
    } finally {
      busy = false;
    }
  }

  function statusText(s: McpServerView): string {
    if (!s.enabled) return "disabled";
    switch (s.status.kind) {
      case "connected": return `${s.status.tool_count} tools`;
      case "connecting": return "connecting…";
      case "failed": return "failed";
      default: return "idle";
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }

  const current = $derived(editing?.id ? mcpStore.servers.find((s) => s.id === editing?.id) : undefined);

  $effect(() => {
    dialog?.focus();
  });
</script>

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="conn-title" tabindex="-1" bind:this={dialog} onkeydown={onKeydown}>
    <header class="head">
      <h2 id="conn-title">Connections</h2>
      <button class="btn-ghost" onclick={onclose} aria-label="Close">✕</button>
    </header>

    <div class="body">
      <aside class="list">
        {#each mcpStore.servers as s (s.id)}
          <button class="item" class:active={editing?.id === s.id} onclick={() => startEdit(s)}>
            <span class="item-name">{s.name}</span>
            <span class="item-sub">{s.transport === "stdio" ? "local" : "remote"} · {statusText(s)}</span>
          </button>
        {/each}
        <button class="btn-outline add" onclick={startNew}>+ Add connection</button>
        <div class="global">
          <span class="glabel">Tool loading</span>
          <select value={mcpStore.settings.loading} onchange={(e) => mcpStore.setLoading(e.currentTarget.value as ToolLoading)}>
            <option value="auto">Auto</option>
            <option value="all">Always load all</option>
            <option value="on_demand">On-demand search</option>
          </select>
          <p class="ghint">On-demand sends a small `find_tools` search instead of every definition — the model loads what it needs, keeping prompts small when many servers are connected.</p>
        </div>
      </aside>

      <div class="form">
        {#if editing}
          <div class="row2">
            <label>
              <span>Transport</span>
              <select bind:value={editing.transport}>
                <option value="stdio">Local command (stdio)</option>
                <option value="streamable_http">Remote server (HTTP)</option>
              </select>
            </label>
            <label>
              <span>Name</span>
              <input type="text" bind:value={editing.name} placeholder="filesystem" maxlength="64" />
            </label>
          </div>

          {#if editing.transport === "stdio"}
            <label>
              <span>Command <small>(e.g. npx -y @modelcontextprotocol/server-everything)</small></span>
              <input type="text" bind:value={editing.command} spellcheck="false" placeholder="npx -y some-mcp-server --flag" />
            </label>
            <label>
              <span>Working directory <small>(optional)</small></span>
              <input type="text" bind:value={editing.cwd} placeholder="~/project" />
            </label>
            <fieldset class="kvs">
              <legend>Environment <small>(secret values go to the keychain; leave a saved secret blank to keep it)</small></legend>
              {#each editing.env as e, i (i)}
                <div class="kvrow">
                  <input type="text" bind:value={e.name} placeholder="NAME" spellcheck="false" />
                  <input type={e.secret ? "password" : "text"} bind:value={e.value} placeholder={e.secret ? "saved — blank keeps it" : "value"} autocomplete="off" />
                  <label class="sec"><input type="checkbox" bind:checked={e.secret} /> secret</label>
                  <button class="btn-ghost btn-sm" onclick={() => editing && (editing.env = editing.env.filter((_, j) => j !== i))} aria-label="Remove">✕</button>
                </div>
              {/each}
              <button class="btn-ghost btn-sm" onclick={() => editing && editing.env.push({ name: "", value: "", secret: false })}>+ Variable</button>
            </fieldset>
          {:else}
            <label>
              <span>URL <small>(Streamable HTTP endpoint, e.g. https://host/mcp)</small></span>
              <input type="url" bind:value={editing.url} placeholder="https://example.com/mcp" />
            </label>
            <fieldset class="kvs">
              <legend>Headers <small>(mark secrets like tokens — stored in the keychain)</small></legend>
              {#each editing.headers as e, i (i)}
                <div class="kvrow">
                  <input type="text" bind:value={e.name} placeholder="Authorization" spellcheck="false" />
                  <input type={e.secret ? "password" : "text"} bind:value={e.value} placeholder={e.secret ? "saved — blank keeps it" : "value"} autocomplete="off" />
                  <label class="sec"><input type="checkbox" bind:checked={e.secret} /> secret</label>
                  <button class="btn-ghost btn-sm" onclick={() => editing && (editing.headers = editing.headers.filter((_, j) => j !== i))} aria-label="Remove">✕</button>
                </div>
              {/each}
              <button class="btn-ghost btn-sm" onclick={() => editing && editing.headers.push({ name: "", value: "", secret: true })}>+ Header</button>
            </fieldset>
          {/if}

          <div class="row2">
            <label>
              <span>Tool permission default</span>
              <select bind:value={editing.default_policy}>
                {#each policies as p (p.value)}<option value={p.value}>{p.label}</option>{/each}
              </select>
            </label>
            <label>
              <span>Tool call timeout <small>(seconds)</small></span>
              <input type="text" inputmode="numeric" bind:value={editing.call_timeout_secs} placeholder="120" />
            </label>
          </div>

          {#if current}
            <div class="status">
              <span>Status: {statusText(current)}{current.server_info ? ` · ${current.server_info}` : ""}</span>
              <button class="btn-ghost btn-sm" onclick={() => mcpStore.reconnect(current.id)}>Reconnect</button>
            </div>
            {#if current.status.kind === "failed"}
              <p class="result bad" role="status">{current.status.error}</p>
              {#if current.stderr_tail.length}
                <pre class="stderr selectable">{current.stderr_tail.slice(-8).join("\n")}</pre>
              {/if}
            {/if}
          {/if}
          {#if notice}
            <p class="result" class:ok={notice.ok} class:bad={!notice.ok} role="status">{notice.text}</p>
          {/if}

          {#if showImport}
            <label>
              <span>Paste an <code>mcpServers</code> JSON config</span>
              <textarea rows="4" spellcheck="false" bind:value={jsonImport} placeholder={'{"mcpServers": {"fs": {"command": "npx", "args": ["-y", "server"]}}}'}></textarea>
            </label>
            <div class="row2">
              <button class="btn-outline btn-sm" onclick={applyImport}>Import into form</button>
              <button class="btn-ghost btn-sm" onclick={() => (showImport = false)}>Cancel</button>
            </div>
          {:else}
            <button class="btn-ghost btn-sm import" onclick={() => (showImport = true)}>Import JSON config…</button>
          {/if}

          <footer class="actions">
            {#if current}
              <button class="btn-danger btn-sm" onclick={() => remove(current)} disabled={busy}>Remove</button>
            {/if}
            <span class="spacer"></span>
            <button class="btn-outline" onclick={() => save(true)} disabled={busy || !editing.name}>Save & connect</button>
            <button class="btn-primary" onclick={() => save(false)} disabled={busy || !editing.name}>Save</button>
          </footer>
        {:else}
          <div class="placeholder">
            <p>Connect MCP servers to give the model tools. Local servers run as commands (stdio); remote servers use Streamable HTTP.</p>
            <p class="muted">Each tool can be set to Always allow, Ask, or Block — from the ⚒ Tools menu or per-server here.</p>
            <button class="btn-primary" onclick={startNew}>Add connection</button>
          </div>
        {/if}
      </div>
    </div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.45); display: grid; place-items: center; z-index: 50; }
  .dialog { width: min(860px, calc(100vw - 48px)); height: min(620px, calc(100vh - 48px)); background: var(--bg-elevated); border: 1px solid var(--border); border-radius: var(--radius-xl); box-shadow: var(--shadow-2); display: flex; flex-direction: column; overflow: hidden; outline: none; }
  .head { display: flex; align-items: center; justify-content: space-between; padding: var(--space-4) var(--space-5); border-bottom: 1px solid var(--border); }
  h2 { margin: 0; font-size: 16px; font-weight: 600; }
  .body { display: grid; grid-template-columns: 260px 1fr; flex: 1; min-height: 0; }
  .list { border-right: 1px solid var(--border); padding: var(--space-3); display: flex; flex-direction: column; gap: 4px; overflow-y: auto; background: var(--bg); }
  .item { display: flex; flex-direction: column; align-items: flex-start; gap: 2px; padding: 8px 10px; text-align: left; width: 100%; }
  .item:hover { background: var(--bg-hover); }
  .item.active { background: var(--accent-soft); }
  .item-name { font-weight: 500; }
  .item-sub { font-size: 11.5px; color: var(--fg-faint); }
  .add { margin-top: auto; justify-content: center; }
  .global { margin-top: var(--space-3); border-top: 1px solid var(--border); padding-top: var(--space-3); display: grid; gap: 6px; }
  .glabel { font-size: 11px; text-transform: uppercase; letter-spacing: 0.07em; color: var(--fg-faint); }
  .global select { font-size: 12px; padding: 5px 8px; }
  .ghint { margin: 0; font-size: 11.5px; color: var(--fg-faint); line-height: 1.45; }
  .form { padding: var(--space-5); display: flex; flex-direction: column; gap: var(--space-3); overflow-y: auto; }
  label { display: grid; gap: 6px; font-size: 13px; color: var(--fg-muted); }
  label small, .kvs legend small { color: var(--fg-faint); font-weight: normal; }
  .row2 { display: grid; grid-template-columns: 1fr 1fr; gap: var(--space-3); align-items: end; }
  .kvs { border: 1px solid var(--border); border-radius: var(--radius-md); padding: var(--space-2) var(--space-3) var(--space-3); display: grid; gap: 6px; margin: 0; }
  .kvs legend { font-size: 12px; color: var(--fg-muted); padding: 0 4px; }
  .kvrow { display: grid; grid-template-columns: 140px 1fr auto auto; gap: 6px; align-items: center; }
  .kvrow input { padding: 5px 8px; font-size: 12.5px; }
  .sec { display: inline-flex; grid-template-columns: none; flex-direction: row; align-items: center; gap: 4px; font-size: 11px; color: var(--fg-faint); }
  .status { display: flex; align-items: center; gap: var(--space-2); font-size: 12.5px; color: var(--fg-muted); }
  .result { margin: 0; padding: 8px 10px; border-radius: var(--radius-md); font-size: 13px; }
  .result.ok { background: var(--accent-soft); }
  .result.bad { background: var(--danger-soft); color: var(--danger); }
  .stderr { margin: 0; font-size: 11px; color: var(--fg-faint); max-height: 100px; overflow-y: auto; white-space: pre-wrap; }
  .actions { display: flex; gap: var(--space-2); align-items: center; margin-top: auto; padding-top: var(--space-3); }
  .spacer { flex: 1; }
  .placeholder { margin: auto; max-width: 42ch; text-align: center; display: grid; gap: var(--space-3); justify-items: center; }
  .placeholder p { margin: 0; }
  .muted { color: var(--fg-faint); font-size: 13px; }
  .import { justify-self: start; }
  textarea { font-family: var(--font-mono); font-size: 12px; resize: vertical; }
</style>
