# Changelog

Notable changes, grouped by roadmap phase (the project is pre-1.0 and has no tagged
releases yet). Shipped features are documented in [docs/features.md](docs/features.md);
planned work lives in the [README roadmap](README.md#roadmap).

## Unreleased — Phase 4 (OAuth)

- Native **Anthropic** provider (Phase 5 — native providers): add it with an Anthropic API
  key (Type → Anthropic). Uses the Messages API (version `2026-10-21`) statelessly, with
  streaming, adaptive thinking (`display: "summarized"`, no beta header needed for
  interleaved reasoning between tool calls), parallel tool calls, image input, and model
  discovery that caches each model's reported context window, output cap, vision and
  thinking flags. Signed `thinking`/`redacted_thinking` blocks are stored as provider
  state (signature only — the readable text is in the Thinking block and the server
  ignores it on replay) and replayed verbatim in order; a block over the size cap ends
  signature storage for that response rather than leaving an invalid gap. On Claude
  5.1-generation models, which bind signatures to the request prefix, requests send
  `block_binding: "drop_block"` so edited history (trimming, re-capped tool results,
  changed tools) costs the affected reasoning instead of the turn. `refusal` stops surface
  the classifier's explanation so agent fallbacks engage. The compiler's output budget is
  now clamped to the model's real output cap (a new `max_output_tokens` capability filled
  from discovery), which also fixes oversized `max_tokens` for 1M-window models on other
  providers. Offline mock: `scripts/dev/mock-anthropic-server.mjs`.
- Provider HTTP clients no longer follow redirects (as remote MCP connections already
  didn't): reqwest forwards custom credential headers such as `x-api-key` and
  `x-goog-api-key` to a cross-origin redirect target. A redirecting base URL now fails with
  a hint to use the final address. Credential headers are also marked sensitive.
- Provider settings redesign: opens on a tile picker of provider types with brand logos
  (OpenAI, Google Gemini; a neutral icon otherwise) instead of a Type dropdown; the form gets a
  logo header, *← All providers*, an API-key *Show* toggle and *Get an API key ↗* link, and
  moves context window, image input, token usage and TLS under **Advanced settings**. Save
  returns to the picker with a confirmation; Save & test returns once the connection works and
  a default model is chosen (no longer auto-picking the first model). Unsaved edits are
  confirmed before leaving, and the action bar stays visible while the form scrolls. Provider
  types now carry a `tagline` and `api_key_url`.
- Model hiding: each model in a provider's Advanced settings has a *shown* toggle that
  removes it from the chat picker and agent dropdowns (per-provider `hidden_models`). The
  default model can't be hidden; the current selection and agents already using a hidden
  model keep working. The set survives re-discovery and clears if the provider type changes.
- Fixed: external links open again. The `opener:allow-open-url` permission enabled the command
  but granted no URL scope, so every `openUrl` call was rejected — chat Markdown links and the
  new "Get an API key ↗" links did nothing. `opener:allow-default-urls` scopes opens to
  `mailto:`/`tel:`/`http:`/`https:` in the OS default app; the provider form surfaces a denied
  open instead of failing silently.
- Native **OpenAI** provider (Phase 5 — native providers): add it with an OpenAI API key
  (Type → OpenAI). Uses the Responses API statelessly (`store: false`) — chosen over Chat
  Completions, where OpenAI's newer models restrict or drop tool calling — with streaming,
  reasoning summaries in the Thinking block, parallel tool calls, image input, and model
  discovery that filters out non-chat models and fills in per-family context windows and
  output caps (GPT-6 1.05M / 128K out).
  Encrypted reasoning items are stored as provider state and replayed verbatim, so reasoning
  carries across tool calls; an assistant message's `phase` is replayed too. Tools are sent
  non-strict so MCP schemas are accepted. The mock OpenAI server now also speaks the
  Responses API, with replay validation and a `mock-reasoning` model.
- Provider layer, built to scale with more adapters: capability lookups (every model change
  and run start) no longer construct an adapter or clone the provider config — adapter
  capabilities are declared once per type; HTTP clients for custom/skip TLS modes are cached
  per provider instead of rebuilt (trust store and all) on every run; shared adapter helpers
  (`CallIds`, bounded string appends, opaque-state lookup, argument shaping, model-listing
  filters) and a shared multi-request test server replace per-adapter copies.
- Native **Google Gemini** provider (Phase 5 — native providers, first item): add it with an AI Studio API key
  (Type → Google Gemini). Uses the stable Interactions API (`/v1`) in stateless mode
  (`store: false`), with streaming, thought summaries in the Thinking block, parallel tool
  calls, image input, and model discovery with per-model context windows. Thought steps and
  function-call signatures are stored as provider state and replayed verbatim, in order;
  long thought summaries are dropped from stored state (signature kept) rather than losing
  the block. Thought tokens are counted as output in usage. Streams that close before
  `interaction.completed` (seen on long responses) resume from the last event id, continue
  via `continuation_token`, or retry once — the run no longer fails. Offline mock:
  `scripts/dev/mock-gemini-server.mjs`.
- Provider settings: model-name examples and API-key help text now come from each provider
  type; types with a built-in endpoint are pre-named and keep Base URL/TLS under
  **Advanced — proxy or custom endpoint**.
- Google-style auth errors (HTTP 400 `API_KEY_INVALID`, `UNAUTHENTICATED`,
  `PERMISSION_DENIED`) are reported as "authentication failed — check the API key".
- Fixed: text a model streams after a tool call in the same response no longer splits the
  step, which gave the call a synthetic "did not complete" result and dropped its real result
  from the next request.
- Roadmap: Phases 5–6 are reorganized into **Phase 5 – Native providers** (one row per
  provider, shipped independently) and **Phase 6 – Provider context & caching**.
- Native provider foundation (groundwork for Phase 5; no new provider yet):
  - Provider continuation state (thought signatures, reasoning items) flows through the agent
    loop as ordered `ProviderOpaque` blocks: persisted with the message at its stream
    position, replayed only to the same protocol family, never sent to the UI, excluded from
    search, and capped at 64 KiB per block. Assistant turns now keep their block order when
    replayed.
  - The provider settings form is driven by Rust-owned type specs (`list_provider_kinds`):
    type selector, presets, default endpoint and API-key requirement per protocol.
  - Model discovery can report per-model context windows and capabilities; when you leave the
    context window unset, the provider-reported value is used before the 8K fallback.
  - Shared HTTP layer for adapters: bounded error/listing reads (64 KiB / 16 MiB), URLs
    stripped from transport errors, and one SSE pump used by every streaming adapter.
  - Capability lookups no longer read the keychain or build TLS clients.
  - Sidebar search matches message text and reasoning only — tool arguments and ids no
    longer produce false matches.
  - A whitespace-only API key now clears the stored key instead of saving an empty secret.
  - Tool names exposed to models always start with a letter or underscore (servers whose
    name starts with a digit get a `_` prefix), as some providers require.
  - Reasoning streams read both `reasoning_content` and the `reasoning` alias used by
    mlx-lm and OpenRouter, and whitespace-only thinking channels no longer leave an empty
    Thinking block.
  - In on-demand tool mode, calling a real catalog tool that isn't loaded yet loads it and
    tells the model to retry — the failed call + `find_tools` detour is gone; truly unknown
    names get a "did you mean" hint.

- The streaming caret now follows tokens into nested list items, table cells, and
  blockquotes, instead of dropping to its own line below the list or table, and stays on
  the current code line rather than the line after it.
- Scrolling up while a response streams now stops auto-scroll on the first upward
  wheel, trackpad, or keyboard scroll, instead of having to scroll past the bottom
  threshold before the next token snapped the view back. Scrolling back to the bottom
  (or **Jump to latest**) resumes following.
- Bumped `katex` 0.16.47 → 0.19.1 (Dependabot alert: prototype-pollution gadget
  allowing inherited `trust` options, fixed in 0.18.2) and added an npm `overrides`
  entry so `@vscode/markdown-it-katex`'s nested copy resolves to the same version.
- Scoped tool permissions. Connection permissions are now defaults: the composer's ⚒ Tools
  menu sets per-chat overrides (turn a server off for one chat without disconnecting it;
  Allow / Ask / Disable per tool) and, in agent chats, edits the agent. Agents own their tools
  and permissions — copied from the defaults when a connection is added, with Allow kept only
  for read-only tools and new tools asking. The approval card's *Always allow* saves to the
  chat or agent, not globally. A tool disabled in Connections is a hard ceiling; signature
  pins are kept per scope; a deleted chat's or agent's runs fail closed.
- Connections dialog: an Enabled switch (a connection turned off from the ⚒ Tools menu
  can be turned back on here); Reconnect is hidden and **Save & test** (was *Save &
  connect*) is greyed out while disabled; Save & test waits for the result and reports
  connected/failed with the discovered tool count; a plain Save no longer reconnects
  unless the command, URL, environment or headers changed. A new **Tool permissions**
  section lists every discovered tool with a Read-only / Write / Destructive / Unannotated
  badge and an Allow / Ask / Disable choice, plus "Reset all to default".
- The last tool list per server is cached in `mcp.json`, so permissions stay editable while
  a server is disabled or offline. Always-allow grants (including a server default of
  Allow) now pin the tool's definition when granted or first listed, not at first use.
- Tool cards show a single access badge; read-only tools no longer also show
  "destructive".
- Fixed the attachment picker hiding text files for vision-capable models: the dialog no
  longer uses extension filters, one unusable file in a multi-select is skipped with a
  per-file reason instead of failing the whole pick, and images left pending after
  switching to a text-only model are flagged in the composer and block Send.
- Consistent OpenGent G-and-diamond branding across the README, desktop app icons,
  sidebar, and welcome screens; light/dark horizontal SVG lockups and a ready-to-upload
  social preview. Assistant activity retains the spinning radar and reduced-motion support.
- Browser sign-in (authorization code + PKCE, U2M) and client credentials (M2M) for
  MCP servers; token lifecycle in the OS keychain. *(planned)*

## 0.1.0 — Phases 0–3

### Phase 0 — Foundation

- Release `.app` (5.98 MiB) that launches, resolves `~/.opengent` with private
  permissions, and selects the macOS Keychain as its secret backend.
- `RunCoordinator`: one active run per chat, bounded concurrency, cooperative
  cancellation, sequence-numbered coalesced events; works when invoked from
  non-runtime threads.
- Simulated provider exercising the full UI stream path (phase rail, Stop,
  jump-to-latest).
- Verified against real systems (opt-in tests): Keychain round trip with an 8 KiB
  payload; live stdio MCP handshake and tool listing with
  `@modelcontextprotocol/server-everything`.
- 36 automated tests (30 core, 2 host, 4 UI), clippy-clean, bindings drift check.

### Phase 1 — Workspace & persistence

- SQLite (WAL) database under the data root on a dedicated worker thread, with
  schema migrations; chats, messages, and attachment join tables.
- Rust-owned history: `start_run` loads prior turns from the database, records
  streamed deltas as checkpoints through `RunRecorder`, and finalizes each message
  (completed / cancelled / failed). Startup recovery marks runs that were
  in-progress at shutdown as interrupted.
- Attachments: native file picker, bounded text import with binary/oversized
  rejection, SHA-256 content-addressed blobs under `~/.opengent/attachments` with
  deduplication, and labeled expansion into the model context.
- UI: chat list/sidebar now backed by persisted summaries, rename and delete,
  attachment chips in the composer, and interrupted/failed states restored on
  reload.

### Phase 2 — Custom/local providers

- OpenAI-compatible adapter with an incremental SSE parser (tested at every byte
  split), friendly error mapping (auth, 404, connection refused, idle timeout,
  truncated streams), cancellation.
- Provider store: named connections persisted as JSON under the data root (secrets
  in the keychain only), model discovery via `/models`, connection test, active
  provider/model selection.
- Context compiler: versioned system prompt as a stable prefix, token budget with
  disclosed trimming of the oldest turns (surfaced as a notice in the UI).
- UI: provider settings dialog with presets, provider/model picker in the
  composer, sanitized Markdown rendering (markdown-it with raw HTML off +
  DOMPurify), links opened via the OS browser, retry/regenerate, chat search and
  delete, and a theme that works on the macOS 12 WebKit baseline.

### Phase 3 — MCP agent

- MCP client (rmcp) for **stdio** (child process, allow-listed env, login-shell
  PATH recovery, stderr tail capture) and **Streamable HTTP** (custom headers,
  shared TLS/pool client). Config lives in `~/.opengent/mcp.json`; env/header
  values marked *secret* go to the OS keychain. Standard `mcpServers` JSON can be
  pasted into the Connections dialog.
- `McpManager`: background connect/reconnect, `tools/list_changed` refresh,
  disconnect watchdog, and the `ToolHost` seam used by the run loop. Policies are
  enforced in Rust at dispatch: **Always allow / Ask / Block** per tool plus a
  per-server default; blocked tools are never advertised to the model (only named
  in the system prompt as disabled). *Always allow* grants are bound to the tool's
  definition signature (pinned at grant time or first use) — a changed
  schema/description reverts to *Ask*.
- Agent loop: model → tool calls → (approval pause) → results → model, up to 8
  tool steps and 16 calls per step. Streamed `tool_calls` fragments are assembled
  in the OpenAI-compatible adapter (bounded index/argument size, deduplicated call
  ids); assistant turns persist as ordered content blocks (text, calls, results)
  and reload identically. Arguments must be a JSON object — malformed calls never
  reach the tool.
- **Tools menu** in the composer: connection toggles, per-tool permission submenu,
  and the **tool loading** mode — *All*, *On-demand* (`find_tools` meta-tool loads
  matching tools by keyword), or *Auto* (all when small, on-demand past a
  size/context threshold). The loaded set is recovered from chat history on later
  turns.
- Approval UI on tool cards: Allow once / Always allow / Deny, with arguments
  shown and server-provided destructive/read-only hints; unanswered approvals
  auto-deny after 15 minutes. Denial and blocking are reported back to the model
  as results. New stdio servers, or changes to their command/cwd/env, require a
  native confirmation dialog.
- Sandboxing & hygiene: stdio servers run as process-group leaders with
  kill-on-drop (grandchildren die on disconnect/quit); stderr tails are
  line-capped and redact secrets; `mcp.json` is owner-only (0600); remote
  connections never follow redirects; cancelled calls send
  `notifications/cancelled`. Mock servers for dev: `mock-mcp-server.mjs` (stdio +
  `--http`, `--many N` filler tools) and the mock OpenAI server can emit tool
  calls.

### Agents & schedules

- **Agents** sidebar area: intro page, agent cards, editor for name, description,
  default and fallback model, instructions (appended to a built-in agent system
  prompt), and per-server MCP tool selection. Agent chats reuse the chat UI with
  the agent's name and models.
- **Fallback model**: switches once per run when the default fails before any
  output.
- **Schedules**: once / interval / weekdays-at-time; missed runs (app closed,
  machine asleep) are run once on return or skipped, per schedule. Runs stream
  live and accept approvals when the chat is open. See
  [docs/features.md](docs/features.md#agents).
