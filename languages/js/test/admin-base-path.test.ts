/** The base path of the admin and the cookie scope, through `admin.testClient`. */

import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, AdminConfigError, type AdminOptions } from "../src/admin.js";
import { UserMethods, UserQueries, makePassword } from "../src/auth/index.js";
import { connect, load, Registry, useModelMethods, useQuerySet, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;
const SECRET_A = "a".repeat(40);
const SECRET_B = "b".repeat(40);
const SU = { is_superuser: true };

type Loose = any;
interface World {
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-base-"));
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
    await models["User"].objects.using(db).insertMany([{ username: "ann", email: "ann@example.com", password: makePassword("pw"), isSuperuser: true }]);
    await models["Role"].objects.using(db).insertMany([{ name: "editors" }, { name: "readers" }]);
    await run({ registry, db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

function siteAt({ registry, models }: World, options: AdminOptions): Admin {
  const site = new Admin(registry, options);
  site.register(models["Role"]);
  return site;
}

function checkError(site: Admin): string {
  try {
    site.check();
  } catch (e) {
    assert.ok(e instanceof AdminConfigError);
    return e.message;
  }
  throw new assert.AssertionError({ message: "check did not throw" });
}

for (const provider of PROVIDERS) {
  test(`a site at a base path serves list, detail, write, export and a custom route there only (${provider})`, () => world(provider, async (w) => {
    const site = siteAt(w, { basePath: "/bundler/admin/" });
    site.route("GET", "reports/summary", async () => ({ rows: 2 }));
    assert.equal(site.basePath, "/bundler/admin/");
    assert.equal(site.prefix, undefined);
    const client = site.testClient(w.db, { user: SU });
    const api = "/bundler/admin/api";
    const preamble = await client.get(`${api}/resources`);
    const listing = await client.get(`${api}/resources/Role`);
    assert.equal(listing.status, 200, listing.text);
    assert.equal((await client.get(`${api}/resources/Role/1`)).status, 200);
    const created = await client.post(`${api}/resources/Role`, { json: { set: { name: "ops" } } });
    assert.equal(created.status, 201, created.text);
    assert.deepEqual([listing.queries, created.queries], [preamble.queries + 1, preamble.queries + 2]); // the base path adds no statement
    const role = await w.models["Role"].objects.using(w.db).filter(w.models["Role"].name.eq("ops")).first();
    assert.ok(role !== null && created.json().key === String(role.id));
    const entry = w.models["AdminLogEntry"];
    const rows: Loose[] = await entry.objects.using(w.db).orderBy(entry.id);
    assert.deepEqual(rows.map((r) => [r.action, r.resource, r.recordKey]), [["create", "Role", String(role.id)]]);
    const exported = await client.get(`${api}/export/Role`);
    assert.equal(exported.status, 200, exported.text);
    assert.ok(exported.text.includes("editors") && exported.text.includes("ops"));
    assert.deepEqual((await client.get(`${api}/routes/reports/summary`)).json(), { rows: 2 });
    assert.notEqual((await client.get("/admin/api/resources/Role")).status, 200);
    assert.equal(site.url(w.models["Role"], 1), "/bundler/admin/Role/1");
  }));

  test(`two sites on one host keep their own cookie paths and sessions (${provider})`, () => world(provider, async (w) => {
    const a = siteAt(w, { basePath: "/a/admin" }).testClient(w.db, { secret: SECRET_A });
    const b = siteAt(w, { basePath: "/b/admin" }).testClient(w.db, { secret: SECRET_B });
    const loginA = await a.login("ann", "pw");
    assert.equal(loginA.status, 200, loginA.text);
    const session = loginA.cookie("__Secure-monorm-session")!;
    const csrf = loginA.cookie("__Secure-monorm-csrf")!;
    for (const flag of ["Path=/a/admin/", "HttpOnly", "Secure", "SameSite=Lax"]) assert.ok(session.includes(flag), flag);
    assert.ok(csrf.includes("Path=/a/admin/") && !csrf.includes("HttpOnly"));
    assert.equal((await a.get("/a/admin/api/me")).status, 200);
    assert.equal((await b.get("/b/admin/api/me")).status, 401);
    const loginB = await b.login("ann", "pw");
    assert.ok(loginB.cookie("__Secure-monorm-session")!.includes("Path=/b/admin/"));
    const out = await a.logout();
    const ended = out.headers.filter(([k]) => k.toLowerCase() === "set-cookie");
    assert.ok(ended.length > 0 && ended.every(([, v]) => v.includes("Path=/a/admin/")));
    assert.equal((await a.get("/a/admin/api/me")).status, 401);
    assert.equal((await b.get("/b/admin/api/me")).status, 200);
  }));

  test(`check names a bad base path and a prefix outside the base (${provider})`, () => world(provider, async (w) => {
    assert.match(checkError(siteAt(w, { basePath: "admin" })), /must start with \//);
    assert.match(checkError(siteAt(w, { basePath: "/" })), /is empty/);
    assert.match(checkError(siteAt(w, { basePath: "/a b" })), /use letters, digits/);
    assert.match(checkError(siteAt(w, { basePath: "/a/../b" })), /empty or dot segment/);
    assert.match(checkError(siteAt(w, { basePath: "/bundler/admin", prefix: "/other/api" })), /must sit under the base path/);
    assert.match(checkError(siteAt(w, { basePath: "/bundler/admin", prefix: "/bundler/admin" })), /path of the UI/);
    assert.match(checkError(siteAt(w, { basePath: "/bundler/admin", prefix: "/bundler/admin/Role/v1" })), /API prefix/);
    for (const taken of ["assets", "shared", "index.html", "-", "-/login"]) {
      assert.match(checkError(siteAt(w, { basePath: "/bundler/admin", prefix: `/bundler/admin/${taken}` })), /for its pages and files/, taken);
    }
    assert.deepEqual(siteAt(w, { basePath: "/bundler/admin", prefix: "/bundler/admin/back" }).check(), []);
  }));
}
