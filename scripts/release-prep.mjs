// Release prep: bump every version field to X.Y.Z, refresh Cargo.lock, and rotate the
// CHANGELOG's Unreleased section to the new release, then run the version gate.
// Every edit is validated before anything is written, and a failed lockfile refresh or
// gate restores the files, so a failed run leaves the tree as it found it.
// Usage: npm run release:prep -- X.Y.Z
import { readFileSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const read = (file) => readFileSync(resolve(root, file), "utf8");
const write = (file, content) => writeFileSync(resolve(root, file), content);

// The app always reports the plain X.Y.Z it is a candidate for; -alpha/-beta/-rc.N
// identifiers live on the git tag only (see docs/releasing.md).
const version = process.argv[2];
if (process.argv.length !== 3 || !/^\d+\.\d+\.\d+$/.test(version)) {
  console.error("usage: npm run release:prep -- X.Y.Z");
  process.exit(1);
}

const errors = [];
const original = Object.fromEntries(
  ["product.json", "package.json", "package-lock.json", "Cargo.toml", "Cargo.lock", "CHANGELOG.md"].map(
    (file) => [file, read(file)],
  ),
);
const next = {};

const current = JSON.parse(original["product.json"]).version;
const parts = (v) => String(v).split(".").map(Number);
const [a, b] = [parts(version), parts(current)];
const order = a[0] - b[0] || a[1] - b[1] || a[2] - b[2];
if (!(order > 0)) errors.push(`${version} is not newer than the current version ${current}`);

// product.json is the single source of truth the gate checks against. Text edits keep
// the $comment, field order and formatting untouched; the two-space indent pins the
// match to the top-level field.
for (const file of ["product.json", "package.json"]) {
  const field = /^( {2}"version": )"[^"]*"/m;
  if (!field.test(original[file])) errors.push(`${file} has no top-level "version" field`);
  next[file] = original[file].replace(field, `$1"${version}"`);
}

// package-lock.json — the two fields the gate reads, written the way npm formats it.
const lock = JSON.parse(original["package-lock.json"]);
if (!lock.packages?.[""]) errors.push('package-lock.json has no root package entry (packages[""])');
else {
  lock.version = lock.packages[""].version = version;
  next["package-lock.json"] = `${JSON.stringify(lock, null, 2)}\n`;
}

// Cargo.toml — [workspace.package] version only (dependency versions live in
// [workspace.dependencies] below it, so scope the replace to the section).
const workspaceVersion = /^(\[workspace\.package\]\s*$[^[]*?^version\s*=\s*)"[^"]+"/m;
if (!workspaceVersion.test(original["Cargo.toml"])) {
  errors.push("Cargo.toml has no [workspace.package] version");
}
next["Cargo.toml"] = original["Cargo.toml"].replace(workspaceVersion, `$1"${version}"`);

// CHANGELOG.md — rename the pending release and leave a fresh empty Unreleased on top.
const changelog = original["CHANGELOG.md"];
const heading = /^## Unreleased[ \t]*$/m.exec(changelog);
if (new RegExp(`^## ${version.replaceAll(".", "\\.")}(\\s|$)`, "m").test(changelog)) {
  errors.push(`CHANGELOG.md already has a ${version} section`);
}
if (!heading) errors.push("CHANGELOG.md has no ## Unreleased heading");
else {
  const start = heading.index + heading[0].length;
  const rest = changelog.slice(start);
  const end = rest.search(/^## /m);
  if (!(end < 0 ? rest : rest.slice(0, end)).trim()) {
    errors.push("CHANGELOG.md has nothing under ## Unreleased to release");
  }
  const now = new Date();
  const pad = (n) => String(n).padStart(2, "0");
  const today = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
  next["CHANGELOG.md"] = `${changelog.slice(0, start)}\n\n## ${version} — ${today}${rest}`;
}

if (errors.length) {
  for (const error of errors) console.error(`release prep: ${error}`);
  console.error("release prep: nothing was changed");
  process.exit(1);
}

for (const [file, content] of Object.entries(next)) write(file, content);
try {
  // Cargo.lock — re-resolve the workspace members only; no dependency is bumped and
  // nothing is built.
  execFileSync("cargo", ["update", "--workspace", "--quiet"], { cwd: root, stdio: "inherit" });
  execFileSync(process.execPath, ["scripts/check-release-version.mjs", `v${version}`], {
    cwd: root,
    stdio: "inherit",
  });
} catch (error) {
  for (const [file, content] of Object.entries(original)) write(file, content);
  console.error(`release prep: ${error.message}`);
  console.error("release prep: restored every file; nothing was changed");
  process.exit(1);
}
console.log(
  `release prep complete: ${current} -> ${version} — open a chore/release-${version} PR, then tag v${version} on main`,
);
