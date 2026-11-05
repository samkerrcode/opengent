# Features

What OpenGent does today, one section per capability. Each entry covers **what it does**,
**how to use it**, **how it works**, and **limits** — keep new features in this format so the
document stays scannable as the app grows.

- [Chat](#chat)
- [Branding & activity indicators](#branding--activity-indicators)
- [Providers & models](#providers--models)
- [Connections (MCP tools)](#connections-mcp-tools)
- [Agents](#agents)
- [Scheduled agent runs](#scheduled-agent-runs)
- [Attachments](#attachments)
- [History & persistence](#history--persistence)
- [Secrets](#secrets)

## Chat

Streaming conversations with Markdown-rendered replies: code highlighting, KaTeX math,
stop/cancel (Esc), retry, live token usage, and "older messages trimmed" notices when history
exceeds the model's context window. While a reply streams, a caret marks where the next token
will appear: at the end of the deepest trailing text (nested list items, the last table cell,
blockquotes, the current code line) and just after rendered math, never inside it.

## Branding & activity indicators

**What.** The open G with a solid diamond identifies the app in the Dock/taskbar, sidebar,
and chat/agent welcome screens. Assistant responses retain the radar-like activity symbol.

**How to use.** No setup is needed. The radar spins while a run is active and stops at a
terminal state; the adjacent phase label describes what is happening. Enable the OS
reduced-motion preference to disable its rotation.

**How it works.** In-app brand marks share the README's canonical SVG and use the theme's
accent color. The sidebar name comes from product identity, not an embedded image.
Desktop icons and the social preview are generated from that same mark.

**Limits.** The radar indicates an active run, not a percentage complete or a security
guarantee. Desktop icons are packaged at build time; the repository social preview must
be uploaded manually. Asset exports and regeneration instructions live in
[the branding asset guide](assets/README.md).

## Providers & models

Any server speaking the OpenAI Chat Completions protocol — Ollama, LM Studio, vLLM, hosted
gateways such as OpenRouter — native **OpenAI** and **Google Gemini**, plus a built-in
simulated provider that needs no account. Per provider: base URL (HTTPS required off-loopback), API key in the OS
keychain, model discovery via `GET /models`, context window override, TLS verification mode
(system / pinned CA / skip), and image-input policy. The model picker in the composer selects
provider+model per message.

**Provider types.** *Add provider* offers a **Type** choice (OpenAI-compatible, OpenAI,
Google Gemini). Each type's label, description, default endpoint, presets, model-name examples,
API-key help text and whether a key is required come from Rust (`list_provider_kinds`), so
the form never hardcodes a protocol. A type with a default endpoint is pre-named after its
type, fills the base URL in for you (leaving it empty uses the default), and tucks Base URL
and TLS settings behind **Advanced — proxy or custom endpoint**; the section opens on its own
when a saved provider uses a non-default endpoint or TLS mode.

**How it works (all providers).** Adapter-level capabilities (streaming, tools, parallel
calls) are declared once per type in Rust, so resolving a model's capabilities — done on every
model change and run start — reads the saved config in place without building an adapter,
touching the keychain or cloning the model list. HTTP clients for *pinned CA* / *skip
verification* TLS modes are built once per provider and reused until its TLS settings change.

### OpenAI

**What.** OpenAI models (GPT-4.1, GPT-5 and later, o-series) through the **Responses API**,
with streaming, reasoning summaries in the Thinking block, parallel tool calls and image
input. Reasoning is carried across tool calls, so the model doesn't re-derive its plan after
every tool result.

**How to use.** Create a key at [platform.openai.com](https://platform.openai.com) → *API
keys*. In Provider settings choose *Add provider* → Type **OpenAI**, paste the key and **Save
& test**: the model list is fetched and you pick a default model (e.g. `gpt-5-mini`). The
endpoint defaults to `https://api.openai.com/v1`; open *Advanced* only for a proxy or a
Responses-compatible gateway. (To use Chat Completions against OpenAI instead, add an
*OpenAI-compatible* provider pointed at the same URL.)

**How it works.** `crates/agent-core/src/providers/openai.rs`.
- **Why Responses, not Chat Completions.** OpenAI's newer models restrict tool calling on
  Chat Completions (GPT-5.4+ only with reasoning off; GPT-6 not at all) and expose reasoning
  summaries only on Responses. Responses is used **statelessly**: `store: false`, the full
  local history is sent each turn and OpenAI keeps nothing.
- Requests go to `POST {base}/responses` with `stream: true` and the key in the
  `Authorization` header. The system prompt is sent as `instructions`; history becomes an
  ordered list of input items: your messages → `message` (`input_text` + `input_image`), the
  model's text → assistant `message`, tool calls → `function_call`, results →
  `function_call_output` (paired by `call_id`). Tools are sent with `strict: false`, since MCP
  schemas rarely meet strict-mode rules.
- **Reasoning models** (o-series, GPT-5+, `codex`; not `-chat` variants) get
  `reasoning: {summary: "auto"}` and `include: ["reasoning.encrypted_content"]`. Each finished
  `reasoning` item (encrypted content + summary) is stored as provider state at its stream
  position and replayed verbatim next turn. Item ids are never stored or sent — with
  `store: false` there is no server state for them to reference. An assistant message's
  `phase` (commentary vs final answer, on newer models) is stored and replayed with it.
- **Reasoning storage decision.** If an item would exceed the 64 KiB provider-state cap, its
  summary is dropped (the text is kept in the Thinking block); if the encrypted content alone
  is still too large, the item isn't stored and the model simply reasons afresh next turn.
- Calls and reasoning items are emitted when their item finishes, so stored blocks keep the
  server's order. A call cut off by the output limit (`status: incomplete`) is dropped rather
  than run with truncated arguments.
- Usage: input, output (reasoning tokens are already included) and cached input tokens.
  `response.completed` → end of turn or tool use; `response.incomplete` → output limit, or an
  error for the content filter; `response.failed` / `error` events → error with the server's
  message.
- Discovery: `GET {base}/models`, minus non-chat families (embeddings, TTS/transcription,
  image/video generation, realtime/audio, moderation, search/deep-research, computer use,
  legacy completions). OpenAI's listing reports no limits, so a built-in table supplies
  per-family context windows (e.g. GPT-6 1.05M, GPT-4.1 ≈ 1M, GPT-5 272K input, o-series 200K,
  GPT-4o 128K) plus vision and reasoning flags; your per-model setting always wins. The
  compiler's 25%-of-window output budget is clamped to each model's real output cap so large
  windows never trigger a rejected `max_output_tokens`.
- The stream idle timeout is 180 s, since reasoning models can think silently for a while.

*Limits:* API keys only (no Azure OpenAI endpoints, organization/project headers or OAuth).
No server-side state (`previous_response_id` chaining, conversations, background mode) and no
stream resume: a dropped connection fails the turn. OpenAI's built-in tools (web search, file
search, code interpreter, computer use, remote MCP) and structured outputs aren't offered.
Reasoning effort and verbosity are the model defaults. Context windows for models newer than
the built-in table fall back to your setting or 8K. Images inside tool results are sent as
text, as with other providers.

Offline testing: `node scripts/dev/mock-openai-server.mjs` serves the Responses API too (see
AGENTS.md).

### Google Gemini

**What.** Gemini models (and Gemma models served by the Gemini API) through Google's
Interactions API, with streaming, thought summaries, parallel tool calls and image input.

**How to use.** Create an API key in [Google AI Studio](https://aistudio.google.com) → *Get
API key*. In Provider settings choose *Add provider* → Type **Google Gemini**, paste the key and
**Save & test**: the model list is fetched and you pick a default model (e.g.
`gemini-flash-latest`). The endpoint defaults to the stable API
(`https://generativelanguage.googleapis.com/v1`); open *Advanced* only for a proxy or to use
`…/v1beta`.

**How it works.** `crates/agent-core/src/providers/gemini.rs`.
- Requests go to `POST {base}/interactions` with `store: false` (Google keeps nothing; the
  chat history stays local and the full history is sent each turn) and `stream: true`. The
  key is sent in the `x-goog-api-key` header, never in the URL.
- History becomes an ordered list of steps: your messages → `user_input` (text + images), the
  model's text → `model_output`, tool calls → `function_call` (arguments as an object), tool
  results → `function_result` (with the tool name).
- Reasoning arrives as `thought` steps; summaries (`thinking_summaries: "auto"`, not requested
  for Gemma models) stream into the Thinking block. Google requires model steps to be
  replayed exactly as received, so each `thought` step is stored verbatim as provider state
  and re-sent at its original position, and a signature on a `function_call` step is stored
  and re-attached to that call. Steps are emitted as they finish, so interleaved
  thought/call sequences keep their order.
- **Thought storage decision.** The complete thought step (signature + summary) is stored,
  because the API asks for verbatim replay. When a long summary would push the block past the
  64 KiB provider-state cap, the summary is dropped and only the signature is kept: the
  signature is what the server validates, and the readable summary is still stored in the
  message's Thinking block. A thought step with neither signature nor summary is not stored.
- Usage: input, output **including thought tokens** (billed as output), and cached tokens.
  Status `completed`/`requires_action` → end of turn or tool use, `incomplete` → output limit,
  `failed`/`cancelled` → error.
- **Stream recovery.** Google's stream can close before `interaction.completed` on long
  responses. The adapter recovers transparently: a `requires_continuation` status resends
  the request with its `continuation_token`; a dropped connection resumes with
  `GET /interactions/{id}?last_event_id=…` (each event carries a resumable id; skipped when
  `store: false` yields no interaction id); and as a last resort the interaction is retried
  once. Only when all of that fails does the run error.
- Discovery pages through `GET {base}/models`, keeps chat models (those supporting
  `generateContent`, minus embedding, TTS, image/video/music and similar families), and caches
  each model's input-token limit as its context window and its `thinking` flag. Listed models
  count as vision-capable unless you set *Image input* to *Not supported*.
- A bad key (Google answers HTTP 400 `API_KEY_INVALID`) is reported as an authentication
  failure. The stream idle timeout is 180 s, since Pro models can think silently for a while.

*Limits:* AI Studio API keys only — no Vertex AI, OAuth or ADC. No server-side state
(`previous_interaction_id`) or background runs; a resumed/restarted stream may briefly
repeat output the earlier connection already delivered. Google's built-in tools (Search, code execution,
URL context, file search, MCP) and structured output aren't offered. Thinking level is the
server default. Images inside tool results are sent as text placeholders, as with other
providers (sending them as image content is a possible follow-up). Mid-turn switches from
another provider into Gemini (e.g. an agent fallback during a tool loop) replay the earlier
calls without signatures; whether Google accepts that for every model is unverified.

Offline testing: `node scripts/dev/mock-gemini-server.mjs` (see AGENTS.md).

**Context windows.** Resolution per model: your per-model override → your provider-wide
setting (a deliberate cap) → the window the provider reports during discovery → 8K. Image
support similarly combines your setting, server-reported flags and a name heuristic.

**Provider state.** Some native APIs return opaque continuation data the next request must
replay (Gemini thought signatures, OpenAI encrypted reasoning items, Anthropic thinking
signatures).
It is stored with the assistant message in stream order and replayed only to adapters of the
same protocol; switching a chat to another provider simply drops it from that request. It
never reaches the UI (not streamed, stripped from chat loads), is excluded from search, and
each block is capped at 64 KiB.

*Limits:* state from a provider family is only replayed when the selected model (or an
agent's fallback) speaks that family.

## Connections (MCP tools)

**What.** MCP servers give the model real tools. Add them in **Connections** (sidebar → ⚒):
a *local command* (stdio — e.g. `npx -y @modelcontextprotocol/server-everything`, with
optional env vars and working directory) or a *remote server* (Streamable HTTP endpoint with
optional headers). Paste a standard `{"mcpServers": {…}}` JSON config to prefill the form.
Secret env/header values go to the OS keychain; everything else lives in `mcp.json`.
Servers connect in the background and are enabled or disabled only with the **Enabled**
switch at the top of a connection in Connections (the ⚒ Tools menu turns a server off for one
chat or agent without disconnecting it); status dots and stderr tails surface failures. While a connection is disabled, Reconnect is hidden and
**Save & test** is greyed out. **Save & test** saves, connects, waits for the handshake and
reports the result ("connected · N tools discovered" or the error); a plain **Save** only
reconnects when the command, working directory, environment, URL or headers changed.

**Tool permissions.** A connection's **Tool permissions** section lists every tool the
server reported, each with an access badge and an *Allow* / *Ask* / *Disable* choice, plus
*Default for new tools* and *Reset all to default*. Changes save immediately. The list is
cached in `mcp.json` after each successful listing, so it stays visible ("last seen") and
editable while the server is disabled or offline. Badges come from the server's MCP
annotation hints and are informational only — never used to grant anything:
*Read-only* (`readOnlyHint`), *Write* (`destructiveHint: false`), *Destructive*
(`destructiveHint: true`), and *Unannotated* (no hints given). Unannotated tools get a
dashed amber badge and a note: the MCP spec defaults them to potentially destructive, so
*Ask* is the safe choice.

**Scoped permissions (chats and agents).** Connection permissions are the **defaults**.
Every run also has a scope that can adjust them:

- *Plain chats* — the composer's ⚒ **Tools** menu ("Tools · this chat") sets overrides for
  that chat only: turn a server off for the chat (it stays connected for everything else),
  or set a tool to Allow / Ask / Disable. Overridden tools show a dot; click it, or *Reset to
  defaults*, to follow Connections again. New chats start with no overrides.
- *Agents* — own their tools and permissions, edited in the agent editor or from an agent
  chat's ⚒ Tools menu ("Tools · agent name"). See [Agents](#agents).
- The approval card's **Always allow** saves to the run's scope ("Always allow in this chat"
  / "for this agent"), never to the global defaults.

Resolution, enforced in Rust (`mcp/permissions.rs`): a disabled connection offers nothing;
tools off in the scope are neither offered nor named; a tool set to *Disable* in Connections
is a **hard ceiling** no chat or agent can turn back on (menus show "off in Connections");
otherwise the scope's choice applies, falling back to the connection's. Dispatch re-checks the
scope on every call, so turning a server or tool off mid-run blocks its next call. A run
whose chat or agent was deleted fails closed (every tool asks); deleting an agent also
cancels its running chats.

**Tool security.** Every tool is *Always allow*, *Ask*, or *Block* — set per tool in
Connections → Tool permissions (the defaults), per chat or agent as above, or per server via
the default. Blocked tools are never sent to the model; *Ask* pauses the run on an approval
card (Allow once / Always allow / Deny), *Always allow* persists the choice. Enforcement
happens in Rust at dispatch, not just in the UI. Two extra guardrails: *Always allow* pins a
signature of the tool's definition, **per scope** — a connection grant is pinned when granted
(from the live definition, or the cached one when offline) or, for a default Allow, when the
tool is first listed (only configs predating the cache pin on first use); a chat/agent grant
is pinned when saved, copying the connection's pin when there is one, so copying a stale
grant keeps it stale, and an approval-card grant pins the definition just approved. Scoped
grants never pin on first use, and re-approving in one scope never re-validates another. If
the server changes the schema or description the tool falls back to *Ask* and shows a
"changed" badge; and a new stdio server,
or a change to its command, working directory or environment, must be confirmed in a native
OS dialog, so server installs can't be driven silently. A model that calls a disabled tool
is told it is disabled (and to say so) rather than that it doesn't exist. Approval cards open
their arguments automatically and carry the server's destructive/read-only hints; unanswered
approvals auto-deny after 15 minutes.

**Tool loading.** Under ⚒ Tools → *Tool loading*: **Always load all** sends every enabled
tool's schema each request; **On-demand** sends only a `find_tools` search tool — the model
calls it with keywords to load the matching tools, keeping prompts small when many servers
are connected; **Auto** (default) loads all until the definitions exceed ~6k estimated
tokens or a quarter of the context window, then switches to on-demand. Tools loaded through
`find_tools` stay available for the rest of the conversation. If the model calls a real
catalog tool that isn't loaded yet, it is loaded on the spot so the retry carries its
definition — a `find_tools` call is only needed for names that don't exist.

**How it works.** The run loop is model → tool calls → (approval) → results → model (max 8
steps, max 16 calls per step). Calls stream in as `tool_calls` fragments, execute against
the MCP connection, and results feed back; tool cards in the chat show status, arguments,
and results. Results are capped before persistence and replay: the lesser of 40k chars or
a quarter of the context window, re-capped to 2k on later turns; binary parts become
labeled stubs. Arguments must be a JSON object — malformed calls are rejected before the
approval step. Stdio servers run as process-group leaders with kill-on-drop, so `npx →
node` grandchildren die on disconnect or app quit; a timed-out or cancelled call notifies
the server via `notifications/cancelled`. Remote (HTTP) connections never follow
redirects, keeping credential headers on the origin. Stderr tails are line-capped and
redact known secret values. Key code: `crates/agent-core/src/mcp/` (config, stdio env,
manager), `agent/tools.rs` (catalog, naming, search), `agent/coordinator.rs` (loop),
`providers/openai_compat.rs` (tool_calls).

**Limits.** Max 8 tool steps per run and 16 calls per step; calls run sequentially. Per
server: up to 500 tools, 64 env/header entries, 1 KB tool descriptions, 16 KB input
schemas (oversized tools are skipped). Stdio children get an allow-listed environment plus
a recovered login-shell PATH so `npx`/`uvx` resolve from GUI launches. Server-supplied
metadata (descriptions, schemas) is treated as untrusted and bounded before it reaches the
model; the cached tool list keeps at most 500 tools per server, with titles capped at 128
and descriptions at 256 characters, and is display/pinning only — calls always need the
live server. Chat overrides and agent access accept at most 64 servers and 500 tools per
server, with tool names up to 128 characters; scoped pins of deleted chats/agents are pruned
at startup. MCP auth (OAuth) is [Phase 4](../README.md#roadmap) — remote servers that need
browser sign-in can't connect yet.

## Agents

**What it does.** Reusable assistants with their own name, description, default model,
optional fallback model, instructions, and its own MCP servers, tools and tool permissions.
Agent chats use the regular chat interface, labeled with the agent's name.

**How to use it.** Click **Agents** in the sidebar. With no agents you get an intro page and a
*Create your first agent* button. The editor has four sections: *Identity* (name, description),
*Models* (default + optional fallback), *Instructions* (your system prompt), and *Tools*: tick
a connection to add it, then **Edit tools & permissions…** to turn each tool on/off and choose
*Allow* or *Ask*, plus *New tools* (Ask or Off) for tools the server adds later. Adding a
connection copies its defaults from Connections — read-only tools keep *Allow*; write,
destructive and unannotated tools start at *Ask*, because agents can run unattended. The card
summarizes how many tools run without asking (highlighted when some can make changes);
*Reset to connection defaults* copies them again. The same controls are in an agent chat's
⚒ Tools menu, which saves straight to the agent. **Save & start chat** opens a new chat with
the agent; the composer shows the agent chip instead of the model picker. Agent cards also
offer **Chat** and **Configure**.

**How it works.** Agents are stored in SQLite (`agents` table; config as JSON). Chats carry an
`agent_id`, and Rust resolves the agent's models, tools and prompt at run time — the webview
never supplies them for agent chats. The system prompt is a built-in agent prompt
(`agent_system_prompt` in `agent/context.rs`: identity, operating rules, untrusted-data
handling, and an unattended-run section for schedules), then the tools section, then your
instructions appended verbatim under *Agent instructions*. The agent's permissions
(`AgentServerAccess`: `default_policy` + per-tool `policies`, `block` = off) are its run
scope (`ScopePolicies::Agent`, resolved in `McpManager::prepare_tools` and at dispatch);
servers the agent doesn't use are neither offered nor named, and tools disabled in
Connections stay disabled. Agents saved before per-agent permissions follow the connection's
permissions until opened in the editor, which converts them (an old *Selected tools* list
becomes per-tool on/off) and saves on **Save**. The fallback model is used once per run when the default fails **before producing any
output** (connection/auth/model errors); the turn shows a notice and is attributed to the
fallback model (`ModelSwitched` run event). Context is budgeted for the smaller of the two
models' windows.

**Limits.** The fallback does not engage after partial output (it would duplicate text).
Image attachments are gated on the default model's capabilities. Deleting an agent removes its
schedules; its chats remain as plain chats. Agents can't allow tools the server adds later
without asking: *New tools* is Ask or Off, and a tool needs an explicit *Allow* (pinned to its
definition at save time). Turning a connection off in an agent chat's menu and back on again
re-copies the defaults rather than restoring earlier per-tool choices.

## Scheduled agent runs

**What it does.** Runs an agent with a preset prompt once, every N minutes/hours/days, or on
chosen weekdays at a local time. Each run opens a new chat (marked ⏱ in the sidebar).

**How to use it.** In a saved agent's editor, **Add schedule**: prompt, repeat rule, what to do
with missed runs, enabled. Each schedule shows its next run, last outcome (with a link to the
chat), a pause toggle, **Run now**, Edit and Delete.

**Missed runs.** If the app was closed or the computer asleep at the scheduled time, the
schedule's policy decides: **Run it once when the app is back** (several missed occurrences
coalesce into one catch-up run) or **Skip it** and wait for the next time. A run counts as
missed when first seen more than 2 minutes after it was due.

**How it works.** `agents/schedule.rs` holds the pure logic (`next_after`, `plan_due`,
DST-aware weekday times via chrono in the OS time zone). `src-tauri/src/scheduler.rs` ticks
every 20 s against wall-clock time (so sleep is detected even though timers pause), persists the
decision *before* starting the run (no double fires after a crash), and waits up to 30 s at
startup for MCP servers to connect so catch-up runs have their tools. Scheduled runs stream
over app events (`background-run-started`, `background-run-event`); if the chat is open you see
it live and can answer approval prompts. The agent is told the run is unattended and must not
wait for answers.

**Limits.** Runs only while the app is open (no OS-level launch agent). Tool calls that need
approval are auto-denied after 15 minutes if nobody answers. Interval schedules count from when
they were saved/last ran, not from a fixed clock time. No OS notifications yet.

## Attachments

Attach files to a message with the **⌁ Attach** button (native file picker, up to 20 files,
2 MiB each). The picker accepts any file; what it can't use is skipped per file with a
reason instead of failing the whole selection. Text files are expanded into labeled
`<attachment>` excerpts in the prompt. Images go to models that can see them.

### Image attachments

**What.** PNG, JPEG, GIF, and WebP images can be attached to a prompt. They appear as
thumbnails in the composer and in the sent message, and are sent to the model as base64
`data:` URLs in the OpenAI `image_url` content-part format.

**Which models.** Whether a model accepts images is resolved per provider+model, in order:

1. **Your setting** — Provider Settings → *Image input*: `Auto-detect` (default),
   `Supported` (all models on the provider), or `Not supported`. Always/Never wins outright.
2. **Server-reported** — in `auto`, detection runs when you save & test a provider or refresh
   its model list: OpenRouter-style `architecture.modality`, LM Studio `type: "vlm"` (via
   `/v1/models` or `/api/v0/models`), and Ollama per-model `capabilities` via `/api/show`.
   Results are cached on the provider (`vision_models`) and marked "· vision" in the picker.
3. **Name heuristic** — in `auto`, well-known vision families (gpt-4o/4.1/5, o3/o4,
   claude ≥ 3, gemini, llava, qwen-vl, pixtral, gemma3, and similar) count as capable even
   when the server reports nothing.

**Behavior when unsupported.** The picker shows all files (a type list can't describe
"any UTF-8 text or sniffed image" without hiding attachable files); picked images are
skipped with a per-file reason. Images already pending when you switch to a text-only
model are flagged in the composer and block Send until removed. `start_run` re-validates
and fails the send with a clear error. Replaying an old chat that contains images against
a text-only model degrades each image to a labeled placeholder (`status="omitted"`)
instead of dropping it silently.

**How it works.** Images are sniffed by magic bytes (a `.png` that isn't one is rejected),
stored as immutable content-addressed blobs under `~/.opengent/attachments/`, base64-encoded
at request time, and rendered as `ContentBlock::Image` — request-path only, never persisted.
Thumbnails load through an IPC command returning a `data:` URL (CSP-safe; the webview has no
file access). Key code: `crates/agent-core/src/providers/capabilities.rs` (heuristic),
`providers/openai_compat.rs` (detection probes, request body), `storage/attachments.rs`
(sniffing, blobs), `agent/context.rs` (render).

**Limits.** 2 MiB per image before base64 inflation; each image is estimated at ~1,500 tokens
for context budgeting. SVG is treated as text (providers don't accept it as image input).
Formats not yet supported — to be added later — include HEIC/HEIF (iPhone photos), BMP,
TIFF, PDF and other document types, and non-UTF-8 text (UTF-16, Latin-1); they are skipped
with a reason at pick time.

## History & persistence

Chats, messages, and attachment metadata live in SQLite (WAL) under `~/.opengent`. Sidebar
search covers titles and message text and reasoning (not tool arguments, ids or provider
state); interrupted streams are recovered as failed turns on
next launch. Attachment blobs are deduplicated by SHA-256 and integrity-checked on read.

## Secrets

API keys live only in the OS credential store (macOS Keychain, Windows Credential Manager).
`ProviderConfig` stores a `has_api_key` flag, never the value; nothing secret crosses IPC.
