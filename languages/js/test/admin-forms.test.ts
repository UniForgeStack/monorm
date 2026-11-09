// Admin form layout and widgets: sections, read-only fields, widgets and the custom form module, on PostgreSQL and SQLite.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, AdminConfigError, type AdminOptions, type ResourceConfig } from "../src/admin.js";
import { hasExtension } from "./artifact-caps.js";
import { connect, load, Registry, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;

const SCHEMA = (provider: string) => `datasource db {
  provider = "${provider}"
}
import "app:admin" (prefix: "admin_")

model FBook {
  id    Int     @id @default(autoincrement())
  title String  @db.VarChar(20)
  blurb String?
  pages Int     @default(0)
  meta  Json?
  @@map("f_books")
}
`;

const GUARDED = (provider: string) => `datasource db {
  provider = "${provider}"
}
import "app:admin" (prefix: "admin_")

model FGuard {
  id    Int    @id @default(autoincrement())
  title String @db.VarChar(20)
  @@locking.version
  @@map("f_guards")
}
`;

const SU = { key: 1, is_superuser: true };
const BOOKS = "/admin/api/resources/FBook";

type Loose = any;
interface World {
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
  readonly dir: string;
}

async function open(provider: (typeof PROVIDERS)[number], schema: string, run: (w: World) => Promise<void>, seed: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-forms-"));
  writeFileSync(join(dir, "package.json"), "{}");
  writeFileSync(join(dir, "schema.prisma"), schema);
  const registry = new Registry();
  const models = load(join(dir, "schema.prisma"), { registry }) as unknown as Record<string, Loose>;
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
    const w = { registry, db, models, dir };
    await seed(w);
    await run(w);
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

const world = (provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>) =>
  open(provider, SCHEMA(provider), run, async (w) => {
    await w.models["FBook"].objects.using(w.db).insertMany([{ title: "First", pages: 10 }]);
  });

function siteOf(w: World, config: ResourceConfig, options: AdminOptions = {}): Admin {
  const site = new Admin(w.registry, options);
  site.register(w.models["FBook"], config);
  return site;
}

const formOf = (meta: Loose) => {
  const resource = meta.resources.find((r: Loose) => r.name === "FBook");
  return { resource, fields: Object.fromEntries(resource.form.map((f: Loose) => [f.name, f])) as Record<string, Loose> };
};

for (const provider of PROVIDERS) {
  test(`sections and widgets reach the form metadata (${provider})`, () => world(provider, async (w) => {
    const config: ResourceConfig = {
      form: { create: [{ title: "Main", fields: ["title", "pages"] }], edit: [{ title: "Main", fields: ["title"] }, { title: "More", fields: ["meta"], collapsed: true }] },
      widgets: { title: "url" },
    };
    const meta = (await siteOf(w, config, { widgets: { String: "email" } }).testClient(w.db, { user: SU }).get("/admin/api/resources")).json();
    const { resource, fields } = formOf(meta);
    assert.deepEqual(resource.sections.create[0], { title: "Main", fields: ["title", "pages"], collapsed: false });
    assert.ok(resource.sections.create[1].title === null && resource.sections.create[1].fields.includes("blurb"));
    assert.equal(resource.sections.edit[1].collapsed, true);
    assert.equal(fields["title"].widget, "url", "a field key beats a type key");
    assert.equal(fields["blurb"].widget, "email", "a site type key applies to a field of that type");
    assert.equal(fields["pages"].widget, "number");
    assert.equal(resource.form_component, false);
  }));

  test(`a read-only field is a 422 on create and on update and nothing is written (${provider})`, () => world(provider, async (w) => {
    const client = siteOf(w, { readonly: ["pages"] }).testClient(w.db, { user: SU });
    const { fields } = formOf((await client.get("/admin/api/resources")).json());
    assert.ok(fields["pages"].readonly === true && fields["title"].readonly === false);
    let r = await client.post(BOOKS, { json: { set: { title: "T", pages: 5 } } });
    assert.deepEqual([r.status, r.json().fields.pages], [422, ["This field is read-only."]], r.text);
    r = await client.patch(`${BOOKS}/1`, { json: { set: { pages: 5 }, loaded: { pages: 10 } } });
    assert.deepEqual([r.status, r.json().fields.pages], [422, ["This field is read-only."]], r.text);
    assert.equal((await w.models["FBook"].objects.using(w.db).first()).pages, 10);
    assert.equal((await client.post(BOOKS, { json: { set: { title: "T" } } })).status, 201);
  }));

  test(`a bad form config fails the build (${provider})`, () => world(provider, async (w) => {
    const cases: [ResourceConfig, string][] = [
      [{ form: [{ fields: ["nope"] }] }, "is not a field of the form"],
      [{ form: [{ fields: ["title"] }, { fields: ["title"] }] }, "is in two places"],
      [{ form: [{ fields: ["title"], collapsed: true }] }, "a collapsed section needs a title"],
      [{ widgets: { title: "slider" } }, "not one of"],
      [{ widgets: { pages: "checkbox" } }, "does not fit"],
      [{ readonly: ["title"] }, "read-only and required with no default"],
      [{ formComponent: "/no/such/file.js" }, "form_component"],
    ];
    for (const [config, match] of cases) {
      assert.throws(() => siteOf(w, config).check(), (e: unknown) => e instanceof AdminConfigError && e.message.includes(match), JSON.stringify(config));
    }
  }));

  test(`a config of the wrong shape is refused before the build (${provider})`, () => world(provider, async (w) => {
    const bad: Loose[] = [{ form: "x" }, { form: [{ title: "T" }] }, { form: { create: { fields: ["title"] } } }, { readonly: "pages" }, { widgets: { title: 1 } }];
    for (const config of bad) {
      assert.throws(() => (siteOf(w, config) as Loose).check(), AdminConfigError, JSON.stringify(config));
    }
  }));

  test(`the form module comes back to staff as javascript (${provider})`, () => world(provider, async (w) => {
    const module = join(w.dir, "form.js");
    writeFileSync(module, "export default (React) => function Form() { return null; };");
    const site = siteOf(w, { formComponent: module });
    assert.equal(formOf((await site.testClient(w.db, { user: SU }).get("/admin/api/resources")).json()).resource.form_component, true);
    const r = await site.testClient(w.db, { user: SU }).get("/admin/api/forms/FBook");
    assert.ok(r.status === 200 && r.text.includes("export default"), r.text);
    assert.equal((await site.testClient(w.db).get("/admin/api/forms/FBook")).status, 401);
    assert.equal((await siteOf(w, {}).testClient(w.db, { user: SU }).get("/admin/api/forms/FBook")).status, 404);
  }));

  test(`a read-only guard field is still compared and never written (${provider})`, { skip: !hasExtension("monorm.locking") }, () =>
    open(provider, GUARDED(provider), async (w) => {
      const site = new Admin(w.registry);
      site.register(w.models["FGuard"], { readonly: ["version"] });
      const client = site.testClient(w.db, { user: SU });
      const url = "/admin/api/resources/FGuard/1";
      let r = await client.patch(url, { json: { set: { version: 9 }, loaded: { version: 0 } } });
      assert.deepEqual([r.status, r.json().fields.version], [422, ["This field is read-only."]], r.text);
      r = await client.patch(url, { json: { set: { title: "Two" }, loaded: { title: "One", version: 5 } } });
      assert.equal(r.status, 409, r.text);
      assert.deepEqual(r.json().conflicts.version, { loaded: 5, current: 0 });
      r = await client.patch(url, { json: { set: { title: "Two" }, loaded: { title: "One", version: 0 } } });
      assert.deepEqual([r.status, r.json().changed], [200, ["title"]], r.text);
      const row = await w.models["FGuard"].objects.using(w.db).first();
      assert.deepEqual([row.title, row.version], ["Two", 1]);
    }, async (w) => {
      await w.models["FGuard"].objects.using(w.db).insertMany([{ title: "One" }]);
    }));
}
