# Features

What OpenGent does today, one section per capability. Each entry covers **what it does**,
**how to use it**, **how it works**, and **limits** — keep new features in this format so the
document stays scannable as the app grows.

- [Chat](#chat)
- [Providers & models](#providers--models)
- [Connections (MCP tools)](#connections-mcp-tools)
- [Attachments](#attachments)
- [History & persistence](#history--persistence)
- [Secrets](#secrets)

## Chat

Streaming conversations with Markdown-rendered replies: code highlighting, KaTeX math,
stop/cancel (Esc), retry, live token usage, and "older messages trimmed" notices when history
exceeds the model's context window.

## Providers & models

Any server speaking the OpenAI Chat Completions protocol — Ollama, LM Studio, vLLM, hosted
gateways such as OpenRouter — plus a built-in simulated provider that needs no account. Per
provider: base URL (HTTPS required off-loopback), optional API key in the OS keychain, model
discovery via `GET /models`, context window override, TLS verification mode (system / pinned CA
/ skip), and image-input policy. The model picker in the composer selects provider+model per
message.

## Connections (MCP tools)

**What.** MCP servers give the model real tools. Add them in **Connections** (sidebar → ⚒):
a *local command* (stdio — e.g. `npx -y @modelcontextprotocol/server-everything`, with
optional env vars and working directory) or a *remote server* (Streamable HTTP endpoint with
optional headers). Paste a standard `{"mcpServers": {…}}` JSON config to prefill the form.
Secret env/header values go to the OS keychain; everything else lives in `mcp.json`.
Servers connect in the background and can be toggled on/off from the ⚒ **Tools** chip in the
composer; status dots and stderr tails surface failures.

**Tool security.** Every tool is *Always allow*, *Ask*, or *Block* — set per tool in the
⚒ Tools menu (each server expands into its tool list) or per server in Connections via the
default policy. Blocked tools are never sent to the model; *Ask* pauses the run on an
approval card (Allow once / Always allow / Deny), *Always allow* persists the choice.
Enforcement happens in Rust at dispatch, not just in the UI.

**Tool loading.** Under ⚒ Tools → *Tool loading*: **Always load all** sends every enabled
tool's schema each request; **On-demand** sends only a `find_tools` search tool — the model
calls it with keywords to load the matching tools, keeping prompts small when many servers
are connected; **Auto** (default) loads all until the definitions exceed ~6k estimated
tokens or a quarter of the context window, then switches to on-demand. Tools loaded through
`find_tools` stay available for the rest of the conversation.

**How it works.** The run loop is model → tool calls → (approval) → results → model (max 8
steps). Calls stream in as `tool_calls` fragments, execute against the MCP connection, and
results feed back; tool cards in the chat show status, arguments, and results. Results are
capped (40k chars, binary parts stubbed) before persistence and replay. Key code:
`crates/agent-core/src/mcp/` (config, stdio env, manager), `agent/tools.rs` (catalog,
naming, search), `agent/coordinator.rs` (loop), `providers/openai_compat.rs` (tool_calls).

**Limits.** Max 8 tool steps per run; calls run sequentially. Stdio children get an
allow-listed environment plus a recovered login-shell PATH so `npx`/`uvx` resolve from GUI
launches. MCP auth (OAuth) is Phase 4 — remote servers that need browser sign-in can't
connect yet.

## Attachments

Attach files to a message with the **⌁ Attach** button (native file picker, up to 20 files,
2 MiB each). Text files are expanded into labeled `<attachment>` excerpts in the prompt.
Images go to models that can see them.

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

**Behavior when unsupported.** The file picker hides its image filter and rejects picked
images; `start_run` re-validates and fails the send with a clear error. Replaying an old chat
that contains images against a text-only model degrades each image to a labeled placeholder
(`status="omitted"`) instead of dropping it silently.

**How it works.** Images are sniffed by magic bytes (a `.png` that isn't one is rejected),
stored as immutable content-addressed blobs under `~/.opengent/attachments/`, base64-encoded
at request time, and rendered as `ContentBlock::Image` — request-path only, never persisted.
Thumbnails load through an IPC command returning a `data:` URL (CSP-safe; the webview has no
file access). Key code: `crates/agent-core/src/providers/capabilities.rs` (heuristic),
`providers/openai_compat.rs` (detection probes, request body), `storage/attachments.rs`
(sniffing, blobs), `agent/context.rs` (render).

**Limits.** 2 MiB per image before base64 inflation; each image is estimated at ~1,500 tokens
for context budgeting. SVG is treated as text (providers don't accept it as image input).

## History & persistence

Chats, messages, and attachment metadata live in SQLite (WAL) under `~/.opengent`. Sidebar
search covers titles and message text; interrupted streams are recovered as failed turns on
next launch. Attachment blobs are deduplicated by SHA-256 and integrity-checked on read.

## Secrets

API keys live only in the OS credential store (macOS Keychain, Windows Credential Manager).
`ProviderConfig` stores a `has_api_key` flag, never the value; nothing secret crosses IPC.
