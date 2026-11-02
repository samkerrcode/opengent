# Contributing to OpenGent

Thanks for your interest! Please read this first — OpenGent accepts contributions in a
deliberately narrow lane.

## How the project is run

OpenGent is **human-designed and AI-led**. The maintainer sets the architecture, priorities, and
feature design through prompt guidance, and most code is written and reviewed with frontier AI
models. Frontier models are also the recommended tool for contributors, whether for
implementation, code review, improvements, or exploring new feature designs.

Two principles shape every review:

- **Security first.** The app holds credentials and runs tools on users' machines. Changes are
  judged on whether they keep the security model intact before anything else.
- **First-party by default.** OpenGent implements as much as practical in its own code and uses
  external dependencies only for key capabilities (see [Dependency policy](#dependency-policy)).

## What's accepted

| Contribution | Process |
|---|---|
| Bug reports | Always welcome. Use the bug report template. |
| Feature ideas | Open a feature request. The maintainer decides whether it fits the roadmap. |
| Pull requests | **Only** for issues labeled `approved`. That covers both triaged bugs and accepted feature requests. |
| Security issues | Never in public. Follow [SECURITY.md](SECURITY.md). |

PRs that aren't linked to an `approved` issue, such as drive-by features, unrequested
refactors, or dependency swaps, will usually be closed without review. If you want to work on
something, comment on the approved issue so effort isn't duplicated.

## Development setup

Prerequisites and commands are in the [README](README.md#quick-start). In short: Rust stable,
Node 24 LTS (`nvm use`), the Tauri system dependencies, then `npm install` and
`npm run tauri:dev`. Mock OpenAI and MCP servers for local testing live in `scripts/dev/` (see
[AGENTS.md](AGENTS.md#commands)).

## Branching

- Branch off `main` and open a PR back to `main`. There is no long-lived `dev` branch.
- Name branches `<type>/<slug>` — e.g. `feat/mcp-oauth`, `fix/catchup-panic`,
  `chore/bump-tauri`. `<type>` must be one of: `feat`, `fix`, `perf`, `refactor`, `test`,
  `docs`, `ci`, `chore`. Write the PR title the same way (`fix: prevent double dispatch`) —
  it becomes the squash-merge commit message.
- One branch = one PR = one issue. Open a draft PR for multi-day work so CI runs on it.
- Branches are deleted on merge. Never reuse a merged branch — cut a fresh one off `main`.
- Security fixes use `fix/` with a neutral slug; don't name branches or PRs in ways that
  disclose a vulnerability before it's patched.

## Pull request checklist

- `npm run verify` passes. This is the PR gate; it runs svelte-check, vitest,
  `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test`, and CI runs the same on
  macOS and Windows.
- If you changed any `#[derive(TS)]` type, run `npm run gen:types` and commit the regenerated
  bindings. CI checks for drift.
- If you added a schema change, add a new SQLite migration; migrations are append-only, so never
  edit an existing one.
- If the change is user-visible, update [docs/features.md](docs/features.md) (keep its
  *what / how to use / how it works / limits* format) and add a line to
  [CHANGELOG.md](CHANGELOG.md).
- Keep PRs focused: one issue per PR.

## Conventions

The full list is in [AGENTS.md](AGENTS.md). These are the ones most often missed:

- Secrets never reach the webview, logs, or SQLite. Use `SecretRef` + `SecretStore`.
- New IPC DTOs derive `Serialize` + `TS` with `#[ts(export)]`.
- CSS must stay within the Safari 15.6 baseline (the macOS 12 WKWebView).
- Product names come from `product.json` only. Never hardcode "OpenGent".

## Dependency policy

Before you add a dependency, be ready to explain why a first-party implementation isn't
practical. Every dependency is pinned to an exact version published at least 7 days ago, with a
minimal feature set. A PR that pulls in a crate or package for something easy to implement
directly will be asked to remove it.

## AI-assisted contributions

AI-assisted PRs are welcome and encouraged, but you are responsible for what you submit:

- Read and understand the whole diff, and run `npm run verify` yourself.
- Say in the PR description which model(s) and tools you used.
- Never put secrets, credentials, real user data, or private code in prompts or commits.

## Security-sensitive areas

Changes to secret handling, MCP policy enforcement, process spawning, the CSP or capabilities,
or network/TLS handling get extra review. The maintainer may re-implement them in their own
workflow.

## Code of Conduct

Everyone participating is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Licensing of contributions

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
OpenGent by you, as defined in the Apache-2.0 license, shall be dual-licensed as
MIT OR Apache-2.0, without any additional terms or conditions.
