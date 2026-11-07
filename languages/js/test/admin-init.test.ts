/** `monorm admin init` in a project directory, and the copies of the `app:admin` schema. */

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { PACKAGE } from "./auth/support.js";

// `npx monorm` as a process of its own: src/cli.ts (Bun) or dist/src/cli.js (Node)
const CLI = [".js", ".ts"].map((ext) => fileURLToPath(new URL(`../src/cli${ext}`, import.meta.url))).find(existsSync)!;

function cli(args: string[], cwd: string): { code: number; out: string; err: string } {
  const r = spawnSync(process.execPath, [CLI, ...args], { cwd, encoding: "utf8", env: { ...process.env, MONORM_DATABASE_URL: "" } });
  return { code: r.status ?? -1, out: r.stdout, err: r.stderr };
}

const SCHEMA = 'model Book {\n  id    BigInt @id @default(autoincrement())\n  title String\n\n  @@map("books")\n}\n';

test("the admin schema copies of both packages match the source", () => {
  const source = readFileSync(join(PACKAGE, "../../apps/admin/schema.prisma"), "utf8");
  assert.equal(readFileSync(join(PACKAGE, "admin/schema.prisma"), "utf8"), source);
  assert.equal(readFileSync(join(PACKAGE, "../../apps/admin/python/monorm_admin/schema.prisma"), "utf8"), source);
});

test("admin init writes the module and the settings and the schema still checks", () => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-init-"));
  writeFileSync(join(dir, "schema.prisma"), SCHEMA);
  const first = cli(["admin", "init", "--models", "./models.js"], dir);
  assert.equal(first.code, 0, first.err);
  assert.match(first.out, /next: makemigrations, migrate/);
  const module = readFileSync(join(dir, "admin.ts"), "utf8");
  assert.ok(module.includes('import { Book } from "./models.js"') && module.includes("admin.register(Book);"), module);
  assert.equal(readFileSync(join(dir, "monorm.toml"), "utf8"), '[admin]\napp = "./admin.ts:admin"\n');
  assert.ok(readFileSync(join(dir, "schema.prisma"), "utf8").includes('import "app:admin" (prefix: "admin_")'));
  const check = cli(["check"], dir);
  assert.equal(check.code, 0, check.err);
  assert.match(check.out, /ok \(3 models\)/);
  const again = cli(["admin", "init", "--models", "./models.js"], dir);
  assert.equal(again.code, 2);
  assert.match(again.err, /already have an admin section/);
});
