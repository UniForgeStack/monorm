/** The built-in resources of the admin on the auth tables: roles, permissions, role membership and the read-only audit log. */

import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin } from "../src/admin.js";
import { UserMethods, UserQueries, makePassword } from "../src/auth/index.js";
import { connect, load, Registry, useModelMethods, useQuerySet, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;
const API = "/admin/api";
const SU = { is_superuser: true };

type Loose = any;
interface World {
  readonly site: Admin;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-builtins-"));
  writeFileSync(join(dir, "package.json"), "{}");
  writeFileSync(
    join(dir, "schema.prisma"),
    `datasource db {\n  provider = "${provider}"\n}\nimport "app:auth" (prefix: "auth_")\nimport "app:admin" (prefix: "admin_")\n`,
  );
  const registry = new Registry();
  const models = load(join(dir, "schema.prisma"), { registry }) as unknown as Record<string, Loose>;
  useModelMethods(models["User"], UserMethods);
  useQuerySet(models["User"], UserQueries);
  let db: Database;
  try {
    db = await connect(provider === "sqlite" ? "sqlite://:memory:" : PG_URL, { registry, default: false });
  } catch (error) {
    rmSync(dir, { recursive: true, force: true });
    if (process.env["ORM_TEST_DATABASE_URL"] !== undefined) throw error;
    return;
  }
  try {
    await db.dropTables();
    await db.createTables();
    await models["User"].objects.using(db).insertMany([{ username: "ann", email: "ann@example.com", password: makePassword("pw") }]);
    await models["Role"].objects.using(db).insertMany([{ name: "editor" }]);
    const site = new Admin(registry);
    site.register(models["User"]);
    await site.syncPermissions(db);
    // A permission that no admin option names must not reach the role form.
    await models["Permission"].objects.using(db).insertMany([{ label: "other", codename: "x", name: "Can x" }]);
    await run({ site, db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

for (const provider of PROVIDERS) {
  test(`the built-in resources are listed without any registration (${provider})`, () => world(provider, async ({ site, db }) => {
    const body = (await site.testClient(db, { user: SU }).get(`${API}/resources`)).json();
    assert.deepEqual(body.resources.map((r: Loose) => r.name), ["User", "AdminLogEntry", "Permission", "Role", "UserRole"]);
  }));

  test(`the role form offers only the permission names of the site (${provider})`, () => world(provider, async ({ site, db }) => {
    const items = (await site.testClient(db, { user: SU }).get(`${API}/autocomplete/Role/permissions?limit=50`)).json().items;
    const labels = items.map((i: Loose) => i.label);
    assert.equal(labels.length, site.permissions.length);
    assert.ok(!labels.includes("Can x"));
    assert.ok(labels.includes("Can read role"));
  }));

  test(`the audit log is read-only even for a superuser (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { user: SU });
    assert.equal((await client.get(`${API}/resources/AdminLogEntry`)).status, 200);
    for (const reply of [
      await client.post(`${API}/resources/AdminLogEntry`, { json: { set: { action: "login" } } }),
      await client.patch(`${API}/resources/AdminLogEntry/1`, { json: { set: { action: "x" } } }),
      await client.delete(`${API}/resources/AdminLogEntry/1`),
    ]) {
      assert.equal(reply.status, 403, reply.text);
    }
  }));

  test(`the role membership of a user is a resource (${provider})`, () => world(provider, async ({ site, db, models }) => {
    const client = site.testClient(db, { user: SU });
    const user = await models["User"].objects.using(db).filter(models["User"].username.eq("ann")).first();
    const role = await models["Role"].objects.using(db).filter(models["Role"].name.eq("editor")).first();
    const made = await client.post(`${API}/resources/UserRole`, { json: { set: { user_id: user.id, role_id: role.id } } });
    assert.equal(made.status, 201, made.text);
    const listed = (await client.get(`${API}/resources/UserRole`)).json();
    const names = listed.columns.map((c: Loose) => c.label);
    const rows = listed.rows.map((row: unknown[]) => Object.fromEntries(names.map((n: string, i: number) => [n, row[i]])));
    assert.deepEqual(rows.map((r: Loose) => [r.user_id, r.role_id]), [[Number(user.id), Number(role.id)]]);
  }));
}
