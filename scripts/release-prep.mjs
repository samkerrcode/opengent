// Release prep: bump every version field to X.Y.Z, refresh Cargo.lock, and rotate the
// CHANGELOG's Unreleased section to the new release, then run the version gate.
// Usage: npm run release:prep -- X.Y.Z
import { readFileSync, writeFileSync } from "node:fs";
import { execSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const read = (file) => readFileSync(resolve(root, file), "utf8");
const write = (file, content) => writeFileSync(resolve(root, file), content);
const run = (command) => execSync(command, { cwd: root, stdio: ["ignore", "ignore", "inherit"] });

// The app always reports the plain X.Y.Z it is a candidate for; -alpha/-beta/-rc.N
// identifiers live on the git tag only (see docs/releasing.md).
const version = process.argv[2];
if (!/^\d+\.\d+\.\d+$/.test(version ?? "")) {
  console.error("usage: npm run release:prep -- X.Y.Z");
  process.exit(1);
}
const tag = `v${version}`;

const fail = (message) => {
  console.error(`::error::${message}`);
  process.exit(1);
};

// product.json — the single source of truth the gate checks against. Text edit keeps
// the $comment and field order untouched.
const product = read("product.json");
const bumped = product.replace(/"version": "\d+\.\d+\.\d+"/, `"version": "${version}"`);
if (bumped === product) fail('product.json has no "version" field to bump');
write("product.json", bumped);

// Cargo.toml — [workspace.package] version only (dependency versions live in
// [workspace.dependencies] below it, so scope the replace to the section).
const cargoToml = read("Cargo.toml");
const bumpedToml = cargoToml.replace(
  /(\[workspace\.package\][^[]*?^version\s*=\s*)"[^"]+"/m,
  `$1"${version}"`,
);
if (bumpedToml === cargoToml) fail("Cargo.toml has no [workspace.package] version to bump");
write("Cargo.toml", bumpedToml);

// package.json + package-lock.json — npm's own bumper updates both fields the gate reads.
run(`npm version ${version} --no-git-tag-version`);

// Cargo.lock — a metadata resolve rewrites the workspace members' version entries
// without a build.
run("cargo metadata --format-version 1 --quiet");

// CHANGELOG.md — rename the pending release and leave a fresh empty Unreleased on top.
const now = new Date();
const today = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(
  now.getDate(),
).padStart(2, "0")}`;
const changelog = read("CHANGELOG.md");
if (new RegExp(`^## ${version} `, "m").test(changelog)) {
  fail(`CHANGELOG.md already has a ${version} section`);
}
if (!/^## Unreleased[ \t]*$/m.test(changelog)) fail("CHANGELOG.md has no ## Unreleased heading");
write(
  "CHANGELOG.md",
  changelog.replace(/^## Unreleased[ \t]*$/m, `## Unreleased\n\n## ${version} — ${today}`),
);

execSync(`node scripts/check-release-version.mjs ${tag}`, { cwd: root, stdio: "inherit" });
console.log(`release prep complete: ${version} — open a chore/release-${version} PR, then tag ${tag} on main`);
