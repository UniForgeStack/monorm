/** Extension packages (`Admin.package`): the files, the session, the version check and the metadata, through `admin.testClient`. */

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, AdminConfigError } from "../src/admin.js";
import { UserMethods, UserQueries, makePassword } from "../src/auth/index.js";
import { connect, load, Registry, useModelMethods, useQuerySet, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;
const BASE = "/admin/api";
const ROOT = { key: 1, is_superuser: true };

type Loose = any;
interface World {
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
  readonly pkg: string;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-packages-"));
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
      [{ username: "root", email: "root@example.com", password, isSuperuser: true }],
    );
    const pkg = join(dir, "rating");
    mkdirSync(join(pkg, "lib"), { recursive: true });
    writeFileSync(join(pkg, "stars.js"), 'import React from "react";\nexport const adminMajor = 1;\nexport default () => null;\n');
    writeFileSync(join(pkg, "lib", "form.mjs"), "export const adminMajor = 1;\nexport default () => null;\n");
    writeFileSync(join(pkg, "lib", "reviews.js"), "export const adminMajor = 1;\nexport default () => null;\n");
    writeFileSync(join(pkg, "stars.css"), ".stars { color: gold }\n");
    writeFileSync(join(dir, "secret.js"), "secret");
    await run({ registry, db, models, pkg });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

function build(w: World, adminMajor?: number): Admin {
  const site = new Admin(w.registry, { widgets: { "Role.name": "rating.stars" } });
  site.register(w.models["Role"], { detailTabs: ["rating.reviews"] });
  site.package("rating", w.pkg, { widgets: { stars: "stars.js" }, forms: { edit: "lib/form.mjs" }, tabs: { reviews: "lib/reviews.js" }, styles: ["stars.css"], ...(adminMajor === undefined ? {} : { adminMajor }) });
  return site;
}

for (const provider of PROVIDERS) {
  test(`a package file is served to a signed-in user with its type (${provider})`, () => world(provider, async (w) => {
    const client = build(w).testClient(w.db, { user: ROOT });
    const js = await client.get(`${BASE}/packages/rating/stars.js`);
    assert.equal(js.status, 200);
    assert.ok(js.text.includes("import React"));
    assert.equal((await client.get(`${BASE}/packages/rating/lib/form.mjs`)).status, 200);
    const css = await client.get(`${BASE}/packages/rating/stars.css`);
    assert.deepEqual([css.status, css.text.includes(".stars")], [200, true]);
  }));

  test(`a package file needs a session and only loaded files are served (${provider})`, () => world(provider, async (w) => {
    const site = build(w);
    assert.equal((await site.testClient(w.db).get(`${BASE}/packages/rating/stars.js`)).status, 401);
    const client = site.testClient(w.db, { user: ROOT });
    for (const path of ["secret.js", "../secret.js", "%2e%2e/secret.js", "nothing.js", "lib"]) {
      assert.equal((await client.get(`${BASE}/packages/rating/${path}`)).status, 404, path);
    }
    assert.equal((await client.get(`${BASE}/packages/other/stars.js`)).status, 404);
  }));

  test(`the resources route lists the package and the admin major (${provider})`, () => world(provider, async (w) => {
    const body = (await build(w).testClient(w.db, { user: ROOT }).get(`${BASE}/resources`)).json();
    assert.equal(body.admin_major, 1);
    const role = body.resources.find((r: Loose) => r.name === "Role");
    assert.deepEqual(role.package_tabs, [{ name: "rating.reviews", title: "Reviews", module: "packages/rating/lib/reviews.js" }]);
    assert.deepEqual(body.packages, [
      { name: "rating", admin_major: 1, widgets: { stars: "packages/rating/stars.js" }, forms: { edit: "packages/rating/lib/form.mjs" }, tabs: { reviews: "packages/rating/lib/reviews.js" }, styles: ["packages/rating/stars.css"] },
    ]);
  }));

  test(`a package built for another admin major is a build error (${provider})`, () => world(provider, async (w) => {
    const site = build(w, 2);
    assert.throws(() => site.permissions, (e: Error) => e instanceof AdminConfigError && e.message.includes("built for admin major 2; this admin is major 1"));
  }));

  test(`check reports a package for another admin major as a build error (${provider})`, () => world(provider, async (w) => {
    const site = build(w, 2);
    assert.throws(() => site.check(), (e: Error) => e instanceof AdminConfigError && e.message.includes("built for admin major 2; this admin is major 1"));
  }));

  test(`a package that is added twice or names a missing file is refused (${provider})`, () => world(provider, async (w) => {
    const site = build(w);
    assert.throws(() => site.package("rating", w.pkg), (e: Error) => e instanceof AdminConfigError && e.message.includes("added twice"));
    const bad = new Admin(w.registry);
    bad.register(w.models["Role"]);
    bad.package("rating", w.pkg, { widgets: { stars: "missing.js" } });
    assert.throws(() => bad.permissions, (e: Error) => e instanceof AdminConfigError && e.message.includes("missing.js"));
  }));

  test(`a detail tab of no package is a build error (${provider})`, () => world(provider, async (w) => {
    const site = new Admin(w.registry);
    site.register(w.models["Role"], { detailTabs: ["rating.nothing"] });
    site.package("rating", w.pkg, { tabs: { reviews: "lib/reviews.js" } });
    assert.throws(() => site.permissions, (e: Error) => e instanceof AdminConfigError && e.message.includes("rating.nothing"));
  }));

  test(`a widget of no package is a build error (${provider})`, () => world(provider, async (w) => {
    const site = new Admin(w.registry, { widgets: { "Role.name": "rating.nothing" } });
    site.register(w.models["Role"]);
    site.package("rating", w.pkg, { widgets: { stars: "stars.js" } });
    assert.throws(() => site.permissions, (e: Error) => e instanceof AdminConfigError && e.message.includes("rating.nothing"));
  }));
}
