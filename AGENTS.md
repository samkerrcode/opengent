# OpenGent – contributor notes

Model-agnostic desktop agent. Tauri 2 host + Svelte 5 UI; all agent logic in Rust (`crates/agent-core`).
Design/roadmap: see the phased plan (Phase 0 = foundation, currently implemented).

## Layout

- `product.json` – single source of product identity (display name, slug, data dir, bundle id, version).
  `scripts/product.mjs` generates `src/lib/generated/product.ts` and `src-tauri/tauri.product.conf.json`;
  `crates/agent-core/build.rs` exposes the same fields to Rust as `PRODUCT_*` env vars.
- `crates/agent-core` – UI-independent runtime (config, domain, agent loop, providers, secrets, mcp). No Tauri deps.
- `src-tauri` – Tauri host: `AppState`, IPC commands, capabilities, CSP.
- `src` – Svelte frontend. `src/lib/generated/bindings` are ts-rs outputs from Rust DTOs (committed; CI checks drift).

## Commands

- `npm install` once; `cargo` uses the committed `Cargo.lock`.
- `npm run tauri:dev` – run the desktop app (generates product artifacts first).
- `npm run tauri:build` – release bundle (unsigned unless signing is configured).
- `npm run verify` – svelte-check, vitest, cargo fmt/clippy/test.
- `npm run gen:types` – regenerate TS bindings after changing any `#[derive(TS)]` type, then commit them.
- `cargo test -p agent-core -- --ignored` – live tests: OS keychain round trip; stdio MCP probe
  (`OPENGENT_TEST_MCP_COMMAND="npx -y @modelcontextprotocol/server-everything"`).

## Conventions

- Never return secrets to the webview, log them, or store them in SQLite; use `SecretRef` + `SecretStore`.
- New IPC DTOs: derive `Serialize` + `TS` with `#[ts(export)]`; map `u64` fields with `#[ts(type = "number")]`.
- Dependencies: pin exact versions published ≥ 7 days ago; use `cargo update -p <crate> --precise <ver>` to hold back
  transitive bumps; keep features minimal.
- Streaming to the UI goes through `tauri::ipc::Channel` (ordered); text deltas are coalesced in `RunEmitter`.
- Product branding only via `product.json`; `slug`, `dataDirName`, `identifier` are stable – renaming needs a migration.
