# Releases

How OpenGent is versioned and released, how to install a release, and — for maintainers — how
to cut one.

- [Where releases live](#where-releases-live)
- [Installing a release](#installing-a-release)
- [Versioning](#versioning)
- [Cutting a release (maintainers)](#cutting-a-release-maintainers)

## Where releases live

Releases are published on the repository's
[GitHub Releases](https://github.com/samkerrcode/opengent/releases) page. Each one is built by
the [`Release` workflow](../.github/workflows/release.yml) from a `v*` tag on `main` and carries:

| Asset | Platform | Notes |
|---|---|---|
| `*_universal.dmg` | macOS 12+ | One image for Apple Silicon and Intel |
| `*-setup.exe` | Windows 10+ (x64) | NSIS installer; the usual choice for a personal machine |
| `*.msi` | Windows 10+ (x64) | For managed deployment (Intune, Group Policy, SCCM) |
| `SHA256SUMS.txt` | — | SHA-256 of every asset above |

There are two kinds of release:

- **Stable** (`v0.2.0`) — shown as *Latest*. This is what most people should install.
- **Pre-release** (`v0.2.0-rc.1`, `-beta.1`, `-alpha.1`) — marked *Pre-release* on GitHub and
  never shown as *Latest*. These exist to test a release before it becomes stable; expect rough
  edges and back up `~/.opengent` first.

There is no auto-updater yet: to update, download and install the newer release. Your data in
`~/.opengent` and your keys in the OS credential store are kept.

## Installing a release

Installers are **unsigned** until signing and notarization land (roadmap Phase 7), so both
operating systems warn on first launch. Verify the download first, then follow the step for
your OS.

**Verify the download** against `SHA256SUMS.txt` from the same release:

```sh
shasum -a 256 -c SHA256SUMS.txt --ignore-missing        # macOS
```

```powershell
Get-FileHash .\OpenGent_0.1.0_x64-setup.exe -Algorithm SHA256   # Windows; compare with SHA256SUMS.txt
```

**macOS.** Open the `.dmg` and drag OpenGent to Applications. Because the app isn't notarized,
macOS refuses to open it ("is damaged" or "cannot be opened because the developer cannot be
verified"). Clear the quarantine flag once, then launch normally:

```sh
xattr -dr com.apple.quarantine /Applications/OpenGent.app
```

**Windows.** Run the installer. On the SmartScreen prompt choose **More info → Run anyway**.
The installer downloads the WebView2 runtime if the machine doesn't have it.

## Versioning

OpenGent follows [Semantic Versioning](https://semver.org). Tags are `v<version>`.

**What the version number protects.** For a desktop app the "public API" is what users and
integrators depend on:

- the `~/.opengent` data directory and its SQLite schema (migrations are append-only, so a newer
  version always opens older data);
- the stable identities in `product.json` (`slug`, `dataDirName`, bundle `identifier`);
- the import formats, such as the `mcpServers` JSON accepted by Connections;
- once they ship, the CLI's flags and the extension (skills / hooks) interfaces.

**While the project is 0.x** (now):

| Bump | When |
|---|---|
| Minor — `0.1.0` → `0.2.0` | New features, or any breaking change. Breaking changes are called out in the [changelog](../CHANGELOG.md). |
| Patch — `0.1.0` → `0.1.1` | Bug and security fixes only. |

Versions are not tied to roadmap phase numbers. Phases and proposed features ship in whatever
order they're ready, and a minor release contains whatever has landed on `main`.

**1.0.0** is the stability commitment, not a feature count: it ships when the items above are
ones we're prepared to keep compatible, and the installers are signed and notarized (Phase 7).
From 1.0 on, new capabilities are minor releases, fixes are patches, and a major version is
reserved for breaking one of the commitments above.

**Pre-release identifiers** are `-alpha.N`, `-beta.N` and `-rc.N` only (they sort in that order
under SemVer). They exist on the *tag*; the app itself always reports the plain `X.Y.Z` it is a
candidate for, because the Windows MSI format only accepts numeric versions. So `v0.2.0-rc.1`
installs as "0.2.0" — use the release page, not the About text, to tell candidates apart.

**Published tags are immutable.** A released version is never rebuilt, moved or re-uploaded.
If a release is bad, the fix ships as the next patch version.

## Cutting a release (maintainers)

The workflow has two modes:

| Trigger | What it does |
|---|---|
| **Run workflow** (manual, any branch) | *Dry run.* Checks versions, builds both platforms, uploads the installers as workflow artifacts. Never creates a release. |
| Push a `v*` tag | Same, plus creates a **draft** release with the installers, `SHA256SUMS.txt` and generated notes. Nothing is public until you click *Publish*. |

The version gate (`node scripts/check-release-version.mjs [tag]`, runnable locally) fails the
run unless `product.json`, `package.json`, `package-lock.json` and the workspace `Cargo.toml`
agree, and the tag is `v<that version>` with an optional `-alpha.N` / `-beta.N` / `-rc.N`.

### 1. Prepare `main`

In a `chore/release-x.y.z` PR:

1. Bump the version in all places:
   ```sh
   # edit "version" in product.json and [workspace.package] in Cargo.toml, then:
   npm version X.Y.Z --no-git-tag-version   # package.json + package-lock.json
   cargo check                              # refreshes Cargo.lock
   node scripts/check-release-version.mjs vX.Y.Z
   ```
2. In `CHANGELOG.md`, rename the *Unreleased* heading to `## X.Y.Z — YYYY-MM-DD` and start a
   new empty *Unreleased* section above it.
3. Merge once CI is green.

### 2. Dry run (optional, recommended after workflow or bundler changes)

**Actions → Release → Run workflow**, pick the branch. When it finishes, download the
`bundles-*` artifacts from the run page and install them.

```sh
gh workflow run release.yml --ref main
gh run watch
gh run download --dir /tmp/opengent-dry-run
```

A manual run only appears once `release.yml` exists on the default branch.

### 3. Release candidate

```sh
git switch main && git pull
git tag -a vX.Y.Z-rc.1 -m "vX.Y.Z-rc.1"
git push origin vX.Y.Z-rc.1
```

The run ends with a draft pre-release. Open it from the Releases page (drafts are visible to
maintainers only), download the assets and check:

- [ ] macOS: installs from the `.dmg` and launches on Apple Silicon and, if available, Intel
- [ ] Windows: both the `.exe` and the `.msi` install, launch and uninstall
- [ ] the app reports the expected version; existing `~/.opengent` data opens
- [ ] `SHA256SUMS.txt` matches the downloaded files
- [ ] the generated notes read correctly

If something is wrong, fix it on `main` and tag `-rc.2`. Either publish the candidate (useful
when you want outside testers) or discard it — a candidate that was never published can be
removed:

```sh
gh release delete vX.Y.Z-rc.1 --cleanup-tag --yes
```

### 4. Final release

```sh
git tag -a vX.Y.Z -m "vX.Y.Z"      # same commit the last candidate passed on
git push origin vX.Y.Z
```

Review the draft, replace or extend the generated notes with the changelog section, keep the
unsigned-installer notice at the top, then **Publish release**. Stable tags become *Latest*
automatically.

### If a run fails

- **Version gate failed** — the tag doesn't match the committed versions. Delete the tag
  (`git push origin :vX.Y.Z-rc.1`; fine because nothing was published), fix, re-tag.
- **A build failed** — use *Re-run failed jobs*. If it needs a code change, fix `main` and use
  the next `-rc.N`.
- **Draft step failed because the release already exists** — delete the draft
  (`gh release delete <tag> --yes`, keeping the tag) and re-run that job.
- **A published release is broken** — don't touch the tag. Ship `X.Y.(Z+1)`; if needed, edit
  the bad release's notes to point at the fix.
