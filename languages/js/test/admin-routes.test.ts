/** Custom admin routes (`Admin.route`): the session, the permission, the request, the JSON and stream answers, through `admin.testClient`. */

import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, AdminConfigError, Reply, Stream, type RouteFunction } from "../src/admin.js";
import { UserMethods, UserQueries, makePassword } from "../src/auth/index.js";
import { connect, load, Registry, useModelMethods, useQuerySet, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;
const SECRET = "s".repeat(40);
const BASE = "/admin/api/routes";
const ROOT = { key: 1, is_superuser: true };

type Loose = any;
type RouteSpec = readonly [method: string, path: string, fn: RouteFunction, permission: string | null];
interface World {
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-routes-"));
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
      ["ann", "bob", "cy", "root"].map((name) => ({ username: name, email: `${name}@example.com`, password, isSuperuser: name === "root" })),
    );
    await run({ registry, db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

async function userId(db: Database, models: Record<string, Loose>, name: string): Promise<number> {
  const User = models["User"];
  return Number((await User.objects.using(db).filter(User.username.eq(name)).first()).id);
}

/** The user holds `names` through a role of the same name as the user. */
async function grant(db: Database, models: Record<string, Loose>, username: string, names: string[]): Promise<number> {
  const rows = (await models["Permission"].objects.using(db)).filter((r: Loose) => names.includes(`${r.label}.${r.codename}`));
  assert.equal(rows.length, names.length);
  const Role = models["Role"];
  await Role.objects.using(db).insertMany([{ name: `for-${username}` }]);
  const role = await Role.objects.using(db).filter(Role.name.eq(`for-${username}`)).first();
  const uid = await userId(db, models, username);
  await models["RolePermission"].objects.using(db).insertMany(rows.map((p: Loose) => ({ roleId: role.id, permissionId: p.id })));
  await models["UserRole"].objects.using(db).insertMany([{ userId: uid, roleId: role.id }]);
  return uid;
}

async function audit(db: Database, models: Record<string, Loose>): Promise<[string, string, any][]> {
  const entry = models["AdminLogEntry"];
  const rows = await entry.objects.using(db).orderBy(entry.id);
  return rows.map((r: Loose) => [r.action, r.resource, r.changes]);
}

async function build({ registry, db, models }: World, ...routes: RouteSpec[]): Promise<Admin> {
  const site = new Admin(registry);
  site.register(models["Role"]);
  for (const [method, path, fn, permission] of routes) site.route(method, path, fn, permission === null ? {} : { permission });
  await site.syncPermissions(db);
  return site;
}

const summary: RouteFunction = async () => ({ rows: 3 });
const report: RouteFunction = async (ctx, request) => ({
  id: request.params["id"],
  user: (ctx.user as Loose)["key"],
  has_db: ctx.database !== null,
  request_id: typeof ctx.requestId === "string",
});

for (const provider of PROVIDERS) {
  test(`a route without a session is 401 and the function does not run (${provider})`, () => world(provider, async (w) => {
    const calls: string[] = [];
    const site = await build(w, ["GET", "reports/summary", async (_ctx, request) => void calls.push(request.path), null]);
    const r = await site.testClient(w.db).get(`${BASE}/reports/summary`);
    assert.equal(r.status, 401);
    assert.deepEqual(calls, []);
  }));

  test(`a route that is twice or not a function is refused at the call (${provider})`, () => world(provider, async (w) => {
    const site = await build(w, ["GET", "reports/summary", summary, null]);
    assert.throws(() => site.route("GET", "reports/summary", summary), (e: Error) => e instanceof AdminConfigError && e.message.includes("registered twice"));
    assert.throws(() => site.route("GET", "other", 7 as never), (e: Error) => e instanceof AdminConfigError && e.message.includes("needs a function"));
  }));

  test(`a route needs the gate and its permission and a refusal is audited (${provider})`, () => world(provider, async (w) => {
    const site = await build(w, ["GET", "reports/summary", summary, null], ["GET", "reports/{id}", report, "admin.run_report"]);
    const ann = await grant(w.db, w.models, "ann", ["admin.access", "admin.run_report"]);
    const bob = await grant(w.db, w.models, "bob", ["admin.access"]);
    const cy = await userId(w.db, w.models, "cy");
    assert.equal((await site.testClient(w.db, { user: { key: cy } }).get(`${BASE}/reports/summary`)).status, 403);
    assert.deepEqual((await site.testClient(w.db, { user: { key: bob } }).get(`${BASE}/reports/summary`)).json(), { rows: 3 });
    const denied = await site.testClient(w.db, { user: { key: bob } }).get(`${BASE}/reports/7`);
    assert.equal(denied.status, 403);
    const refused = (await audit(w.db, w.models)).filter(([a, res]) => a === "permission_denied" && res.startsWith("route:"));
    assert.deepEqual(refused.map(([a, res, ch]) => [a, res, ch.needs]), [["permission_denied", "route:reports/{id}", ["admin.run_report"]]]);
    const ok = await site.testClient(w.db, { user: { key: ann } }).get(`${BASE}/reports/7`);
    assert.equal(ok.status, 200);
    assert.deepEqual(ok.json(), { id: "7", user: ann, has_db: true, request_id: true });
  }));

  test(`the route permission shows in permissions and an unknown label is a build error (${provider})`, () => world(provider, async ({ registry, models }) => {
    const site = new Admin(registry);
    site.register(models["Role"]);
    site.route("GET", "a", summary, { permission: "admin.run_report" });
    assert.ok(site.permissions.includes("admin.run_report"));
    const bad = new Admin(registry);
    bad.register(models["Role"]);
    bad.route("GET", "a", summary, { permission: "nope.run_report" });
    assert.throws(() => bad.permissions, (e: Error) => e instanceof AdminConfigError && e.message.includes('unknown label "nope"'));
  }));

  test(`check reports an unknown route label as a build error (${provider})`, () => world(provider, async ({ registry, models }) => {
    const bad = new Admin(registry);
    bad.register(models["Role"]);
    bad.route("GET", "a", summary, { permission: "nope.run_report" });
    assert.throws(() => bad.check(), (e: Error) => e instanceof AdminConfigError && e.message.includes('unknown label "nope"'));
  }));

  test(`the function gets params query and the JSON body and a reply sets the status (${provider})`, () => world(provider, async (w) => {
    const seen: unknown[] = [];
    const rerun: RouteFunction = async (_ctx, request) => {
      seen.push([request.method, request.path, { ...request.params }, { ...request.query }, request.body]);
      return new Reply({ queued: true }, { status: 202 });
    };
    const site = await build(w, ["POST", "reports/{id}/rerun", rerun, null]);
    const r = await site.testClient(w.db, { user: ROOT }).post(`${BASE}/reports/42/rerun?mode=fast&tag=a&tag=b`, { json: { limit: 5 } });
    assert.deepEqual([r.status, r.json()], [202, { queued: true }]);
    assert.deepEqual(seen, [["POST", "reports/{id}/rerun", { id: "42" }, { mode: "fast", tag: ["a", "b"] }, { limit: 5 }]]);
  }));

  test(`a literal path wins over a parameter and a wrong method is 405 with allow (${provider})`, () => world(provider, async (w) => {
    const site = await build(w, ["GET", "reports/summary", async () => "literal", null], ["GET", "reports/{id}", async () => "param", null]);
    const client = site.testClient(w.db, { user: ROOT });
    assert.equal((await client.get(`${BASE}/reports/summary`)).json(), "literal");
    assert.equal((await client.get(`${BASE}/reports/other`)).json(), "param");
    assert.equal((await client.get(`${BASE}/nothing/here`)).status, 404);
    const wrong = await client.delete(`${BASE}/reports/summary`);
    assert.equal(wrong.status, 405);
    assert.equal(wrong.header("allow"), "GET");
  }));

  test(`a sync function and a function that fails (${provider})`, () => world(provider, async (w) => {
    const broken: RouteFunction = async () => {
      throw new Error("secret detail");
    };
    const site = await build(w, ["GET", "sync", () => ({ sync: true }), null], ["GET", "broken", broken, null]);
    const client = site.testClient(w.db, { user: ROOT });
    assert.deepEqual((await client.get(`${BASE}/sync`)).json(), { sync: true });
    const failed = await client.get(`${BASE}/broken`);
    assert.equal(failed.status, 500);
    assert.ok(!failed.text.includes("secret detail"));
  }));

  test(`a stream reaches the client in chunks with its headers (${provider})`, () => world(provider, async (w) => {
    const rows: RouteFunction = async () => {
      async function* chunks() {
        yield "id,name\n";
        for (let i = 0; i < 3; i++) yield new TextEncoder().encode(`${i},row ${i}\n`);
      }
      return new Stream(chunks(), { contentType: "text/csv; charset=utf-8", filename: "rows.csv" });
    };
    const lines: RouteFunction = () => new Stream((function* () { for (let i = 0; i < 2000; i++) yield `line ${i}\n`; })(), { contentType: "text/plain" });
    const site = await build(w, ["GET", "rows", rows, null], ["GET", "lines", lines, null]);
    const client = site.testClient(w.db, { user: ROOT });
    const r = await client.get(`${BASE}/rows`);
    assert.equal(r.status, 200);
    assert.equal(new TextDecoder().decode(r.content), "id,name\n0,row 0\n1,row 1\n2,row 2\n");
    assert.equal(r.header("content-type"), "text/csv; charset=utf-8");
    assert.equal(r.header("content-disposition"), 'attachment; filename="rows.csv"; filename*=UTF-8\'\'rows.csv');
    assert.equal(r.header("cache-control"), "no-store");
    const big = await client.get(`${BASE}/lines`);
    assert.equal(new TextDecoder().decode(big.content), Array.from({ length: 2000 }, (_, i) => `line ${i}\n`).join(""));
  }));

  test(`a login client needs the session and the CSRF token for a write (${provider})`, () => world(provider, async (w) => {
    const echo: RouteFunction = async (_ctx, request) => ({ body: request.body });
    const site = await build(w, ["GET", "echo", echo, null], ["POST", "echo", echo, null]);
    const client = site.testClient(w.db, { secret: SECRET });
    assert.equal((await client.get(`${BASE}/echo`)).status, 401);
    assert.equal((await client.login("root", "pw")).status, 200);
    assert.deepEqual((await client.get(`${BASE}/echo`)).json(), { body: null });
    assert.equal((await client.post(`${BASE}/echo`, { json: { a: 1 }, csrf: "wrong" })).status, 403);
    assert.deepEqual((await client.post(`${BASE}/echo`, { json: { a: 1 } })).json(), { body: { a: 1 } });
  }));
}
