# Changelog

What changed in each release, newest first. For what the app does today see the
[feature guide](docs/features.md); for what's planned see the
[roadmap](README.md#roadmap); for how versions are numbered see
[docs/releasing.md](docs/releasing.md#versioning).

**Format.** One section per release (`## X.Y.Z — YYYY-MM-DD`), with changes grouped as
*Added*, *Changed*, *Fixed*, *Security* and *Removed* — only the groups a release needs.
Entries are one or two lines, written for users, and start with the area they touch (**Chat**,
**Providers**, **Tools**, **Agents**, **Platform**). Breaking changes are marked
**Breaking**. How a feature works belongs in the feature guide, not here. New entries go
under *Unreleased*.

## Unreleased

Nothing yet.

## 0.1.0 — 2026-11-11

First release. It covers roadmap Phases 0–3 (foundation, persistence, OpenAI-compatible
providers, MCP agent loop), agents and schedules, and the first three native providers of
Phase 5. Installers are unsigned; see [installing a release](docs/releasing.md#installing-a-release).

### Added

**Chat**

- Streaming conversations with real run phases, Stop (Esc), Regenerate, copy buttons for
  replies and code blocks, live token usage, and *Jump to latest*.
- Sanitized Markdown with syntax-highlighted code, KaTeX math and a collapsible Thinking
  block for models that expose reasoning. Links open in the system browser.
- Attachments: text files as labeled excerpts and PNG/JPEG/GIF/WebP images for
  vision-capable models (up to 20 files, 2 MiB each), stored as deduplicated,
  content-addressed snapshots.
- Local chat history in SQLite with sidebar search, rename and delete. Replies are saved as
  they stream; a run interrupted by a quit or crash is recovered on the next launch.
- Context budgeting: when history exceeds the model's window the oldest turns are trimmed and
  the reply says so.

**Providers**

- **OpenAI-compatible** provider for any Chat Completions server, with presets for Ollama,
  LM Studio, vLLM / llama.cpp and OpenRouter.
- Native **OpenAI** provider (Responses API, stateless with `store: false`): reasoning
  summaries, encrypted reasoning carried across tool calls, built-in context limits per model
  family.
- Native **Google Gemini** provider (Interactions API, stateless with `store: false`): thought
  summaries, signed thoughts replayed across tool calls, automatic recovery of streams that
  close early.
- Native **Anthropic** provider (Messages API, stateless): adaptive thinking with summaries,
  signed thinking blocks replayed across tool calls, refusals reported so agent fallbacks
  engage.
- **Simulated** provider for trying the app with no account.
- Provider settings: a tile picker of provider types, Save & test with model discovery, API
  keys stored in the OS keychain, and Advanced settings for proxy endpoints, TLS (system /
  pinned CA / skip), context window, image input and token usage.
- Model list per provider with per-model context windows, automatic vision detection, and a
  *shown* toggle to hide models from the pickers.
- Provider continuation state (reasoning items, thought and thinking signatures) is stored
  locally, replayed only to the provider family it came from, and never shown or searched.

**Tools**

- MCP connections over **stdio** and **Streamable HTTP**, with background reconnect, pasted
  `mcpServers` JSON import, secret env/header values in the keychain, and Save & test.
- Per-tool permissions — *Always allow* / *Ask* / *Block* — enforced in Rust at dispatch.
  Access badges (Read-only / Write / Destructive / Unannotated) come from server annotations.
- Scoped permissions: connection settings are the defaults; chats override them from the
  composer's ⚒ Tools menu; agents own their set. A tool disabled in Connections is a hard
  ceiling.
- Approval cards with Allow once / Always allow / Deny and the call's arguments; unanswered
  approvals are denied after 15 minutes.
- Agent loop: model → approval → tool → result → model, up to 8 steps and 16 calls per step,
  with tool cards showing arguments and results.
- Tool loading modes: *All*, *On-demand* (a `find_tools` search tool) and *Auto*.

**Agents**

- Agents: name, description, default and fallback model, instructions, and per-agent tools
  and permissions. Write, destructive and unannotated tools start at *Ask*.
- Fallback model: takes over once per run when the default fails before producing output.
- Schedules: once, every N minutes/hours/days, or weekdays at a local time, with a per-schedule
  policy for missed runs (catch up once, or skip). Runs stream live and accept approvals when
  the chat is open.

**Platform**

- Native desktop app for macOS 12+ and Windows 10+ (Tauri 2, Svelte 5, Rust core), about
  6 MiB, with data under `~/.opengent` and no telemetry.
- Product identity from a single `product.json`.
- Tag-driven release workflow: universal macOS `.dmg`, Windows x64 `.exe` and `.msi`, and
  `SHA256SUMS.txt` on a draft GitHub release, gated on all version fields agreeing.

### Security

- Secrets (API keys, secret env/header values) exist only in the OS credential store and in
  Rust memory — never in the webview, SQLite, settings files or logs.
- The webview has no shell, filesystem or HTTP capability; a strict CSP applies, and external
  links open only `http(s)`/`mailto`/`tel` URLs in the OS default app.
- HTTPS is required for non-loopback endpoints. Neither provider nor remote MCP connections
  follow redirects, so credential headers stay on the configured origin.
- *Always allow* grants are pinned to the tool's definition per scope; a changed schema or
  description reverts the tool to *Ask*.
- Adding a stdio server, or changing its command, working directory or environment, requires
  a native confirmation dialog. Stdio servers get an allow-listed environment and are killed
  with their child processes on disconnect or quit.
- Server-supplied tool metadata and results are treated as untrusted and size-bounded before
  they reach the model.

### Known limitations

See [Known limitations](README.md#known-limitations) in the README.
