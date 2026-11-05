# OpenGent – contributor notes

Frontier quality model-agnostic desktop agent and harness. Tauri 2 host + Svelte 5 UI; all agent logic in Rust (`crates/agent-core`).
Design/roadmap: see the phased plan and proposed features in README.md; delivered work in CHANGELOG.md.
Contributor guidelines: see CONTRIBUTING.md. Security scope: SECURITY.md.

## Layout

- `product.json` – single source of product identity (display name, slug, data dir, bundle id, version).
  `scripts/product.mjs` generates `src/lib/generated/product.ts` and `src-tauri/tauri.product.conf.json`;
  `crates/agent-core/build.rs` exposes the same fields to Rust as `PRODUCT_*` env vars.
- `crates/agent-core` – UI-independent runtime (config, domain, agent loop, providers, storage, secrets, http, mcp,
  `agents` = user-configured agents + schedule math). No Tauri deps. SQLite migrations are append-only (`storage/migrations.rs`).
- `src-tauri` – Tauri host: `AppState`, IPC commands, capabilities, CSP. `commands::launch_run` is the single
  run entry point (interactive chats and `scheduler.rs` background runs).
- `src` – Svelte frontend. `src/lib/generated/bindings` are ts-rs outputs from Rust DTOs (committed; CI checks drift).

## Commands

- Node 24 LTS (`nvm use` reads `.nvmrc`); `npm install` once; `cargo` uses the committed `Cargo.lock`.
- `npm run tauri:dev` – run the desktop app (generates product artifacts first).
- `npm run tauri:build` – release bundle (unsigned unless signing is configured).
- `npm run verify` – svelte-check, vitest, cargo fmt/clippy/test.
- `npm run gen:types` – regenerate TS bindings after changing any `#[derive(TS)]` type, then commit them.
- Adding a provider: new `ProviderKind` + `KindMeta` row + one `ProviderStore::instantiate` arm + an
  adapter in `providers/` built on `providers/common.rs` (`send_checked`, `pump_sse`, `read_json`,
  `map_status`/`map_transport`, `normalize_call_ids`, `PartialCall`). Protocol state the next request
  must replay: emit `ProviderEvent::Opaque` in stream order; it lands as `ContentBlock::ProviderOpaque`
  (≤ `MAX_OPAQUE_BYTES`, family-tagged, IPC-invisible). Adapter streaming tests reuse `tests/common`
  (`serve_once`, `collect`). The settings form is driven by `list_provider_kinds` — no per-kind Svelte.
- `npm run gen:brand` – regenerate horizontal SVGs, the social preview, and desktop icons from
  `docs/assets/logo.svg` and `product.json`; requires Inkscape and Arial (see `docs/assets/README.md`).
  Normal builds use committed assets. `npm run check && npm test && npm run build` verifies frontend/branding changes.
- `cargo test -p agent-core -- --ignored` – live tests: OS keychain round trip; stdio MCP probe
  (`OPENGENT_TEST_MCP_COMMAND="npx -y @modelcontextprotocol/server-everything"`); provider round trip
  (`OPENGENT_TEST_OPENAI_BASE_URL=http://localhost:8787/v1` with `node scripts/dev/mock-openai-server.mjs`).
- `node scripts/dev/mock-openai-server.mjs` – fake OpenAI-compatible server for manual UI testing (`/v1`, models `mock-fast`, `mock-slow`; API key `bad` triggers a 401). Send "use tool <name>" to exercise tool calling.
- `node scripts/dev/mock-gemini-server.mjs [port]` – fake Gemini Interactions API (`http://localhost:8789/v1`, models
  `mock-gemini-fast`, `mock-gemini-thinking`, `mock-gemini-slow`; any key except `bad`, which gets Google's 400
  `API_KEY_INVALID`). "use tool <name>" returns a signed `function_call`; it rejects requests whose replayed thought/call
  signatures were altered or reordered. Live/offline round trip: `OPENGENT_TEST_GEMINI_API_KEY=x
  OPENGENT_TEST_GEMINI_BASE_URL=http://localhost:8789/v1 OPENGENT_TEST_GEMINI_MODEL=mock-gemini-thinking
  cargo test -p agent-core --test provider_store_live -- --ignored gemini` (drop the base URL/model for real Gemini).
- `node scripts/dev/mock-mcp-server.mjs` – fake MCP server (tools: echo, add, get_time, fail, slow).
  Stdio by default (use `node <abs path>` as a stdio command); `--http 8788` serves Streamable HTTP at `/mcp`; `--many 50` adds filler tools for on-demand loading tests.

## Conventions

- Never return secrets to the webview, log them, or store them in SQLite; use `SecretRef` + `SecretStore`.
- New IPC DTOs: derive `Serialize` + `TS` with `#[ts(export)]`; map `u64` fields with `#[ts(type = "number")]`.
- Dependencies: pin exact versions published ≥ 7 days ago; use `cargo update -p <crate> --precise <ver>` to hold back
  transitive bumps; keep features minimal.
- CSS: no `light-dark()`, `:has()`, or other post-Safari-15 features — the macOS 12 WKWebView baseline is Safari 15.6. Theme via `prefers-color-scheme`.
- Streaming to the UI goes through `tauri::ipc::Channel` (ordered); text deltas are coalesced in `RunEmitter`.
- Product branding only via `product.json`; `slug`, `dataDirName`, `identifier` are stable – renaming needs a migration.
- Prefer first-party code; add external dependencies only for key capabilities.
- User-visible changes: update `docs/features.md` (what / how to use / how it works / limits) and `CHANGELOG.md`;
  move shipped roadmap items out of README "Proposed features".
