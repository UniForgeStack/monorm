/** The example extension package (`examples/admin-package/rating.mjs`): one widget, one detail tab, one route and one action,
 * plugged in with `install(admin, model)`, through `admin.testClient` on PostgreSQL and SQLite. */

import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";

import { Admin } from "../src/admin.js";
import { UserMethods, UserQueries, makePassword } from "../src/auth/index.js";
import { connect, load, Registry, useModelMethods, useQuerySet, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;
const BASE = "/admin/api";
const ROOT = { key: 1, is_superuser: true };
const EXAMPLE = resolve(process.cwd(), "..", "..", "examples", "admin-package");
const PACKAGE_ROOT = process.cwd();

type Loose = any;
interface World {
  readonly site: Admin;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

function schema(provider: string): string {
  return `datasource db {
  provider = "${provider}"
}
import "app:auth" (prefix: "auth_")
import "app:admin" (prefix: "admin_")

model Movie {
  id    BigInt @id @default(autoincrement())
  title String
  stars Int?
  @@map("movies")
}
`;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-example-"));
  writeFileSync(join(dir, "package.json"), "{}");
  writeFileSync(join(dir, "schema.prisma"), schema(provider));
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
    await models["User"].objects.using(db).insertMany([{ username: "root", email: "root@example.com", password: makePassword("pw"), isSuperuser: true }]);
    await models["Movie"].objects.using(db).insertMany([{ title: "Alpha", stars: 4 }, { title: "Beta", stars: 2 }, { title: "Gamma", stars: null }]);
    const site = new Admin(registry);
    // The example imports "monorm/admin", as in a project: a copy next to a linked node_modules/monorm resolves it to this build.
    cpSync(EXAMPLE, join(dir, "example"), { recursive: true });
    mkdirSync(join(dir, "example", "node_modules"), { recursive: true });
    symlinkSync(PACKAGE_ROOT, join(dir, "example", "node_modules", "monorm"), "dir");
    const example = (await import(pathToFileURL(join(dir, "example", "rating.mjs")).href)) as { install(admin: Admin, model: unknown): void };
    example.install(site, models["Movie"]);
    await run({ site, db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

async function movieId(w: World, title: string): Promise<string> {
  const rows = (await w.models["Movie"].objects.using(w.db)) as Loose[];
  return String(rows.find((r) => r.title === title).id);
}

for (const provider of PROVIDERS) {
  test(`the example package plugs in with one call and its files and metadata are served (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { user: ROOT });
    const body = (await client.get(`${BASE}/resources`)).json() as Loose;
    const movie = body.resources.find((r: Loose) => r.name === "Movie");
    assert.deepEqual(movie.package_tabs, [{ name: "rating.summary", title: "Summary", module: "packages/rating/summary.js" }]);
    assert.deepEqual(body.packages.map((p: Loose) => p.name), ["rating"]);
    const stars = movie.form.find((f: Loose) => f.name === "stars");
    assert.equal(stars.widget, "rating.stars");
    const widget = await client.get(`${BASE}/packages/rating/stars.js`);
    assert.deepEqual([widget.status, widget.text.includes('from "react"'), widget.text.includes("adminMajor")], [200, true, true]);
    assert.equal((await client.get(`${BASE}/packages/rating/rating.css`)).status, 200);
  }));

  test(`the example route reads the record and the average behind the session (${provider})`, () => world(provider, async (w) => {
    const first = await movieId(w, "Alpha");
    assert.equal((await w.site.testClient(w.db).get(`${BASE}/routes/rating/${first}/summary`)).status, 401);
    const client = w.site.testClient(w.db, { user: ROOT });
    const r = await client.get(`${BASE}/routes/rating/${first}/summary`);
    assert.deepEqual([r.status, r.json()], [200, { title: "Alpha", stars: 4, count: 2, average: 3.0 }]);
    assert.equal((await client.get(`${BASE}/routes/rating/999999/summary`)).status, 404);
  }));

  test(`the example action clears the stars of one record (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { user: ROOT });
    const first = await movieId(w, "Alpha");
    const r = await client.post(`${BASE}/actions/Movie/reset_stars`, { json: { keys: [first], inputs: {} } });
    assert.equal(r.status, 200);
    const rows = (await w.models["Movie"].objects.using(w.db)) as Loose[];
    assert.deepEqual(rows.map((m) => [m.title, m.stars]).sort(), [["Alpha", null], ["Beta", 2], ["Gamma", null]]);
  }));
}
