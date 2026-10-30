import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

// bin.js runs from a temporary package whose `@ufstack/monorm` is the registry package that `npm install` put in this
// package when CREATE_MONORM_TEST_REGISTRY is set (the publish job), else the local build (languages/js).
const local = fileURLToPath(new URL("../..", import.meta.url));
const installed = fileURLToPath(new URL("../node_modules/@ufstack/monorm", import.meta.url));
const registry = Boolean(process.env.CREATE_MONORM_TEST_REGISTRY);
if (!registry && !existsSync(join(local, "dist", "src", "cli.js"))) {
  throw new Error("build languages/js first (npm run assemble), or set CREATE_MONORM_TEST_REGISTRY=1 to test the registry package");
}
const monorm = registry ? realpathSync(installed) : local;
const root = mkdtempSync(join(tmpdir(), "create-monorm-"));
after(() => rmSync(root, { recursive: true, force: true }));
const pkg = join(root, "pkg");
mkdirSync(join(pkg, "node_modules", "@ufstack"), { recursive: true });
symlinkSync(monorm, join(pkg, "node_modules", "@ufstack", "monorm"), "dir");
copyFileSync(fileURLToPath(new URL("../bin.js", import.meta.url)), join(pkg, "bin.js"));
writeFileSync(join(pkg, "package.json"), '{"type":"module"}\n');
const run = (...args) => runAs("npm/10.0.0 node/v22.0.0", ...args);
const runAs = (agent, ...args) =>
  spawnSync(process.execPath, [join(pkg, "bin.js"), ...args], { encoding: "utf8", env: { ...process.env, npm_config_user_agent: agent } });

test("writes a TypeScript project on SQLite with the npm commands", () => {
  const dest = join(root, "my-app");
  const result = run(dest);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(readdirSync(dest).sort(), [".gitignore", "README.md", "index.ts", "package.json", "schema.prisma"]);
  const manifest = JSON.parse(readFileSync(join(dest, "package.json"), "utf8"));
  assert.equal(manifest.name, "my-app");
  assert.match(manifest.dependencies.monorm, /^npm:@ufstack\/monorm@\^\d+\.\d+\.\d+/);
  assert.equal(manifest.scripts.start, "node index.ts");
  assert.match(result.stdout, /npm install\n\s+npm start/);
});

test("uses Bun commands under bun create", () => {
  const dest = join(root, "bun-app");
  const result = runAs("bun/1.2.0 npm/? node/v24.0.0 darwin arm64", dest);
  assert.equal(result.status, 0, result.stderr);
  const manifest = JSON.parse(readFileSync(join(dest, "package.json"), "utf8"));
  assert.equal(manifest.scripts.start, "bun index.ts");
  assert.match(result.stdout, /bun install\n\s+bun start/);
});

test("passes the other arguments to monorm init", () => {
  const dest = join(root, "pg-app");
  const result = run(dest, "--db", "postgresql", "--url", "postgresql://u:p@localhost/x", "--ext", "timestamps", "--yes");
  assert.equal(result.status, 0, result.stderr);
  assert.match(readFileSync(join(dest, "index.ts"), "utf8"), /postgresql:\/\/u:p@localhost\/x/);
  assert.match(readFileSync(join(dest, "schema.prisma"), "utf8"), /@@timestamps/);
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
