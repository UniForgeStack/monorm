import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const bin = fileURLToPath(new URL("../bin.js", import.meta.url));
const root = mkdtempSync(join(tmpdir(), "create-monorm-"));
after(() => rmSync(root, { recursive: true, force: true }));
const run = (...args) => runAs("npm/10.0.0 node/v22.0.0", ...args);
const runAs = (agent, ...args) =>
  spawnSync(process.execPath, [bin, ...args], { encoding: "utf8", env: { ...process.env, npm_config_user_agent: agent } });

test("copies the template, renames _gitignore and sets the package name", () => {
  const dest = join(root, "my-app");
  const result = run(dest);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(readdirSync(dest).sort(), [".gitignore", "index.ts", "package.json", "schema.prisma"]);
  assert.ok(!existsSync(join(dest, "_gitignore")));
  assert.match(readFileSync(join(dest, ".gitignore"), "utf8"), /node_modules/);
  const pkg = JSON.parse(readFileSync(join(dest, "package.json"), "utf8"));
  assert.equal(pkg.name, "my-app");
  assert.match(pkg.dependencies.monorm, /^npm:@ufstack\/monorm@\^\d+\.\d+\.\d+/);
  assert.equal(pkg.scripts.start, "node index.ts");
  assert.match(result.stdout, /npm install\n  npm start/);
});

test("uses Bun commands under bun create", () => {
  const dest = join(root, "bun-app");
  const result = runAs("bun/1.2.0 npm/? node/v24.0.0 darwin arm64", dest);
  assert.equal(result.status, 0, result.stderr);
  const pkg = JSON.parse(readFileSync(join(dest, "package.json"), "utf8"));
  assert.equal(pkg.scripts.start, "bun index.ts");
  assert.match(result.stdout, /bun install\n  bun start/);
});

test("refuses a directory that is not empty", () => {
  const dest = join(root, "full");
  assert.equal(run(dest).status, 0);
  writeFileSync(join(dest, "extra.txt"), "x");
  const result = run(dest);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /is not empty/);
});

test("prints the usage without a directory", () => {
  const result = run();
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Usage/);
});
