/** Field permissions `see` and `change`: a hidden field leaves every path, through `admin.testClient`, on PostgreSQL and SQLite. */

import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Operator } from "opendal";

import { Admin, AdminConfigError, inline } from "../src/admin.js";
import { UserMethods, UserQueries, makePassword } from "../src/auth/index.js";
import { hasExtension } from "./artifact-caps.js";
import { connect, load, Registry, useModelMethods, useQuerySet, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;
const SCHEMA = (provider: string) => `datasource db {
  provider = "${provider}"
}
import "app:auth" (prefix: "auth_")
import "app:admin" (prefix: "admin_")

model FpSupplier {
  id       Int         @id @default(autoincrement())
  name     String      @db.VarChar(40)
  iban     String      @db.VarChar(40)
  logo     Json?
  products FpProduct[]
  @@map("fp_suppliers")
}

model FpProduct {
  id          Int        @id @default(autoincrement())
  supplier_id Int
  name        String     @db.VarChar(40)
  code        String     @db.VarChar(20)
  price       Int
  cost        Int
  supplier    FpSupplier @relation(fields: [supplier_id], references: [id])
  notes       FpNote[]
  @@index([name])
  @@index([code])
  @@map("fp_products")
}

model FpNote {
  id         Int       @id @default(autoincrement())
  product_id Int
  body       String    @db.VarChar(40)
  internal   String    @db.VarChar(40)
  product    FpProduct @relation(fields: [product_id], references: [id], onDelete: Cascade)
  @@map("fp_notes")
}
`;
const API = "/admin/api";
const PRODUCTS = `${API}/resources/FpProduct`;
const BASE = ["admin.access", "admin.manage_fpproduct", "admin.manage_fpnote", "admin.manage_fpsupplier"];
const COST = ["admin.see_cost"];
const EVERY = ["admin.see_cost", "admin.see_code", "admin.change_price", "admin.see_internal", "admin.see_supplier_name", "admin.see_logo"];

type Loose = any;
interface World {
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}
type Clients = Record<string, Loose>;

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-field-permissions-"));
  writeFileSync(join(dir, "package.json"), "{}");
  writeFileSync(join(dir, "schema.prisma"), SCHEMA(provider));
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
    const using = (name: string): Loose => models[name].objects.using(db);
    const password = makePassword("pw");
    await using("User").insertMany(["ann", "bob", "cy", "dee"].map((name) => ({ username: name, email: `${name}@example.com`, password })));
    await using("FpSupplier").insertMany([{ name: "Acme", iban: "DE01" }, { name: "Bolt", iban: "DE02" }]);
    await using("FpProduct").insertMany([
      { supplierId: 1, name: "lamp", code: "L-1", price: 20, cost: 7 },
      { supplierId: 2, name: "desk", code: "D-1", price: 90, cost: 40 },
      { supplierId: 1, name: "chair", code: "C-1", price: 50, cost: 21 },
    ]);
    await using("FpNote").insertMany([{ productId: 1, body: "fragile", internal: "margin 65%" }]);
    await run({ registry, db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

const files = hasExtension("monorm.file-storage");
if (files) {
  // A variable specifier keeps tsc out of the staged (generated) extension sources.
  const specifier = "../../extensions/file-storage/index.js";
  (globalThis as Loose).__fileStorage = ((await import(specifier)) as Loose).Registry;
}

function siteOf(w: World, { save = "row", withFiles = false }: { save?: "row" | "with_parent"; withFiles?: boolean } = {}): Admin {
  const storage = withFiles ? { storage: new (globalThis as Loose).__fileStorage(new Map([["media", new Operator("memory")]])) } : {};
  const site = new Admin(w.registry, storage as never);
  const p = w.models["FpProduct"];
  const s = w.models["FpSupplier"];
  site.register(p, {
    list: {
      columns: [p.name, p.code, p.price, p.cost],
      order: [p.id],
      search: [p.name, p.code],
      filters: [p.code, p.cost, p.price],
      sortable: [p.name, p.cost, p.price],
    },
    see: { cost: "see_cost", code: "see_code" },
    change: { price: "change_price" },
    relations: { supplier: {} },
    inlines: [inline(w.models["FpNote"], { save })],
  } as never);
  site.register(w.models["FpNote"], { see: { internal: "see_internal" } });
  site.register(s, { list: { columns: [s.name, s.iban], order: [s.id], search: [s.name] }, see: { name: "see_supplier_name", logo: "see_logo" }, ...(withFiles ? { files: { logo: "media" } } : {}) } as never);
  site.register(w.models["Role"]);
  site.register(w.models["Permission"], { allow: ["read"] });
  return site;
}

async function grant(w: World, username: string, names: string[]): Promise<unknown> {
  const rows = (await w.models["Permission"].objects.using(w.db)).filter((r: Loose) => names.includes(`${r.label}.${r.codename}`));
  assert.equal(rows.length, names.length, names.join());
  const Role = w.models["Role"];
  await Role.objects.using(w.db).insertMany([{ name: `for-${username}` }]);
  const role = await Role.objects.using(w.db).filter(Role.name.eq(`for-${username}`)).first();
  const User = w.models["User"];
  const user = await User.objects.using(w.db).filter(User.username.eq(username)).first();
  await w.models["RolePermission"].objects.using(w.db).insertMany(rows.map((p: Loose) => ({ roleId: role.id, permissionId: p.id })));
  await w.models["UserRole"].objects.using(w.db).insertMany([{ userId: user.id, roleId: role.id }]);
  return user.id;
}

/** ann holds the page names only; bob holds every field name; cy holds `see_cost` only; dee holds `see_supplier_name` only. */
async function users(w: World, site: Admin): Promise<Clients> {
  await site.syncPermissions(w.db);
  const given = { ann: BASE, bob: [...BASE, ...EVERY], cy: [...BASE, ...COST], dee: [...BASE, "admin.see_supplier_name"] };
  const clients: Loose = {};
  for (const [name, names] of Object.entries(given)) clients[name] = site.testClient(w.db, { user: { key: await grant(w, name, names) } });
  return clients;
}

const labels = (columns: Loose[]): string[] => columns.map((c) => c.label);
const names = (rows: Loose[][]): unknown[] => rows.map((r) => r[0]);
const csvHeader = (reply: Loose): string[] => {
  assert.equal(reply.status, 200, reply.text);
  return reply.text.split(/\r?\n/)[0].split(",");
};

for (const provider of PROVIDERS) {
  test(`a name binds a field and an unbound field follows the page (${provider})`, () => world(provider, async (w) => {
    const site = siteOf(w);
    const c = await users(w, site);
    for (const name of ["admin.see_cost", "admin.see_code", "admin.change_price", "admin.see_internal", "admin.see_supplier_name"]) assert.ok(site.permissions.includes(name), name);
    const [ann, cy, bob] = await Promise.all(["ann", "cy", "bob"].map(async (u) => (await c[u].get(PRODUCTS)).json()));
    assert.deepEqual(labels(ann.columns).slice(0, 3), ["name", "price", "id"]);
    assert.deepEqual(labels(cy.columns).slice(0, 3), ["name", "price", "cost"]);
    assert.deepEqual(labels(bob.columns).slice(0, 4), ["name", "code", "price", "cost"]);
    assert.deepEqual(ann.rows[0].slice(0, 2), ["lamp", 20]);
    assert.equal(ann.rows[0].length, 3);
    assert.deepEqual(cy.rows.map((r: Loose[]) => r[2]), [7, 40, 21]);
    assert.deepEqual(bob.rows[1].slice(0, 4), ["desk", "D-1", 90, 40]);
  }));

  test(`the metadata leaves out a field the user cannot see (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    const meta: Loose = {};
    for (const u of ["ann", "bob"]) meta[u] = Object.fromEntries((await c[u].get(`${API}/resources`)).json().resources.map((r: Loose) => [r.name, r]));
    const ann = meta.ann.FpProduct;
    const bob = meta.bob.FpProduct;
    const keys = (items: Loose[]): string[] => items.map((x) => x.label ?? x.name);
    for (const part of ["columns", "fields", "form"]) {
      assert.ok(!keys(ann[part]).some((k) => k === "cost" || k === "code"), part);
      assert.ok(keys(bob[part]).includes("cost") && keys(bob[part]).includes("code"), part);
    }
    assert.deepEqual(ann.filters.map((f: Loose) => f.name), ["price"]);
    assert.deepEqual(bob.filters.map((f: Loose) => f.name).sort(), ["code", "cost", "price"]);
    assert.ok(!JSON.stringify(meta.ann.FpNote).includes("internal") && JSON.stringify(meta.bob.FpNote).includes("internal"));
    assert.ok(JSON.stringify(meta.bob.FpSupplier).includes("name") && JSON.stringify(meta.ann.FpSupplier).includes("iban"));
    assert.equal(ann.form.find((f: Loose) => f.name === "price").readonly, true);
    assert.equal(bob.form.find((f: Loose) => f.name === "price").readonly, false);
  }));

  test(`the detail page leaves out a hidden field (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    const [ann, cy, bob] = await Promise.all(["ann", "cy", "bob"].map(async (u) => (await c[u].get(`${PRODUCTS}/1`)).json()));
    assert.deepEqual(labels(ann.fields), ["id", "supplier_id", "name", "price"]);
    assert.deepEqual(ann.values, [1, 1, "lamp", 20]);
    assert.deepEqual(cy.values, [1, 1, "lamp", 20, 7]);
    assert.deepEqual(bob.values, [1, 1, "lamp", "L-1", 20, 7]);
    assert.ok(!JSON.stringify(ann).includes("L-1") && !JSON.stringify(cy).includes("L-1"));
  }));

  test(`search matches only a field the user can see (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    assert.deepEqual(names((await c.bob.get(`${PRODUCTS}?q=L-1`)).json().rows), ["lamp"]);
    assert.deepEqual((await c.ann.get(`${PRODUCTS}?q=L-1`)).json().rows, []);
    assert.deepEqual(names((await c.ann.get(`${PRODUCTS}?q=lamp`)).json().rows), ["lamp"]);
  }));

  test(`a filter or a sort on a hidden field is an error (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    assert.deepEqual(names((await c.ann.get(`${PRODUCTS}?f.price=50`)).json().rows), ["chair"]);
    for (const hidden of [`${PRODUCTS}?f.cost=7`, `${PRODUCTS}?f.code=L-1`, `${PRODUCTS}?sort=cost`, `${API}/counts/FpProduct?f.cost=7`, `${API}/export/FpProduct?f.cost=7`]) {
      assert.equal((await c.ann.get(hidden)).status, 400, hidden);
    }
    assert.equal((await c.ann.get(`${PRODUCTS}?sort=-price`)).status, 200);
    assert.deepEqual(names((await c.cy.get(`${PRODUCTS}?f.cost=7`)).json().rows), ["lamp"]);
    assert.deepEqual(names((await c.cy.get(`${PRODUCTS}?sort=-cost`)).json().rows), ["desk", "chair", "lamp"]);
    assert.equal((await c.cy.get(`${PRODUCTS}?f.code=L-1`)).status, 400);
  }));

  test(`the export has no hidden column (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    const exp = `${API}/export/FpProduct`;
    assert.deepEqual(csvHeader(await c.ann.get(exp)), ["Name", "Price", "Id"]);
    assert.deepEqual(csvHeader(await c.cy.get(exp)).slice(0, 3), ["Name", "Price", "Cost"]);
    assert.deepEqual(csvHeader(await c.bob.get(exp)).slice(0, 4), ["Name", "Code", "Price", "Cost"]);
    assert.ok(!(await c.ann.get(exp)).text.includes("L-1") && (await c.bob.get(exp)).text.includes("L-1"));
  }));

  test(`a picker label that the user cannot see falls back to the key (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    const url = `${API}/autocomplete/FpProduct/supplier`;
    assert.deepEqual((await c.bob.get(url)).json().items.map((i: Loose) => i.label), ["Acme", "Bolt"]);
    const ann = await c.ann.get(url);
    assert.deepEqual(ann.json().items.map((i: Loose) => i.label), [1, 2]);
    assert.ok(!ann.text.includes("Acme"));
    assert.equal((await c.ann.get(`${url}?q=Acme`)).status, 400, "the label column is the only search field and ann cannot see it");
  }));

  test(`a tab and an inline leave out a hidden field (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    const tab = `${PRODUCTS}/1/notes`;
    const ann = (await c.ann.get(tab)).json();
    const bob = (await c.bob.get(tab)).json();
    assert.deepEqual(labels(ann.columns), ["id", "product_id", "body"]);
    assert.deepEqual(ann.rows, [[1, 1, "fragile"]]);
    assert.equal(labels(bob.columns).at(-1), "internal");
    assert.deepEqual(bob.rows, [[1, 1, "fragile", "margin 65%"]]);
    const inlineValues = async (u: string): Promise<unknown> => (await c[u].get(`${PRODUCTS}/1`)).json().inlines.notes.rows[0].values;
    assert.deepEqual(await inlineValues("ann"), [1, 1, "fragile"]);
    assert.deepEqual(await inlineValues("bob"), [1, 1, "fragile", "margin 65%"]);
    assert.equal((await c.ann.get(`${API}/counts/FpProduct/1/notes`)).json().count, 1);
  }));

  test(`a write on a field the user cannot see or change is a 422 (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    const product = w.models["FpProduct"];
    const created = await c.ann.post(PRODUCTS, { json: { set: { supplier_id: 1, name: "rug", code: "R-1", price: 5, cost: 1 } } });
    assert.equal(created.status, 422, created.text);
    assert.deepEqual(Object.keys(created.json().fields).sort(), ["code", "cost", "price"]);
    const locked = await c.ann.patch(`${PRODUCTS}/1`, { json: { set: { price: 99 }, loaded: { price: 20 } } });
    assert.equal(locked.status, 422, locked.text);
    assert.deepEqual(locked.json().fields.price, ["This field is read-only."]);
    const unseen = await c.ann.patch(`${PRODUCTS}/1`, { json: { set: { cost: 1 }, loaded: { cost: 7 } } });
    assert.equal(unseen.status, 422, unseen.text);
    assert.ok("cost" in unseen.json().fields);
    const renamed = await c.ann.patch(`${PRODUCTS}/1`, { json: { set: { name: "lamp2" }, loaded: { name: "lamp" } } });
    assert.equal(renamed.status, 200, renamed.text);
    const allowed = await c.bob.patch(`${PRODUCTS}/1`, { json: { set: { price: 25, cost: 8 }, loaded: { price: 20, cost: 7 } } });
    assert.equal(allowed.status, 200, allowed.text);
    const row = await product.objects.using(w.db).filter(product.id.eq(1)).first();
    assert.deepEqual([row.name, row.price, row.cost], ["lamp2", 25, 8]);
  }));

  test(`the response of a write leaves out a hidden value (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    const changed = await c.ann.patch(`${PRODUCTS}/2`, { json: { set: { name: "table" }, loaded: { name: "desk" } } });
    assert.equal(changed.status, 200, changed.text);
    assert.ok(!changed.text.includes("D-1") && !String(changed.json().values ?? []).includes("40"));
    const made = await c.bob.post(PRODUCTS, { json: { set: { supplier_id: 1, name: "rug", code: "R-1", price: 5, cost: 1 } } });
    assert.ok([200, 201].includes(made.status) && made.text.includes("R-1"), made.text);
  }));

  test(`a resource without a bound name serves the page to everyone (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    const supplier = (await c.ann.get(`${API}/resources/FpSupplier`)).json();
    assert.deepEqual(labels(supplier.columns).slice(0, 1), ["iban"]);
    assert.ok(!JSON.stringify(supplier).includes("Acme"));
  }));

  test(`a bad binding is an admin config error (${provider})`, () => world(provider, async (w) => {
    const p = w.models["FpProduct"];
    const bad: [Loose, RegExp][] = [
      [{ see: { nope: "see_nope" } }, /nope/],
      [{ see: { id: "see_id" } }, /key/],
      [{ change: { nope: "change_nope" } }, /nope/],
      [{ see: { cost: "Bad Name" } }, /lowercase/],
      [{ see: { cost: 3 } }, /see/],
    ];
    for (const [config, pattern] of bad) {
      const candidate = new Admin(w.registry);
      candidate.register(p, config);
      assert.throws(() => candidate.check(), (e) => e instanceof AdminConfigError && pattern.test(e.message), JSON.stringify(config));
    }
    const nested = new Admin(w.registry);
    nested.register(p, { inlines: [inline(w.models["FpNote"], { page: { see: { internal: "see_internal" } } as Loose })] });
    assert.throws(() => nested.check(), (e) => e instanceof AdminConfigError && /registered page/.test(e.message));
    const strategy = new Admin(w.registry, { permissions: "access" });
    strategy.register(p, { see: { cost: "see_cost" } });
    assert.throws(() => strategy.check(), (e) => e instanceof AdminConfigError && /need the page or model/.test(e.message));
  }));
  test(`every other route leaks no hidden value to a user without the name (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    const ann = c.ann;
    const secrets = ["margin 65%", "L-1", "D-1", "C-1", "DE01", "DE02"];
    const open = [
      PRODUCTS, `${PRODUCTS}/1`, `${PRODUCTS}/1/notes`, `${PRODUCTS}?q=L-1`, `${API}/export/FpProduct`, `${API}/counts/FpProduct`, `${API}/counts/FpProduct/1/notes`,
      `${API}/links/FpProduct/1/supplier`, `${API}/autocomplete/FpProduct/supplier`, `${API}/forms/FpProduct`, `${API}/resources`,
    ];
    for (const uri of open) {
      const reply = await ann.get(uri);
      assert.ok(!secrets.some((secret) => reply.text.includes(secret)), uri);
    }
    for (const hidden of [`${PRODUCTS}/1/notes?f.internal=x`, `${PRODUCTS}/1/notes?sort=internal`, `${API}/counts/FpProduct/1/notes?f.internal=x`, `${API}/export/FpProduct?sort=cost`]) {
      assert.equal((await ann.get(hidden)).status, 400, hidden);
    }
    const stale = await ann.patch(`${PRODUCTS}/1`, { json: { set: { name: "x" }, loaded: { name: "stale" } } });
    assert.ok(stale.status === 409 && !secrets.some((secret) => stale.text.includes(secret)) && !stale.text.includes("cost"), stale.text);
    const note = await ann.post(`${PRODUCTS}/1/inlines/notes`, { json: { set: { body: "b", internal: "x" } } });
    assert.equal(note.status, 422, note.text);
  }));

  test(`a view belongs to a granted set, not to the first user of it (${provider})`, () => world(provider, async (w) => {
    await users(w, siteOf(w));
    for (const order of [["ann", "dee"], ["dee", "ann"]]) {
      const site = siteOf(w);
      const keys: Record<string, number> = { ann: 1, dee: 4 };
      const seen: Record<string, unknown> = {};
      for (const name of order) {
        const client = site.testClient(w.db, { user: { key: keys[name] } });
        seen[name] = (await client.get(`${API}/autocomplete/FpProduct/supplier`)).json().items.map((i: Loose) => i.label);
        assert.deepEqual(labels((await client.get(PRODUCTS)).json().columns), ["name", "price", "id"]);
      }
      assert.deepEqual(seen, { ann: [1, 2], dee: ["Acme", "Bolt"] }, order.join());
    }
  }));

  test(`a create that a hidden required field blocks names the field (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    const blocked = await c.ann.post(PRODUCTS, { json: { set: { supplier_id: 1, name: "rug" } } });
    assert.equal(blocked.status, 422, blocked.text);
    const fields = Object.keys(blocked.json().fields);
    assert.ok(fields.length > 0 && fields.every((f) => ["code", "cost", "price"].includes(f)), blocked.text);
    assert.equal(await w.models["FpProduct"].objects.using(w.db).count(), 3);
  }));

  test(`a bulk delete whose filter names a hidden field is a 400 and deletes nothing (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w));
    const del = `${API}/actions/FpProduct/bulk_delete`;
    const product = w.models["FpProduct"];
    const hidden = await c.ann.post(`${del}?f.cost=7`, { json: { all: true, inputs: {} } });
    assert.equal(hidden.status, 400, hidden.text);
    assert.equal(await product.objects.using(w.db).count(), 3);
    const shown = await c.cy.post(`${del}?f.cost=7`, { json: { all: true, inputs: {} } });
    assert.equal(shown.status, 200, shown.text);
    assert.deepEqual((await product.objects.using(w.db)).map((r: Loose) => r.name).sort(), ["chair", "desk"]);
    const visible = await c.ann.post(`${del}?f.price=50`, { json: { all: true, inputs: {} } });
    assert.equal(visible.status, 200, visible.text);
    assert.deepEqual((await product.objects.using(w.db)).map((r: Loose) => r.name), ["desk"]);
  }));

  test(`an inline written with its parent leaves out a hidden field (${provider})`, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w, { save: "with_parent" }));
    const notes = w.models["FpNote"];
    const product = w.models["FpProduct"];
    const body = { set: { name: "lamp2" }, loaded: { name: "lamp" }, inlines: { notes: { create: [{ set: { body: "b", internal: "x" } }] } } };
    const hidden = await c.ann.patch(`${PRODUCTS}/1`, { json: body });
    assert.equal(hidden.status, 422, hidden.text);
    assert.equal(await notes.objects.using(w.db).count(), 1);
    assert.equal((await product.objects.using(w.db).filter(product.id.eq(1)).first()).name, "lamp", "the failed change set is not stored");
    const allowed = await c.bob.patch(`${PRODUCTS}/1`, { json: body });
    assert.equal(allowed.status, 200, allowed.text);
    assert.deepEqual((await notes.objects.using(w.db)).map((r: Loose) => r.internal).sort(), ["margin 65%", "x"]);
  }));

  test(`a file field that the user cannot see takes no upload and gives no download (${provider})`, { skip: !files }, () => world(provider, async (w) => {
    const c = await users(w, siteOf(w, { withFiles: true }));
    const path = `${API}/files/FpSupplier/1/logo`;
    const up = await c.bob.post(path, { file: ["logo.png", "png-bytes", "image/png"] });
    assert.equal(up.status, 200, up.text);
    assert.equal(Buffer.from((await c.bob.get(path)).content).toString(), "png-bytes");
    assert.equal((await c.ann.get(path)).status, 404);
    assert.equal((await c.ann.post(path, { file: ["evil.png", "x", "image/png"] })).status, 404);
    assert.equal(Buffer.from((await c.bob.get(path)).content).toString(), "png-bytes", "the hidden upload changed nothing");
  }));
}
