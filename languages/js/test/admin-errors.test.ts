/** The code of an admin error and its class: `AdminConfigError` for the site config, `AdminSettingsError` for the settings. */

import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, AdminConfigError, AdminSettingsError } from "../src/admin.js";
import { connect, load, Registry, type Database } from "../src/index.js";

const SECRET = "s".repeat(40);

type Loose = any;

async function world(run: (site: Admin, db: Database, models: Record<string, Loose>) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-errors-"));
  writeFileSync(join(dir, "package.json"), "{}");
  writeFileSync(join(dir, "schema.prisma"), `datasource db {\n  provider = "sqlite"\n}\nimport "app:auth" (prefix: "auth_")\nimport "app:admin" (prefix: "admin_")\n`);
  const registry = new Registry();
  const models = load(join(dir, "schema.prisma"), { registry }) as unknown as Record<string, Loose>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await run(new Admin(registry), db, models);
  } finally {
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

test("a wrong config raises a config error with a code", () => world(async (site, _db, models) => {
  site.register(models["Role"], { list: { pageSize: 0 } });
  assert.throws(
    () => site.check(),
    (e: unknown) => e instanceof AdminConfigError && !(e instanceof AdminSettingsError) && e.code === "config.invalid_value" && e.message.includes("Role: list.pageSize is 0"),
  );
}));

test("a wrong secret raises a settings error with a code", () => world(async (site, db) => {
  await assert.rejects(
    () => site.testClient(db, { secret: "short" }).get("/admin/api/me"),
    (e: unknown) => e instanceof AdminSettingsError && e.code.startsWith("settings."),
  );
}));

test("a settings key that does not exist has its own code", () => world(async (site, db) => {
  await assert.rejects(
    () => site.testClient(db, { secret: SECRET, settings: { nope: 1 } }).get("/admin/api/me"),
    (e: unknown) => e instanceof AdminSettingsError && e.code === "settings.unknown_key",
  );
}));
