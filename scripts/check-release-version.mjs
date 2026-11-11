// Release gate: every version field must equal product.json's, and a release tag (if given)
// must be v<that version> with an optional -alpha.N / -beta.N / -rc.N suffix.
// Usage: node scripts/check-release-version.mjs [tag]
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const read = (file) => readFileSync(resolve(root, file), "utf8");

const version = JSON.parse(read("product.json")).version;
const lock = JSON.parse(read("package-lock.json"));
const found = {
  "package.json": JSON.parse(read("package.json")).version,
  "package-lock.json": lock.version,
  "package-lock.json (root package)": lock.packages?.[""]?.version,
  "Cargo.toml [workspace.package]": read("Cargo.toml").match(
    /^\[workspace\.package\]\s*$[^[]*?^version\s*=\s*"([^"]+)"/m,
  )?.[1],
};

const errors = Object.entries(found)
  .filter(([, value]) => value !== version)
  .map(([file, value]) => `${file} version is ${JSON.stringify(value)}, product.json is "${version}"`);

const tag = process.argv[2];
if (tag) {
  const match = /^v(\d+\.\d+\.\d+)(?:-(?:alpha|beta|rc)\.\d+)?$/.exec(tag);
  if (!match) {
    errors.push(`tag "${tag}" is not vX.Y.Z or vX.Y.Z-(alpha|beta|rc).N`);
  } else if (match[1] !== version) {
    errors.push(`tag "${tag}" does not match product.json version "${version}"`);
  }
}

if (errors.length) {
  for (const error of errors) console.error(`::error::${error}`);
  process.exit(1);
}
console.log(`release version: ${version}${tag ? ` (tag ${tag})` : " (no tag: dry run)"}`);
