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

### Fixed

- **Chat**: Display math no longer flashes a red KaTeX parse error while a reply or Thinking
  block is still streaming; an unfinished formula shows as dimmed source and renders as soon
  as its closing delimiter arrives. A reply that was stopped or cut off keeps its unfinished
  formula as dimmed source instead of ending in an error.
- **Chat**: Scrolling up in a streaming Thinking block no longer stalls or snaps back, and
  formulas no longer render scattered or hide surrounding text while it streams. Long,
  math-heavy replies and reasoning stay responsive because only the changed text is redrawn.
- **Chat**: Square roots and other stretchy math symbols (long arrows, wide accents, braces)
  render again; their drawing was being removed by the HTML sanitizer.

## 0.2.1 — 2026-11-19

### Fixed

- **Chat**: On Windows, after a Thinking block collapsed, wheel scrolling could stop short of
  the rest of the reply and snap back to a stuck position until the chat was revisited.
- **Chat**: A horizontal scrollbar no longer flickers in a Thinking block while the model
  streams; indented or code-formatted reasoning now wraps to the block's width.

## 0.2.0 — 2026-11-16

### Added

- **Tools**: Remote MCP connections can sign in with OAuth in your browser (authorization
  code + PKCE, with automatic client registration or your own client ID), or use OAuth
  client credentials. Tokens stay in the OS credential store and refresh on their own.
- **Tools**: Connections and the ⚒ Tools menu show each server's own icon (inline PNG, JPEG
  or WebP only; nothing is downloaded), matched to the light or dark theme.
- **Tools**: A connection shows the instructions its server sends, with a switch to keep them
  out of the system prompt.
- **Chat**: Attach PDFs (up to 32 MiB) by picker or drag-and-drop. The first 20 pages are
  sent to vision-capable models as labeled page images, so layout, tables and scans come
  through; chips show a thumbnail of page 1 and the page count, and the composer shows
  while a PDF is being rendered. Password-protected and damaged PDFs are skipped with a reason.
- **Chat**: Each request carries at most 100 images and about 16 MiB of image data; once a
  chat's history exceeds that, older messages' images and PDF pages are replaced with a
  labeled placeholder instead of the request failing.

### Changed

- **Chat**: Finished reasoning blocks are now labeled **Thoughts** instead of **Thinking**,
  so they read differently from a block that is still streaming (**Thinking…**).
- **Tools**: **Save** in Connections now returns to the start screen; **Save & test** still
  stays on the connection to show the result.
- **Tools**: On/off controls in the ⚒ Tools menu, the agent editor and schedules are now
  toggle switches instead of checkboxes; a switch whose change fails flips back.
- **Tools**: Server instructions are no longer cut off at 600 characters on one line: the
  system prompt keeps their line breaks, up to 2,000 characters per server, cut at a line break.

### Fixed

- **Tools**: A remote server that needs authentication now shows a short message saying what
  to do instead of a raw transport error, and long errors no longer overflow the Connections
  dialog.
- **Tools**: Remote MCP servers such as GitHub's (`api.githubcopilot.com/mcp`) now connect —
  they negotiate the session down from the protocol version the client advertised and then
  rejected every request, so a valid PAT reported only "Transport channel closed".

### Security

- **Chat**: PDFs are parsed and drawn only by the operating system's own renderer
  (CoreGraphics on macOS, Windows.Data.Pdf on Windows), with caps on file size, pages and
  pixels per page; no PDF parser is bundled with the app. The renderer runs in the app's
  process, not a sandbox — see SECURITY.md for what that means.
- **Tools**: Server instructions are now sent to the model as a marked, untrusted block, with
  invisible characters removed, and only for chats and agents that can use that server's tools.
  If a server changes its instructions, or adds them after first connecting without any, they
  are left out until you accept the new version.
- **Tools**: Server icons larger than 512×512 pixels are ignored, so a tiny file can't make the
  app decode a huge image.

## 0.1.2 — 2026-11-15

### Fixed

- **Chat**: On Windows the chat could stop following a streaming reply, leaving the
  scrollbar short of the content. Browser scroll anchoring (Windows only — macOS never had
  it) is now off, and the view re-pins for late growth such as images, math layout and a
  Thinking block collapsing.
- **Platform**: Windows scrollbars are now overlay-style — thin and shown only while
  scrolling — matching how they already behaved on macOS.

## 0.1.1 — 2026-11-14

### Added

- **Chat**: Drag files from the file manager and drop them anywhere on the chat to attach
  them — same rules as the **⌁ Attach** picker, including per-file rejection reasons.
  Drops while the Agents view or a settings dialog is open are ignored with a notice.

### Changed

- **Chat**: Each reasoning step gets its own Thinking block, shown in order between tool calls
  and the answer, instead of one merged block above the reply.

### Fixed

- **Chat**: A Thinking block with wide code or tables no longer grows past the window edge.
- **Chat**: Scrolling up inside a streaming Thinking block is no longer snapped back to the
  bottom; returning to its bottom resumes following.

- **Tools**: The composer's ⚒ Tools menu now stays inside the window when it is small,
  shifting left and shrinking to fit instead of overflowing the right edge or top.
- **Platform**: The window now opens at about 80% of the screen's usable area, centred, instead
  of a fixed 1280 × 820 that overflowed high-DPI screens such as 4K at 300% scaling.
- **Providers**: **Save & test** stays on the form after a successful test so the discovered
  model list is visible; plain **Save** still returns to the picker.

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
