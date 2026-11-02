// Fails if generated TypeScript bindings differ from what is committed (drift check for CI).
import { execSync } from "node:child_process";

const status = execSync("git status --porcelain -- src/lib/generated/bindings", { encoding: "utf8" });
if (status.trim()) {
  const diff = execSync("git diff -- src/lib/generated/bindings", {
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
  });
  console.error(
    "Generated bindings are out of date. Run `npm run gen:types` and commit the result:\n" +
      status +
      "\n" +
      diff.split("\n").slice(0, 80).join("\n")
  );
  process.exit(1);
}
console.log("bindings: up to date");
