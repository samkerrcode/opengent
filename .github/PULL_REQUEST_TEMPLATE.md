<!--
PRs are accepted only for issues labeled `approved` (triaged bugs or accepted
feature requests). See CONTRIBUTING.md. PRs without one will usually be closed.
Security fixes: do not open a PR — report privately per SECURITY.md.
-->

Fixes #

## What & why

<!-- One short paragraph. -->

## Verification

- [ ] `npm run verify` passes (svelte-check, vitest, fmt, clippy, cargo test)
- [ ] Bindings regenerated with `npm run gen:types` and committed — or no `#[derive(TS)]` type changed
- [ ] No secrets, keys, or real user data added; nothing logged that could contain them
- [ ] No new dependencies — or each is justified below and pinned to an exact version ≥ 7 days old
- [ ] `docs/features.md` and `CHANGELOG.md` updated — or the change isn't user-visible
- [ ] Tested on: <!-- macOS / Windows -->

## AI assistance

<!-- Which model(s)/tools were used, if any. -->

## Notes for reviewers
