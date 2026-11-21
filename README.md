<p align="center">
  <img src="docs/assets/logo.svg" alt="OpenGent logo" width="120" />
</p>

<h1 align="center">OpenGent</h1>

<p align="center">
  <strong>Any model. Only the tools you allow. Every step in plain sight.</strong>
</p>

<p align="center">
  <a href="https://github.com/samkerrcode/opengent/releases/latest"><strong>Download for macOS</strong></a>
  &nbsp;·&nbsp;
  <a href="https://github.com/samkerrcode/opengent/releases/latest"><strong>Download for Windows</strong></a>
</p>

<p align="center">
  <a href="https://github.com/samkerrcode/opengent/releases/latest"><img src="https://img.shields.io/github/v/release/samkerrcode/opengent?label=release" alt="Latest release" /></a>
  <a href="https://github.com/samkerrcode/opengent/releases/latest"><img src="https://img.shields.io/github/downloads/samkerrcode/opengent/total?label=downloads" alt="Total downloads" /></a>
  <a href="https://github.com/samkerrcode/opengent/actions/workflows/ci.yml"><img src="https://github.com/samkerrcode/opengent/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue" alt="License: MIT OR Apache-2.0" /></a>
  <img src="https://img.shields.io/badge/platform-macOS%2012%2B%20%C2%B7%20Windows%2010%2B-lightgrey" alt="Platform: macOS 12+ · Windows 10+" />
</p>

OpenGent is an open-source, native desktop AI agent that connects any model to the
[MCP](https://modelcontextprotocol.io) tools you trust — and works for you, not for a model
vendor. Plug in a local model running on your laptop or a frontier model behind your company's
gateway, and stay in control of every action it takes. The model proposes; you decide; Rust
enforces.

It's built to be equally at home on a personal machine and in a business setting:

- **For individuals** — run fully local with Ollama or LM Studio: no account, no cloud, no
  telemetry. Chats stay on your disk; API keys stay in your OS keychain.
- **For teams and businesses** — point it at an approved OpenAI-compatible gateway, gate every
  tool with *Allow / Ask / Disable* permissions enforced below the UI, and build reusable
  **agents** that run on schedules. The source is open, auditable, and permissively licensed.

https://github.com/user-attachments/assets/e552658e-231b-44a2-97c2-7181deb878e3

## Highlights

- **Model-agnostic.** Any OpenAI-compatible endpoint — Ollama, LM Studio, vLLM, llama.cpp,
  OpenRouter, or a self-hosted gateway — plus native OpenAI, Google Gemini and Anthropic,
  with a built-in simulated provider for trying the app without an account. Native
  xAI (next), Bedrock and Hugging Face adapters are on the [roadmap](#native-providers).
- **Real tools, real permissions.** Connect [MCP](https://modelcontextprotocol.io) servers over
  stdio or Streamable HTTP. Each tool is *Allow*, *Ask*, or *Disable*; permissions are
  enforced in Rust at dispatch, and an *Always allow* grant reverts to *Ask* if the tool's
  definition changes.
- **Transparent execution.** Every turn shows its real phases — generating, awaiting approval,
  calling a tool — with inspectable arguments and results. Nothing is simulated or hidden.
- **Agents & schedules.** Package a role, default + fallback model, instructions and a tool
  selection into an agent; run it on demand or once / every N minutes / on chosen weekdays,
  with catch-up handling for runs missed while the app was closed.
- **Polished chat.** Token-by-token streaming, sanitized Markdown with code highlighting and
  KaTeX math, file, image and PDF attachments, search, and recovery of interrupted runs.
- **Small, fast, local.** A ~6 MiB app on the OS webview. Data lives in `~/.opengent`, secrets in
  the macOS Keychain / Windows Credential Manager, and there is no telemetry.

See **[docs/features.md](docs/features.md)** for the full feature guide.

## Why OpenGent

| | OpenGent | A typical vendor desktop app |
|---|---|---|
| **Models** | Any OpenAI-compatible server, local or hosted, plus native OpenAI, Gemini and Anthropic | The vendor's own models |
| **Account** | None | Vendor sign-in |
| **Tool permissions** | Per tool (*Allow* / *Ask* / *Disable*), enforced in Rust; a grant reverts to *Ask* when the tool's definition changes | Varies by vendor |
| **Scheduled agents** | Built in, run on your machine | Varies by vendor |
| **Your data** | Chats on your disk, keys in the OS keychain, no telemetry | Usually stored in the vendor's cloud |
| **Source** | Open, MIT OR Apache-2.0 | Usually closed |
| **Footprint** | ~6 MiB on the OS webview | Often a bundled browser runtime |

## Install

**[Download the latest release](https://github.com/samkerrcode/opengent/releases/latest)** — no
account, no build tools.

| OS | File | Then |
|---|---|---|
| macOS 12+ (Apple Silicon & Intel) | `OpenGent_<version>_universal.dmg` | Drag to Applications, then run the command below once |
| Windows 10+ (x64) | `OpenGent_<version>_x64-setup.exe` | Run it; on SmartScreen choose **More info → Run anyway** |
| Windows, managed deployment | `OpenGent_<version>_x64_en-US.msi` | Intune / Group Policy / SCCM |

Installers are unsigned for now, so macOS needs the quarantine flag cleared once:

```sh
xattr -dr com.apple.quarantine /Applications/OpenGent.app
```

Checksums are in `SHA256SUMS.txt`; see
[installing a release](docs/releasing.md#installing-a-release).

### First run

1. **Try it instantly.** The built-in *Simulated* model works with no setup.
2. **Add a model.** Open **Providers** (⌘, on macOS / Ctrl+, on Windows) and pick a tile:
   - **Local and free:** *OpenAI-compatible* → the **Ollama** or **LM Studio** preset (presets
     for vLLM / llama.cpp and OpenRouter are included too).
   - **Hosted:** *OpenAI*, *Anthropic* or *Google Gemini* → paste an API key. Keys go straight
     to the OS keychain.
3. **Add tools (optional).** Open **Connections** and add MCP servers. You can paste a standard
   `{"mcpServers": {…}}` JSON config. Each tool is *Allow*, *Ask* or *Disable*.

To update, download and install the newer release; your data in `~/.opengent` is kept.

## Status

OpenGent is **pre-1.0** and under active development. Installers for macOS and Windows are on
the [Releases](https://github.com/samkerrcode/opengent/releases) page, and both platforms are
verified by hand. Phases 0–4 of the [roadmap](#roadmap) are complete and Phase 5 (native
providers) is in progress. Installers are unsigned until signing and notarization land in Phase 7.
Versioning and the release process are described in [docs/releasing.md](docs/releasing.md).

## Roadmap

Each phase has an acceptance gate that must pass before the next begins. Delivered work is
detailed in [CHANGELOG.md](CHANGELOG.md); shipped features are documented in
[docs/features.md](docs/features.md).

| Phase | Scope | Status |
|---|---|---|
| **0 – Foundation** | Cross-platform Tauri/Svelte build, product identity, typed IPC, run coordinator with real streaming/cancellation, native keychain + rmcp feasibility, CI | ✅ Done (macOS and Windows verified) |
| **1 – Workspace & persistence** | SQLite (WAL) chat history, search, attachments as immutable content-hashed snapshots, restart recovery, command palette, inspector panel | ✅ Done (command palette and inspector deferred) |
| **2 – Custom/local providers** | OpenAI-compatible Chat Completions streaming, provider/model settings, capability flags, incremental sanitized Markdown, context limits. *First usable chat.* | ✅ Done |
| **3 – MCP agent** | stdio + Streamable HTTP servers, tool discovery, Allow / Ask / Disable permissions, approval UI, full loop: model → approval → tool → result → model. *Agent alpha.* | ✅ Done |
| **4 – OAuth** | Browser sign-in (authorization code + PKCE, U2M) and client credentials (M2M) for MCP servers; token lifecycle in the keychain | ✅ Done |
| **5 – Native providers** | One adapter per vendor protocol, each shipped independently — see [Native providers](#native-providers) | In progress (Gemini, OpenAI, Anthropic done; xAI next) |
| **6 – Provider context & caching** | Provider-aware prompt caching (OpenAI `prompt_cache_key`, Anthropic `cache_control`), cross-provider context projection, summarization checkpoints | |
| **7 – Release hardening** | Performance profiling, accessibility, export/import, signing & notarization, installers | |
| **8 – Extensions** | Sub-agents, A2A, skills, hooks, model router — through the existing `ModelProvider` / tool / permission seams | |

### Native providers

Phase 5 is delivered one provider at a time. Every adapter plugs into the same `ModelProvider`
seam, replays its own continuation state through `ProviderOpaque` blocks, and gets its settings
form from Rust (`list_provider_kinds`) — adding one needs no UI changes.

| Provider | Protocol | Notes | Status |
|---|---|---|---|
| **Google Gemini** | Interactions API (`store: false`) | AI Studio key; thought signatures replayed verbatim; stream resume | ✅ Done |
| **OpenAI** | Responses API (`store: false`) | API key; encrypted reasoning items replayed; reasoning summaries | ✅ Done |
| **Anthropic** | Messages API | Adaptive thinking, signed thinking-block replay | ✅ Done |
| **xAI** | xAI API (stateless) | API key; Grok models; reasoning replayed across tool calls | Next |
| **AWS Bedrock** | ConverseStream | Feature-gated AWS SDK; IAM / SSO profiles | |
| **Hugging Face** | Inference Providers (OpenAI-compatible) | Presets on the OpenAI-compatible type | |

### Proposed features

These are approved for the roadmap and can be built in any order, alongside or between the
numbered phases:

| Feature | Summary |
|---|---|
| **Encrypted chats** | A lock toggle in the composer prompts for a password. Every message in that chat is encrypted at rest with a strong, ideally quantum-resistant algorithm, and you have to unlock the chat again after navigating away or relaunching. |
| **Security policies** | A policies menu with configurable data masking. Sensitive values are detected and replaced with placeholder tokens before a prompt leaves the app, the model is told to keep the tokens, and the original values are restored in the response. Each classification (phone numbers, SSN/TIN, email addresses) can be toggled separately. |
| **Native tools & sandboxed runtime** | Built-in tools you can configure from the Tools menu: file creation and editing; a basic POSIX command set (curl, grep, glob, cat, …) running in an isolated per-chat filesystem (a headless container or an embedded runtime, whichever balances security and performance best); and web search over free, configurable sources and APIs. |
| **Image generation** | Text-to-image, with automatic detection of provider models that support it. |
| **CLI** | An `opengent` binary (name derived from `product.json`) built on `agent-core` for headless use: list configured models, run a prompt, run a prompt with an agent, and schedule a prompt. |

Have an idea? Open a [feature or enhancement request](https://github.com/samkerrcode/opengent/issues/new/choose).
See [CONTRIBUTING.md](CONTRIBUTING.md) for how proposals are approved.

### Known limitations

- There's no packaged end-to-end UI test; the webview path is verified manually with
  `tauri:dev`.
- The command palette and inspector panel from Phase 1 are deferred (sidebar search covers the
  main need for now).
- Implemented protocols: OpenAI-compatible, OpenAI (Responses API; no built-in tools such as
  web search or code interpreter yet), Google Gemini (AI Studio API key; no Vertex AI, Google
  built-in tools or images inside tool results yet) and Anthropic (API key; no Bedrock/Vertex,
  Anthropic built-in tools or prompt caching yet). The remaining native adapters — xAI
  first — arrive during [Phase 5](#native-providers).
- Scheduled runs only fire while the app is open.
- PDFs are rendered inside the app's process by the OS renderer, not in a sandboxed helper,
  and its time budget is only checked between pages (see [SECURITY.md](SECURITY.md)).
- Attachments are never deleted from disk, even when their chat is.
- Installers are unsigned, so macOS and Windows warn on first launch (see [Install](#install)).

## Security

OpenGent is built to high security standards, and its security and performance are reviewed
regularly. In short:

- Strict CSP with frozen prototypes. The webview is granted only `core:default`, window-title,
  dialog-open, and opener permissions — no shell, filesystem, or HTTP access.
- Provider and MCP traffic originates in Rust. HTTPS is required except for loopback endpoints,
  and remote MCP connections never follow redirects.
- MCP tool permissions reduce risk but are **not** an OS sandbox. A local stdio server can do
  anything your user account can, so only add servers you trust.
- Local chat data isn't encrypted at rest (it relies on OS account isolation and disk
  encryption). Per-chat encryption is on the [roadmap](#proposed-features).

To report a vulnerability, see [SECURITY.md](SECURITY.md). Please don't open a public issue.

## Build from source

For contributors and anyone who wants to run from `main`. To just use the app, see
[Install](#install).

**Prerequisites:** Rust (via [`rustup`](https://rustup.rs), which installs the version pinned
in `rust-toolchain.toml`), Node 24 LTS (`nvm use` reads `.nvmrc`), and the [Tauri system dependencies](https://v2.tauri.app/start/prerequisites/)
for your OS — Xcode Command Line Tools on macOS; MSVC Build Tools + WebView2 on Windows.

```sh
git clone https://github.com/samkerrcode/opengent.git
cd opengent
npm install
npm run tauri:dev      # run the app with hot reload
npm run verify         # svelte-check, vitest, cargo fmt/clippy/test (PR gate)
```

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a PR.

## Development

```sh
npm run verify                                     # svelte-check, vitest, cargo fmt/clippy/test (PR gate)
npm run gen:types                                  # regenerate TS bindings after changing #[derive(TS)] types
cargo test -p agent-core -- --ignored              # live tests (keychain; MCP needs the env var below)
OPENGENT_TEST_MCP_COMMAND="npx -y @modelcontextprotocol/server-everything" \
  cargo test -p agent-core live_stdio_probe -- --ignored
node scripts/dev/mock-openai-server.mjs              # fake OpenAI server at http://localhost:8787/v1
node scripts/dev/mock-mcp-server.mjs                 # stdio MCP server for manual testing
node scripts/dev/mock-mcp-server.mjs --http 8788     # same server over Streamable HTTP at /mcp
node scripts/dev/mock-mcp-server.mjs --many 50       # pad the catalog to test on-demand loading
OPENGENT_DATA_DIR=/tmp/og ./target/release/opengent-desktop   # run against an alternate data root
OPENGENT_LOG=debug npm run tauri:dev                          # tracing filter
```

Environment variable names derive from the product slug, so they follow a rename of
`product.json`.

For a realistic local test server, run the mock OpenAI server and point a provider at
`http://localhost:8787/v1`.

Release installers are built by `.github/workflows/release.yml`, so a local
`npm run tauri:build` (bundle in `target/release/bundle/`) is rarely needed. If you do run it,
the DMG step can fail when a previous build's image is still mounted (`hdiutil detach`); the
`.app` itself is unaffected.

## Architecture

```
Svelte UI  ──typed commands / ordered Channel──▶  Tauri host (src-tauri)
                                                      │
                                                      ▼
                                             agent-core (Rust, no UI deps)
                                             ├── config      product identity, data root
                                             ├── domain      ids, messages, run events
                                             ├── agent       run coordinator, context, tool loop
                                             ├── agents      user agents, schedule math
                                             ├── providers   ModelProvider trait + adapters
                                             ├── storage     SQLite (WAL), migrations, attachments
                                             ├── secrets     SecretStore (Keychain / Cred. Manager)
                                             ├── http        shared rustls client
                                             └── mcp         rmcp client (stdio, Streamable HTTP)
```

Design rules (contributor conventions live in [AGENTS.md](AGENTS.md)):

- **One identity source.** `product.json` defines the display name, slug, data directory,
  bundle identifier, and version. A build step propagates it to Rust (`build.rs`), the frontend,
  and the Tauri bundle; the name is not hardcoded anywhere else.
- **Rust owns the truth.** The webview holds view state only. It has no shell, filesystem, or
  HTTP capability; everything goes through validated commands.
- **Ordered streaming with backpressure.** Run events carry a schema version and a monotonic
  sequence number, are coalesced (~30 ms / 2 KiB) before crossing IPC, and are delivered over
  Tauri Channels. A dropped receiver cancels the run.
- **Secrets never leave Rust.** The core hands out opaque `SecretRef`s; values live in the OS
  credential store and are never written to SQLite, logs, or IPC responses.
- **Provider-native state is preserved.** Messages are typed content blocks; opaque provider
  continuation data (reasoning items, thought signatures) is kept verbatim and never forwarded
  to a different provider.
- **First-party by default.** External dependencies are reserved for key capabilities, pinned
  to exact versions published ≥ 7 days ago, with minimal feature sets.

## Repository layout

| Path | Purpose |
|---|---|
| `product.json` | Product identity (single source of truth) |
| `scripts/product.mjs` | Generates `src/lib/generated/product.ts` and the Tauri config overlay |
| `crates/agent-core/` | Runtime library; fully testable without a window |
| `src-tauri/` | Tauri host: `AppState`, IPC commands, scheduler, capabilities, CSP, icons |
| `src/` | Svelte app: `features/{chat,history,providers,mcp,agents}`, `lib/ipc`, `lib/state` |
| `src/lib/generated/bindings/` | TypeScript types generated from Rust DTOs by ts-rs (committed; CI checks drift) |
| `scripts/dev/` | Mock OpenAI (Chat Completions + Responses), Gemini, Anthropic and MCP servers for local testing |
| `docs/` | Feature guide, release & versioning policy, and branding assets |
| `.github/workflows/ci.yml` | macOS + Windows checks and tests on every PR; unsigned release-profile build on pushes to `main` |
| `.github/workflows/release.yml` | Tag-driven installers (universal macOS `.dmg`, Windows `.exe` + `.msi`) as a draft GitHub release |

## Contributing

OpenGent is **human-designed and AI-led**. The architecture and features are directed by the
maintainer, and most code is written and reviewed with frontier AI models, which are also
recommended for contributors. Contributions are welcome on **approved issues only**. Read
[CONTRIBUTING.md](CONTRIBUTING.md) before opening a PR. Everyone is expected to follow the
[Code of Conduct](CODE_OF_CONDUCT.md).

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.

OpenGent was created by Sam Kerr ([@samkerrcode](https://github.com/samkerrcode)), co-authored
with AI. If you use this code in a paid product, attribution to the original creator is
appreciated, though not required by the licenses.
