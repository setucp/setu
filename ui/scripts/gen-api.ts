/**
 * Generates the typed API client from the repo's checked-in OpenAPI
 * document. Reads `openapi.json` from the repo root, never a running
 * server (CLAUDE.md working rules).
 *
 * Usage:
 *   pnpm gen:api
 */
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const uiRoot = join(__dirname, "..");
const snapshot = join(uiRoot, "..", "openapi.json");
const output = join(uiRoot, "src", "lib", "api-types.d.ts");

function main() {
  let spec: string;
  try {
    spec = readFileSync(snapshot, "utf8");
  } catch {
    process.stderr.write(`openapi.json not found at ${snapshot}; regenerate it with \`setu openapi\`\n`);
    process.exit(1);
  }

  const tmp = join(uiRoot, ".openapi-tmp.json");
  try {
    writeFileSync(tmp, spec);
    mkdirSync(dirname(output), { recursive: true });
    execFileSync(
      process.platform === "win32" ? "npx.cmd" : "npx",
      ["openapi-typescript", ".openapi-tmp.json", "--output", "src/lib/api-types.d.ts"],
      { cwd: uiRoot, stdio: "inherit" },
    );
  } finally {
    execFileSync("rm", ["-f", tmp], { cwd: uiRoot });
  }
  process.stdout.write(`wrote ${output}\n`);
}

main();
