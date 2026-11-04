# Changelog

Notable changes, grouped by roadmap phase (the project is pre-1.0 and has no tagged
releases yet). Shipped features are documented in [docs/features.md](docs/features.md);
planned work lives in the [README roadmap](README.md#roadmap).

## Unreleased — Phase 4 (OAuth)

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
