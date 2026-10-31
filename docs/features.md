# Features

What OpenGent does today, one section per capability. Each entry covers **what it does**,
**how to use it**, **how it works**, and **limits** — keep new features in this format so the
document stays scannable as the app grows.

- [Chat](#chat)
- [Providers & models](#providers--models)
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
