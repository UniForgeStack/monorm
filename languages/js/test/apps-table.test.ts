/** `[apps]`: the command line reads the apps of the settings file, and finds an installed package that it names with `app:<package>`. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

// `npx monorm` as a process of its own: src/cli.ts (Bun) or dist/src/cli.js (Node)
const CLI = [".js", ".ts"].map((ext) => fileURLToPath(new URL(`../src/cli${ext}`, import.meta.url))).find(existsSync)!;

test("the apps table compiles a bundled app and names a wrong label", (t) => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "monorm-apps-table-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  writeFileSync(join(root, "shop.prisma"), "model Order {\n id Int @id\n}\n");
  const project = (apps: string) => writeFileSync(join(root, "monorm.toml"), `[database]\nprovider = "sqlite"\n[apps]\n${apps}`);
  const check = () => spawnSync(process.execPath, [CLI, "check"], { cwd: root, encoding: "utf8", env: { ...process.env, MONORM_DATABASE_URL: "" } });

  project('shop = "shop.prisma"\nauth = "app:auth"\n');
  const ok = check();
  assert.equal(ok.status, 0, ok.stderr);
  // the package declares the label `auth`: the project cannot relabel it
  project('shop = "shop.prisma"\nidentity = "app:auth"\n');
  const relabeled = check();
  assert.equal(relabeled.status, 1, relabeled.stderr);
  assert.match(relabeled.stderr, /`auth`[\s\S]*`identity`|`identity`[\s\S]*`auth`/);
  // a prefix on an installed app is a setup error
  project('shop = "shop.prisma"\nauth = { path = "app:auth", prefix = "x_" }\n');
  const prefixed = check();
  assert.equal(prefixed.status, 2, prefixed.stderr);
  assert.match(prefixed.stderr, /not allowed on an installed app/);
});
