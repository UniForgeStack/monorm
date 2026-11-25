// Admin inlines: a to-many relation edited in place on the page of its parent, through the bridge, on PostgreSQL and SQLite.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, AdminConfigError, inline, type ResourceConfig } from "../src/admin.js";
import { connect, load, registerFieldAdapter, Registry, type Database } from "../src/index.js";
import { ValidationError } from "../src/errors.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;

const SCHEMA = (provider: string) => `datasource db {
  provider = "${provider}"
}
import "app:admin" (prefix: "admin_")

model IOrder {
  id     Int          @id @default(autoincrement())
  number String       @unique @db.VarChar(10)
  status String       @default("open") @db.VarChar(10)
  lines  IOrderLine[]
  notes  INote[]
  @@map("i_orders")
}

model IOrderLine {
  id       Int    @id @default(autoincrement())
  order_id Int
  sku      String @db.VarChar(10)
  qty      Int    @default(1)
  kind     String @default("goods") @db.VarChar(10)
  order    IOrder @relation(fields: [order_id], references: [id], onDelete: Cascade)
  @@map("i_order_lines")
}

model INote {
  id       Int    @id @default(autoincrement())
  order_id Int
  body     String @db.VarChar(40)
  order    IOrder @relation(fields: [order_id], references: [id], onDelete: Cascade)
  @@map("i_notes")
}

model IAuthor {
  id      Int     @id @default(autoincrement())
  name    String  @db.VarChar(20)
  written IBook[] @relation("written")
  edited  IBook[] @relation("edited")
  @@map("i_authors")
}

model IBook {
  id        Int      @id @default(autoincrement())
  author_id Int
  editor_id Int?
  title     String   @db.VarChar(20)
  author    IAuthor  @relation("written", fields: [author_id], references: [id])
  editor    IAuthor? @relation("edited", fields: [editor_id], references: [id])
  @@map("i_books")
}
`;

const ORDER = "/admin/api/resources/IOrder";
const SU = { key: 1, is_superuser: true };

type Loose = any;
interface World {
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-inlines-"));
  writeFileSync(join(dir, "package.json"), "{}");
  writeFileSync(join(dir, "schema.prisma"), SCHEMA(provider));
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
    await models["IOrder"].objects.using(db).insertMany([{ number: "A1" }, { number: "A2" }]);
    await models["IOrderLine"].objects.using(db).insertMany([
      { orderId: 1, sku: "S1", qty: 1 },
      { orderId: 1, sku: "S2", qty: 2 },
      { orderId: 2, sku: "X", qty: 9 },
    ]);
    await models["INote"].objects.using(db).insertMany([{ orderId: 1, body: "first" }, { orderId: 2, body: "other" }]);
    await run({ registry, db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

const lines = async (w: World): Promise<unknown[][]> => {
  const line = w.models["IOrderLine"];
  return (await line.objects.using(w.db).orderBy(line.id)).map((r: Loose) => [r.id, r.orderId, r.sku, r.qty]);
};

const audit = async (w: World): Promise<Loose[]> => {
  const entry = w.models["AdminLogEntry"];
  return await entry.objects.using(w.db).orderBy(entry.id);
};

const order = async (w: World, id: number): Promise<Loose> => {
  const Order = w.models["IOrder"];
  return await Order.objects.using(w.db).filter(Order.id.eq(id)).first();
};

function siteOf(w: World, parent?: ResourceConfig, line: ResourceConfig = {}, note: ResourceConfig = {}): Admin {
  const site = new Admin(w.registry);
  site.register(w.models["IOrder"], parent ?? { inlines: [inline(w.models["IOrderLine"]), inline(w.models["INote"], { save: "row" })] });
  site.register(w.models["IOrderLine"], line);
  site.register(w.models["INote"], note);
  return site;
}

const upper = {
  validate: (value: string) => value.toUpperCase(),
  prepare: async (value: string) => value,
  decode: (stored: string) => stored,
};

const ORIGINAL = [[1, 1, "S1", 1], [2, 1, "S2", 2], [3, 2, "X", 9]];

for (const provider of PROVIDERS) {
  // -- the engine path --------------------------------------------------------------------------------------------

  test(`two relations to the parent and no via fail admin check and via names one (${provider})`, () => world(provider, async (w) => {
    const bare = new Admin(w.registry);
    bare.register(w.models["IAuthor"], { inlines: [inline(w.models["IBook"])] });
    bare.register(w.models["IBook"]);
    assert.throws(() => bare.check(), (e) => e instanceof AdminConfigError && /ambiguous/.test(e.message));
    const ok = new Admin(w.registry);
    ok.register(w.models["IAuthor"], { inlines: [inline(w.models["IBook"], { via: "written" })] });
    ok.register(w.models["IBook"]);
    ok.check();
  }));

  test(`with_parent saves creates updates and deletes with the parent and writes one audit row each (${provider})`, () => world(provider, async (w) => {
    const client = siteOf(w).testClient(w.db, { user: SU });
    const body = {
      set: { status: "paid" },
      loaded: { status: "open" },
      inlines: { lines: { create: [{ set: { sku: "N1", qty: 5 } }], update: [{ key: "1", set: { qty: 10 }, loaded: { qty: 1 } }], delete: ["2"] } },
    };
    const r = await client.patch(`${ORDER}/1`, { json: body });
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(await lines(w), [[1, 1, "S1", 10], [3, 2, "X", 9], [4, 1, "N1", 5]]);
    assert.equal((await order(w, 1)).status, "paid");
    const rows = await audit(w);
    assert.deepEqual(rows.map((e) => [e.action, e.resource]), [["delete", "IOrderLine"], ["update", "IOrderLine"], ["create", "IOrderLine"], ["update", "IOrder"]]);
    assert.equal(rows[1].changes.inline, "IOrder.lines");
  }));

  test(`one refused row refuses the whole change set and names the row (${provider})`, () => world(provider, async (w) => {
    const client = siteOf(w).testClient(w.db, { user: SU });
    const body = { set: { status: "paid" }, loaded: { status: "open" }, inlines: { lines: { create: [{ set: { sku: "ok" } }, { set: { qty: 2 } }] } } };
    const r = await client.patch(`${ORDER}/1`, { json: body });
    assert.equal(r.status, 422, r.text);
    assert.ok(Object.keys(r.json().fields).some((key) => key.startsWith("lines.new.1")), r.text);
    assert.deepEqual(await lines(w), ORIGINAL.slice(0, 3).map((row, i) => (i === 2 ? [3, 2, "X", 9] : row)));
    assert.equal((await order(w, 1)).status, "open");
    assert.deepEqual(await audit(w), []);
  }));

  test(`row mode saves each row alone through the page of the child and pins it to the parent (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["INote"];
    const client = siteOf(w).testClient(w.db, { user: SU });
    const r = await client.post(`${ORDER}/1/inlines/notes`, { json: { set: { body: "second" } } });
    assert.equal(r.status, 201, r.text);
    const made = await Note.objects.using(w.db).filter(Note.body.eq("second")).first();
    assert.equal(made.orderId, 1);
    const edited = await client.patch(`${ORDER}/1/inlines/notes/${made.id}`, { json: { set: { body: "edited" }, loaded: { body: "second" } } });
    assert.equal(edited.status, 200, edited.text);
    assert.equal((await client.patch(`${ORDER}/1/inlines/notes/2`, { json: { set: { body: "x" }, loaded: { body: "other" } } })).status, 404);
    assert.equal((await client.post(`${ORDER}/1/inlines/lines`, { json: { set: { sku: "q" } } })).status, 404);
    assert.equal((await client.delete(`${ORDER}/1/inlines/notes/${made.id}`)).status, 200);
    assert.deepEqual((await Note.objects.using(w.db).all()).map((n: Loose) => n.body).sort(), ["first", "other"]);
    assert.deepEqual((await audit(w)).map((e) => [e.action, e.resource]), [["create", "INote"], ["update", "INote"], ["delete", "INote"]]);
  }));

  test(`the row cap and the new parent rules hold on the server (${provider})`, () => world(provider, async (w) => {
    const client = siteOf(w, { inlines: [inline(w.models["IOrderLine"], { max: 3 })] }).testClient(w.db, { user: SU });
    let r = await client.patch(`${ORDER}/1`, { json: { set: {}, inlines: { lines: { create: [{ set: { sku: "a" } }, { set: { sku: "b" } }] } } } });
    assert.equal(r.status, 422, r.text);
    r = await client.post(ORDER, { json: { set: { number: "C3" }, inlines: { lines: { update: [{ key: "1", set: { qty: 1 } }] } } } });
    assert.equal(r.status, 400, r.text);
    r = await client.post(ORDER, { json: { set: { number: "C3" }, inlines: { lines: { create: [{ set: { sku: "K", qty: 2 } }] } } } });
    assert.equal(r.status, 201, r.text);
    assert.deepEqual((await lines(w)).at(-1), [4, 3, "K", 2]);
  }));

  // -- the page keys of a wrapper ---------------------------------------------------------------------------------

  test(`a wrapper queryset filters the rows on read and on inline writes (${provider})`, () => world(provider, async (w) => {
    const Line = w.models["IOrderLine"];
    await Line.objects.using(w.db).filter(Line.id.eq(2)).update({ kind: "fee" });
    const parent = { inlines: [inline(Line, { page: { queryset: Line.objects.filter(Line.kind.ne("fee")).prepare() } })] };
    const client = siteOf(w, parent).testClient(w.db, { user: SU });
    const detail = (await client.get(`${ORDER}/1`)).json();
    assert.deepEqual(detail.inlines.lines.rows.map((r: Loose) => r.key), ["1"]);
    const r = await client.patch(`${ORDER}/1`, { json: { set: {}, inlines: { lines: { update: [{ key: "2", set: { qty: 5 }, loaded: { qty: 2 } }] } } } });
    assert.equal(r.status, 404, r.text);
    assert.deepEqual(await lines(w), ORIGINAL);
  }));

  test(`a wrapper overrides a page key and a child without a page takes the config of the wrapper (${provider})`, () => world(provider, async (w) => {
    const site = new Admin(w.registry);
    site.register(w.models["IOrder"], { inlines: [inline(w.models["IOrderLine"], { page: { readonly: ["qty"] } })] });
    const client = site.testClient(w.db, { user: SU });
    const meta = (await client.get("/admin/api/resources")).json().resources[0].inlines[0].form;
    assert.deepEqual(Object.fromEntries(meta.map((f: Loose) => [f.name, f.readonly])), { id: false, sku: false, qty: true, kind: false });
    const r = await client.patch(`${ORDER}/1`, { json: { set: {}, inlines: { lines: { update: [{ key: "1", set: { qty: 4 }, loaded: { qty: 1 } }] } } } });
    assert.equal(r.status, 422, r.text);
    assert.deepEqual(await lines(w), ORIGINAL);
  }));

  test(`a page key that cannot be shared is refused by name and a wrong wrapper is a config error (${provider})`, () => world(provider, async (w) => {
    for (const key of ["create", "nope"]) {
      const site = new Admin(w.registry);
      site.register(w.models["IOrder"], { inlines: [inline(w.models["IOrderLine"], { page: { [key]: 1 } as Loose })] });
      assert.throws(() => site.check(), (e) => e instanceof AdminConfigError && e.message.includes(key), key);
    }
    assert.throws(() => inline(w.models["IOrderLine"], { save: "later" as Loose }), (e) => e instanceof AdminConfigError && /save/.test(e.message));
    assert.throws(() => inline(w.models["IOrderLine"], { style: "grid" as Loose }), (e) => e instanceof AdminConfigError && /style/.test(e.message));
  }));

  // -- permissions ------------------------------------------------------------------------------------------------

  test(`an own permission names the write and an inline row links to its page when the user can view it (${provider})`, () => world(provider, async (w) => {
    const site = siteOf(w, { inlines: [inline(w.models["IOrderLine"], { permission: "edit_lines" })] });
    assert.ok(site.permissions.includes("admin.edit_lines"));
    const detail = (await site.testClient(w.db, { user: SU }).get(`${ORDER}/1`)).json();
    assert.ok(detail.inlines.lines.link);
    assert.notEqual(detail.inlines.lines.rows[0].flags, null);
  }));

  test(`a row write needs the parent to take an update its allow and its change rule (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["INote"];
    const Order = w.models["IOrder"];
    const inlines = [inline(Note, { save: "row" })];
    let client = siteOf(w, { allow: ["read"], inlines }).testClient(w.db, { user: SU });
    assert.equal((await client.post(`${ORDER}/1/inlines/notes`, { json: { set: { body: "x" } } })).status, 403);
    assert.equal((await client.patch(`${ORDER}/1/inlines/notes/1`, { json: { set: { body: "x" }, loaded: { body: "first" } } })).status, 403);
    assert.equal((await client.delete(`${ORDER}/1/inlines/notes/1`)).status, 403);
    assert.deepEqual((await Note.objects.using(w.db).all()).map((n: Loose) => n.body).sort(), ["first", "other"]);
    const meta = (await client.get("/admin/api/resources")).json().resources.find((r: Loose) => r.name === "IOrder");
    assert.deepEqual(meta.inlines[0].ops, []);
    await Order.objects.using(w.db).filter(Order.id.eq(1)).update({ status: "shipped" });
    client = siteOf(w, { can: { change: Order.status.ne("shipped") }, inlines }).testClient(w.db, { user: SU });
    assert.equal((await client.post(`${ORDER}/1/inlines/notes`, { json: { set: { body: "x" } } })).status, 403);
    assert.equal((await client.post(`${ORDER}/2/inlines/notes`, { json: { set: { body: "ok" } } })).status, 201);
    assert.deepEqual((await audit(w)).map((e) => e.action), ["permission_denied", "permission_denied", "permission_denied", "permission_denied", "create"]);
  }));

  // -- host behavior and services ---------------------------------------------------------------------------------

  test(`a parent with host behavior needs row mode and a child with host behavior saves through the bridge (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["INote"];
    registerFieldAdapter(w.models["IOrder"]._meta, "status", upper);
    assert.throws(() => siteOf(w).check(), (e) => e instanceof AdminConfigError && /host behavior and no service/.test(e.message));
    registerFieldAdapter(Note._meta, "body", upper);
    const client = siteOf(w, { inlines: [inline(Note, { save: "row" })] }).testClient(w.db, { user: SU });
    const r = await client.post(`${ORDER}/1/inlines/notes`, { json: { set: { body: "loud" } } });
    assert.equal(r.status, 201, r.text);
    const made = await Note.objects.using(w.db).filter(Note.orderId.eq(1), Note.body.eq("LOUD")).first();
    assert.notEqual(made, null);
    const edited = await client.patch(`${ORDER}/1/inlines/notes/${made.id}`, { json: { set: { body: "quiet" }, loaded: { body: "LOUD" } } });
    assert.equal(edited.status, 200, edited.text);
    assert.equal((await Note.objects.using(w.db).filter(Note.id.eq(made.id)).first()).body, "QUIET");
    assert.equal((await client.patch(`${ORDER}/1/inlines/notes/2`, { json: { set: { body: "x" }, loaded: { body: "other" } } })).status, 404);
  }));

  test(`a host child row write keeps the registered queryset when the wrapper queryset is wider (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["INote"];
    registerFieldAdapter(Note._meta, "body", upper);
    await Note.objects.using(w.db).insertMany([{ orderId: 1, body: "HIDDEN" }]);
    const hidden = await Note.objects.using(w.db).filter(Note.body.eq("HIDDEN")).first();
    const wide = Note.objects.filter(Note.id.gt(0)).prepare();
    const parent = { inlines: [inline(Note, { save: "row", page: { queryset: wide } })] };
    const client = siteOf(w, parent, {}, { queryset: Note.objects.filter(Note.body.ne("HIDDEN")).prepare() }).testClient(w.db, { user: SU });
    const r = await client.patch(`${ORDER}/1/inlines/notes/${hidden.id}`, { json: { set: { body: "seen" }, loaded: { body: "HIDDEN" } } });
    assert.equal(r.status, 404, r.text);
    assert.equal((await client.delete(`${ORDER}/1/inlines/notes/${hidden.id}`)).status, 404);
    assert.equal((await Note.objects.using(w.db).filter(Note.id.eq(hidden.id)).first()).body, "HIDDEN");
  }));

  test(`a host child row write keeps the registered can rule when the wrapper overrides it (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["INote"];
    registerFieldAdapter(Note._meta, "body", upper);
    await Note.objects.using(w.db).insertMany([{ orderId: 1, body: "LOCKED" }]);
    const locked = await Note.objects.using(w.db).filter(Note.body.eq("LOCKED")).first();
    const parent = { inlines: [inline(Note, { save: "row", page: { can: { change: Note.id.gt(0), delete: Note.id.gt(0) } } })] };
    const client = siteOf(w, parent, {}, { can: { change: Note.body.ne("LOCKED"), delete: Note.body.ne("LOCKED") } }).testClient(w.db, { user: SU });
    const r = await client.patch(`${ORDER}/1/inlines/notes/${locked.id}`, { json: { set: { body: "seen" }, loaded: { body: "LOCKED" } } });
    assert.equal(r.status, 404, r.text);
    assert.equal((await client.delete(`${ORDER}/1/inlines/notes/${locked.id}`)).status, 404);
    assert.equal((await Note.objects.using(w.db).filter(Note.id.eq(locked.id)).first()).body, "LOCKED");
  }));

  test(`a wrapper computed and search override get their own tagged host functions and leave the page of the child alone (${provider})`, () => world(provider, async (w) => {
    const Line = w.models["IOrderLine"];
    const calls = { page: 0, inline: 0, pageSearch: 0, inlineSearch: 0 };
    const computed = (name: "page" | "inline") => [{ name: "doubled", type: "int", nullable: false, function: async (keys: readonly unknown[]) => { calls[name] += 1; return keys.map(() => 1); } }];
    const search = (name: "pageSearch" | "inlineSearch") => async () => { calls[name] += 1; return [1, 2, 3]; };
    const page = { list: { columns: ["sku", "doubled"], computed: computed("page"), searchWith: search("pageSearch") } } as ResourceConfig;
    const wrapper = { list: { columns: ["sku", "doubled"], computed: computed("inline"), searchWith: search("inlineSearch") } } as ResourceConfig;
    const client = siteOf(w, { inlines: [inline(Line, { page: wrapper })] }, page).testClient(w.db, { user: SU });
    assert.equal((await client.get("/admin/api/resources/IOrderLine?q=S")).status, 200);
    assert.deepEqual([calls.page, calls.pageSearch], [1, 1]);
    assert.equal((await client.get(`${ORDER}/1`)).status, 200);
    assert.deepEqual([calls.inline, calls.inlineSearch], [0, 0]);
  }));

  test(`a service of the parent reads the checked rows from ctx inlines (${provider})`, () => world(provider, async (w) => {
    const Line = w.models["IOrderLine"];
    const seen: Record<string, unknown> = {};
    const config: ResourceConfig = {
      async update(ctx, _qs, changes) {
        const rows = ctx.inlines["lines"]!;
        seen["lines"] = { model: rows.model._meta.name, create: rows.create, update: rows.update.map((u) => [u.key, u.set]), delete: rows.delete.map((d) => d.key) };
        if (rows.create.some((c) => c["sku"] === "bad")) throw new ValidationError({ status: "No." });
        await ctx.database!.transaction(async () => {
          for (const created of rows.create) await Line.objects.using(ctx.database!).insert({ orderId: 1, ...created });
          await ctx.audit(undefined, { changes: { rows: rows.create.length, set: Object.keys(changes).length } });
        });
      },
      inlines: [inline(Line)],
    };
    const client = siteOf(w, config).testClient(w.db, { user: SU });
    const body = { set: {}, inlines: { lines: { create: [{ set: { sku: "N" } }], update: [{ key: "1", set: { qty: 4 }, loaded: { qty: 1 } }], delete: ["2"] } } };
    const r = await client.patch(`${ORDER}/1`, { json: body });
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(seen["lines"], { model: "IOrderLine", create: [{ sku: "N" }], update: [[{ id: 1 }, { qty: 4 }]], delete: [{ id: 2 }] });
    assert.deepEqual((await lines(w)).map((row) => row[2]), ["S1", "S2", "X", "N"]);
    assert.deepEqual((await audit(w)).map((e) => [e.action, e.resource]), [["update", "IOrder"]]);
  }));
}
