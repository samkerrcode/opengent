# OpenGent

A model-agnostic desktop agent for macOS and Windows. OpenGent talks to whichever LLM provider you
configure (OpenAI, Anthropic, Google, AWS Bedrock, Hugging Face, or any custom/local endpoint),
streams responses token by token, and takes actions only through explicitly configured MCP servers
with per-tool permissions.

Built with **Tauri 2** (desktop shell), **Svelte 5 + TypeScript** (interface), and **Rust** for
everything privileged: networking, secrets, persistence, the MCP client, and the agent loop.

> **Status: Phase 0 of 8 complete.** The foundation builds, launches, and streams against a
> simulated provider. No real model provider, chat persistence, or MCP tooling is wired up yet.
> See [Roadmap](#roadmap).

---

## Why

Most agent UIs are tied to one vendor, bundle a browser runtime, or hide what the agent is doing.
OpenGent aims for:

- **Provider parity** – cloud and local models are equal citizens behind one adapter interface.
- **Transparent execution** – every turn shows its real phases (generating, awaiting approval,
  calling a tool) with inspectable arguments and results. Nothing is simulated or invented.
- **Permissioned actions** – tools come from MCP servers you add; each tool is *Always allow*,
  *Ask*, or *Block*, enforced in Rust at dispatch time, not just in the UI.
- **Small, fast, local** – a ~6 MiB app using the OS webview; data in `~/.opengent`; secrets in
  the OS keychain; no telemetry.

## Quick start

Prerequisites: Rust (stable, via `rustup`), Node 22+, and the
[Tauri system dependencies](https://v2.tauri.app/start/prerequisites/) for your OS
(Xcode Command Line Tools on macOS; MSVC Build Tools + WebView2 on Windows).

```sh
npm install
npm run tauri:dev      # run the app with hot reload
npm run tauri:build    # release bundle in target/release/bundle/
npm run verify         # svelte-check, vitest, cargo fmt/clippy/test
```

Right now the composer is connected to an in-process **simulated provider**: it echoes your prompt
in streamed chunks so the streaming pipeline, cancellation (Stop / Esc), and event ordering can be
exercised without an account. Real providers arrive in Phase 2.

## Architecture

```
Svelte UI  ──typed commands / ordered Channel──▶  Tauri host (src-tauri)
                                                      │
                                                      ▼
                                             agent-core (Rust, no UI deps)
                                             ├── config      product identity, data root
                                             ├── domain      ids, messages, run events
                                             ├── agent       run coordinator, event emitter
                                             ├── providers   ModelProvider trait + adapters
                                             ├── secrets     SecretStore (Keychain / Cred. Manager)
                                             └── mcp         rmcp client (stdio, Streamable HTTP)
```

Key design rules (see `AGENTS.md` for contributor conventions):

- **One identity source.** `product.json` defines the display name, slug, data directory,
  bundle identifier, and version. A build step propagates it to Rust (`build.rs`), the frontend,
  and the Tauri bundle. The name "OpenGent" is not hardcoded anywhere else.
- **Rust owns the truth.** The webview holds view state only. It has no shell, filesystem, or HTTP
  capability; everything goes through validated commands.
- **Ordered streaming with backpressure.** Run events carry a schema version and a monotonic
  sequence number, are coalesced (~30 ms / 2 KiB) before crossing IPC, and are delivered over
  Tauri Channels. A dropped receiver cancels the run.
- **Secrets never leave Rust.** The core hands out opaque `SecretRef`s; values live in the OS
  credential store and are never written to SQLite, logs, or IPC responses.
- **Provider-native state is preserved.** Messages are typed content blocks; opaque provider
  continuation data (reasoning items, thought signatures) is kept verbatim and never forwarded to
  a different provider.
- **Pinned, vetted dependencies.** Exact versions published ≥ 7 days ago; minimal feature sets.

## Repository layout

| Path | Purpose |
|---|---|
| `product.json` | Product identity (single source of truth) |
| `scripts/product.mjs` | Generates `src/lib/generated/product.ts` and the Tauri config overlay |
| `crates/agent-core/` | Runtime library; fully testable without a window |
| `src-tauri/` | Tauri host: `AppState`, IPC commands, capabilities, CSP, icons |
| `src/` | Svelte app: `features/chat`, `features/history`, `lib/ipc`, `lib/state` |
| `src/lib/generated/bindings/` | TypeScript types generated from Rust DTOs by ts-rs (committed; CI checks drift) |
| `.github/workflows/ci.yml` | macOS + Windows checks, tests, and unsigned build |

## Roadmap

The full design document lives outside the repo for now; this is the phase summary.
Each phase has an acceptance gate that must pass before the next begins.

| Phase | Scope | Status |
|---|---|---|
| **0 – Foundation** | Cross-platform Tauri/Svelte build, product identity, typed IPC, run coordinator with real streaming/cancellation, native keychain + rmcp feasibility, CI | ✅ Done (macOS verified; Windows via CI, not yet run) |
| **1 – Workspace & persistence** | SQLite (WAL) chat history, search, attachments as immutable content-hashed snapshots, restart recovery, command palette, inspector panel | ⏳ Next |
| **2 – Custom/local providers** | OpenAI-compatible Chat Completions streaming, provider/model settings, capability flags, incremental sanitized Markdown, context limits. *First usable chat.* | |
| **3 – MCP agent** | stdio + Streamable HTTP servers, tool discovery, Always allow / Ask / Block policies, approval UI, full loop: model → approval → tool → result → model. *Agent alpha.* | |
| **4 – OAuth** | Browser sign-in (authorization code + PKCE, U2M) and client credentials (M2M) for MCP servers; token lifecycle in the keychain | |
| **5 – OpenAI & Anthropic native** | Responses API and Messages API adapters, provider-aware prompt caching, cross-provider context projection | |
| **6 – Google, Bedrock, Hugging Face** | Gemini (thought signatures), Bedrock ConverseStream (feature-gated AWS SDK), HF presets | |
| **7 – Release hardening** | Performance profiling, accessibility, export/import, signing & notarization, installers | |
| **8 – Extensions** | Sub-agents, A2A, skills, hooks, model router — through the existing `ModelProvider` / tool / permission seams | |

### What Phase 0 delivered

- Release `.app` (5.98 MiB) that launches, resolves `~/.opengent` with private permissions, and
  selects the macOS Keychain as its secret backend.
- `RunCoordinator`: one active run per chat, bounded concurrency, cooperative cancellation,
  sequence-numbered coalesced events; works when invoked from non-runtime threads.
- Simulated provider exercising the full UI stream path (phase rail, Stop, jump-to-latest).
- Verified against real systems (opt-in tests): Keychain round trip with an 8 KiB payload; live
  stdio MCP handshake and tool listing with `@modelcontextprotocol/server-everything`.
- 36 automated tests (30 core, 2 host, 4 UI), clippy-clean, bindings drift check.

### Known gaps

- Windows build has not been executed yet (CI job exists).
- No packaged end-to-end UI test; the webview path is verified manually via `tauri:dev`.
- Chats are in-memory until Phase 1. Markdown rendering is plain text until Phase 2.
- Builds are unsigned. The DMG step can fail if a previous build's image is still mounted
  (`hdiutil detach`); the `.app` itself is unaffected.

## Development notes

```sh
npm run gen:types                                  # regenerate TS bindings after changing #[derive(TS)] types
cargo test -p agent-core -- --ignored              # live tests (keychain; MCP needs the env var below)
OPENGENT_TEST_MCP_COMMAND="npx -y @modelcontextprotocol/server-everything" \
  cargo test -p agent-core live_stdio_probe -- --ignored
OPENGENT_DATA_DIR=/tmp/og ./target/release/opengent-desktop   # run against an alternate data root
OPENGENT_LOG=debug npm run tauri:dev                          # tracing filter
```

Environment variable names derive from the product slug, so they follow a rename of `product.json`.

## Security model (short version)

- Strict CSP; webview has only `core:default`, dialog-open, and opener permissions.
- Provider and MCP traffic originates in Rust. HTTPS required except explicit loopback endpoints.
- MCP tool permission prompts reduce risk but are **not** an OS sandbox: a local stdio server can
  do whatever your user account can. Only add servers you trust.
- Local chat data is not encrypted at rest (relies on OS account isolation / disk encryption);
  at-rest encryption is a separate future feature.

## License

MIT OR Apache-2.0
