/** Permission strategies, sync and seeded roles, through `admin.testClient(db, { user })`. */

import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, AdminConfigError } from "../src/admin.js";
import { resolvePatternGrants } from "../src/admin-permissions.js";
import { UserMethods, UserQueries, makePassword } from "../src/auth/index.js";
import { connect, load, Registry, useModelMethods, useQuerySet, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;
const ROLES = "/admin/api/resources/Role";
const LIST = "/admin/api/resources";

type Loose = any;
interface World {
  readonly site: Admin;
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-permissions-"));
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
    const password = makePassword("pw");
    await models["User"].objects.using(db).insertMany(
      ["ann", "bob", "root"].map((name) => ({ username: name, email: `${name}@example.com`, password, isSuperuser: name === "root" })),
    );
    const site = new Admin(registry);
    site.register(models["Role"]);
    site.register(models["Permission"], { allow: ["read"] });
    await run({ site, registry, db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

/** The user holds `names` through a role of the same name as the user. */
async function grant(db: Database, models: Record<string, Loose>, username: string, names: string[]): Promise<unknown> {
  const rows = (await models["Permission"].objects.using(db)).filter((r: Loose) => names.includes(`${r.label}.${r.codename}`));
  assert.equal(rows.length, names.length);
  const Role = models["Role"];
  await Role.objects.using(db).insertMany([{ name: `for-${username}` }]);
  const role = await Role.objects.using(db).filter(Role.name.eq(`for-${username}`)).first();
  const User = models["User"];
  const user = await User.objects.using(db).filter(User.username.eq(username)).first();
  await models["RolePermission"].objects.using(db).insertMany(rows.map((p: Loose) => ({ roleId: role.id, permissionId: p.id })));
  await models["UserRole"].objects.using(db).insertMany([{ userId: user.id, roleId: role.id }]);
  return user.id;
}

async function audit(db: Database, models: Record<string, Loose>): Promise<[string, string, string][]> {
  const entry = models["AdminLogEntry"];
  const rows = await entry.objects.using(db).orderBy(entry.id);
  return rows.map((r: Loose) => [r.userKey, r.action, r.resource]);
}

for (const provider of PROVIDERS) {
  test(`each strategy lists its names (${provider})`, () => world(provider, async ({ site, registry, models }) => {
    assert.deepEqual(site.permissions, ["admin.access", "admin.read_role", "admin.manage_role", "admin.read_permission", "admin.manage_permission"]);
    const model = new Admin(registry, { permissions: "model", label: "staff" });
    model.register(models["Role"], { actions: ["export"], fieldPermissions: ["see_name"] });
    assert.deepEqual(model.permissions, [
      "admin.access",
      "staff.add_role",
      "staff.change_role",
      "staff.delete_role",
      "staff.view_role",
      "staff.see_name",
      "staff.export_role",
    ]);
    const only = new Admin(registry, { permissions: "access" });
    only.register(models["Role"]);
    assert.deepEqual(only.permissions, ["admin.access"]);
  }));

  test(`perms with delete adds a delete name (${provider})`, () => world(provider, async ({ registry, models }) => {
    const split = new Admin(registry);
    split.register(models["Role"], { perms: ["read", "manage", "delete"] });
    assert.equal(split.permissions.at(-1), "admin.delete_role");
  }));

  test(`a wrong permission option is an admin config error (${provider})`, () => world(provider, async ({ site, registry, models }) => {
    const bad: [Admin, Loose, string][] = [
      [new Admin(registry, { permissions: "roles" as never }), undefined, "page"],
      [new Admin(registry), { allow: ["write"] }, "allow"],
      [new Admin(registry), { perms: ["read"] }, "perms"],
      [new Admin(registry, { permissions: "model" }), { perms: ["read", "manage"] }, "page strategy"],
      [new Admin(registry), { actions: ["Bad.Name"] }, "lowercase"],
    ];
    for (const [candidate, config, text] of bad) {
      candidate.register(models["Role"], config);
      assert.throws(() => candidate.check(), (e: Error) => e instanceof AdminConfigError && e.message.includes(text), text);
    }
    assert.throws(() => site.register(models["User"], { bogus: 1 } as never), /unknown option/);
  }));

  test(`sync creates the names once and reports stale names without deleting them (${provider})`, () => world(provider, async ({ site, db, models }) => {
    const first = await site.syncPermissions(db);
    assert.deepEqual([first.created, first.stale], [5, []]);
    const perm = models["Permission"];
    await perm.objects.using(db).insertMany([
      { label: "admin", codename: "gone", name: "Can gone" },
      { label: "other", codename: "x", name: "x" },
    ]);
    const second = await site.syncPermissions(db);
    assert.deepEqual([second.created, second.stale], [0, ["admin.gone"]]);
    assert.equal(await perm.objects.using(db).count(), 7);
  }));

  test(`a role grant reads and a missing grant is a 403 with an audit row (${provider})`, () => world(provider, async ({ site, db, models }) => {
    await site.syncPermissions(db);
    const ann = await grant(db, models, "ann", ["admin.access", "admin.read_role"]);
    const bob = await grant(db, models, "bob", ["admin.read_role"]);
    assert.equal((await site.testClient(db, { user: { key: ann } }).get(ROLES)).status, 200);
    assert.equal((await site.testClient(db, { user: { key: ann } }).get("/admin/api/resources/Permission")).status, 403);
    assert.equal((await site.testClient(db, { user: { key: bob } }).get(ROLES)).status, 403);
    assert.deepEqual(await audit(db, models), [
      [String(ann), "permission_denied", "Permission"],
      [String(bob), "permission_denied", ""],
    ]);
  }));

  test(`manage includes read and model view or change reads (${provider})`, () => world(provider, async ({ site, registry, db, models }) => {
    await site.syncPermissions(db);
    const ann = await grant(db, models, "ann", ["admin.access", "admin.manage_role"]);
    assert.equal((await site.testClient(db, { user: { key: ann } }).get(ROLES)).status, 200);
    const model = new Admin(registry, { permissions: "model" });
    model.register(models["Role"]);
    await model.syncPermissions(db);
    const bob = await grant(db, models, "bob", ["admin.access", "admin.change_role"]);
    assert.equal((await model.testClient(db, { user: { key: bob } }).get(ROLES)).status, 200);
    assert.equal((await model.testClient(db, { user: { key: ann } }).get(ROLES)).status, 403);
  }));

  test(`a superuser has everything but allow limits it and an inactive user has nothing (${provider})`, () => world(provider, async ({ site, registry, db, models }) => {
    const su = { is_superuser: true };
    assert.equal((await site.testClient(db, { user: su }).get(ROLES)).status, 200);
    assert.equal((await site.testClient(db, { user: { ...su, is_active: false } }).get(ROLES)).status, 403);
    const limited = new Admin(registry);
    limited.register(models["Role"], { allow: ["create"] });
    limited.register(models["Permission"]);
    assert.equal((await limited.testClient(db, { user: su }).get(ROLES)).status, 403);
    const names = (await limited.testClient(db, { user: su }).get(LIST)).json().resources.map((r: Loose) => r.name);
    assert.deepEqual(names, ["Permission"]);
  }));

  test(`the grants cost one statement for a user and none for a superuser (${provider})`, () => world(provider, async ({ site, db, models }) => {
    await site.syncPermissions(db);
    const ann = await grant(db, models, "ann", ["admin.access", "admin.read_role"]);
    const root = await site.testClient(db, { user: { is_superuser: true } }).get(ROLES);
    const plain = await site.testClient(db, { user: { key: ann } }).get(ROLES);
    assert.equal(plain.queries, root.queries + 1);
  }));

  test(`metadata has only what the user can use and the filtered names (${provider})`, () => world(provider, async ({ site, db, models }) => {
    await site.syncPermissions(db);
    const ann = await grant(db, models, "ann", ["admin.access", "admin.read_role", "admin.manage_permission"]);
    let body = (await site.testClient(db, { user: { key: ann } }).get(LIST)).json();
    assert.deepEqual(body.resources.map((r: Loose) => r.name), ["Role", "Permission"]);
    assert.deepEqual(body.permissions, ["admin.access", "admin.read_role", "admin.manage_permission"]);
    const bob = await grant(db, models, "bob", ["admin.access", "admin.read_role"]);
    body = (await site.testClient(db, { user: { key: bob } }).get(LIST)).json();
    assert.deepEqual(body.resources.map((r: Loose) => r.name), ["Role"]);
    assert.equal((await site.testClient(db, { user: { key: 999 } }).get(LIST)).status, 403);
  }));

  test(`pattern grants seed roles and a second run changes nothing (${provider})`, () => world(provider, async ({ registry, db, models }) => {
    const seeded = new Admin(registry, { roles: { support: ["admin.*", "-admin.manage_*"], reader: ["admin.read_role"] } });
    seeded.register(models["Role"]);
    seeded.register(models["Permission"]);
    await seeded.syncPermissions(db);
    const first = await seeded.seedRoles(db);
    assert.deepEqual([first.rolesCreated.length, first.grants], [2, 4]);
    const again = await seeded.seedRoles(db);
    assert.deepEqual([again.rolesCreated.length, again.grants], [0, 0]);
    const Role = models["Role"];
    const support = await Role.objects.using(db).filter(Role.name.eq("support")).first();
    const Through = models["RolePermission"];
    assert.equal((await Through.objects.using(db).filter(Through.roleId.eq(support.id))).length, 3);
    assert.equal(await Role.objects.using(db).count(), 2);
  }));

  test(`a pattern that matches nothing and a missing sync are errors (${provider})`, () => world(provider, async ({ registry, db, models }) => {
    const typo = new Admin(registry, { roles: { support: ["orders.*"] } });
    typo.register(models["Role"]);
    await assert.rejects(typo.seedRoles(db), (e: Error) => e instanceof AdminConfigError && e.message.includes("matches no permission name"));
    const names = ["admin.access", "admin.read_role"];
    assert.deepEqual(resolvePatternGrants(["admin.*", "-admin.acce??"], names, "r"), ["admin.read_role"]);
    assert.throws(() => resolvePatternGrants(["admin.READ_*"], names, "r"), /matches no permission name/);
    assert.throws(() => resolvePatternGrants(["admin.[ar]*"], names, "r"), /has \[ or \]/);
    const unsynced = new Admin(registry, { roles: { support: ["admin.read_role"] } });
    unsynced.register(models["Role"]);
    await assert.rejects(unsynced.seedRoles(db), (e: Error) => e instanceof AdminConfigError && e.message.includes("sync-permissions"));
  }));
}
