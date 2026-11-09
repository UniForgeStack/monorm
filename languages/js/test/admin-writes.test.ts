// Admin plain writes: create, change and delete, the 409 and 422 answers, the read-only reason and the audit row, on PostgreSQL and SQLite.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, AdminConfigError } from "../src/admin.js";
import { hasExtension } from "./artifact-caps.js";
import { connect, load, param, Registry, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;

const SCHEMA = (provider: string) => `datasource db {
  provider = "${provider}"
}
import "app:admin" (prefix: "admin_")

enum WStatus {
  draft
  published
  ${provider === "sqlite" ? "@@storage(text)" : ""}
}

model WAuthor {
  id    Int     @id @default(autoincrement())
  email String  @unique
  name  String  @db.VarChar(10)
  books WBook[]
  @@map("w_authors")
}

model WBook {
  id        Int      @id @default(autoincrement())
  author_id Int
  title     String   @db.VarChar(20)
  pages     Int      @default(0) @check(raw("pages >= 0"))
  status    WStatus  @default(draft)
  meta      Json?
  author    WAuthor  @relation(fields: [author_id], references: [id], onDelete: Restrict)
  @@check(raw("title <> 'banned'"), name: "no_banned_title")
  @@map("w_books")
}

model WLocked {
  id Int @id @default(autoincrement())
  x  Int
  @@protected_write
  @@map("w_locked")
}
`;

const GUARDED = (provider: string) => `datasource db {
  provider = "${provider}"
}
import "app:admin" (prefix: "admin_")

model GBook {
  id    Int    @id @default(autoincrement())
  title String @db.VarChar(20)
  @@timestamps.updated_at("changed")
  @@locking.version
  @@map("g_books")
}
`;

const BOOKS = "/admin/api/resources/WBook";
const AUTHORS = "/admin/api/resources/WAuthor";
const SU = { key: 1, is_superuser: true };

type Loose = any;
interface World {
  readonly site: Admin;
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-writes-"));
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
    await models["WAuthor"].objects.using(db).insertMany([{ email: "a@x.io", name: "Ada" }, { email: "b@x.io", name: "Bo" }]);
    await models["WBook"].objects.using(db).insertMany([{ authorId: 1, title: "First", pages: 10 }]);
    const site = new Admin(registry);
    site.register(models["WAuthor"]);
    site.register(models["WBook"], { checkMessages: { no_banned_title: "That title is not allowed." } });
    site.register(models["WLocked"]);
    await run({ site, registry, db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

const audit = async (w: World): Promise<Loose[]> => {
  const entry = w.models["AdminLogEntry"];
  return await entry.objects.using(w.db).orderBy(entry.id);
};

for (const provider of PROVIDERS) {
  test(`a create stores the row and writes one audit row with the diff (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { user: SU });
    const body = { set: { author_id: 2, title: "Second", status: "published", meta: { a: [1] } } };
    const response = await client.post(BOOKS, { json: body });
    assert.equal(response.status, 201, response.text);
    assert.equal(response.json().key, "2");
    const Book = w.models["WBook"];
    const book = await Book.objects.using(w.db).filter(Book.id.eq(2)).first();
    assert.deepEqual([book.authorId, book.title, book.pages, String(book.status), book.meta], [2, "Second", 0, "published", { a: [1] }]);
    const rows = await audit(w);
    assert.equal(rows.length, 1);
    assert.deepEqual([rows[0].action, rows[0].resource, rows[0].recordKey, rows[0].recordLabel], ["create", "WBook", "2", "Second"]);
    const changes = rows[0].changes;
    assert.deepEqual(changes.diff.title, { from: null, to: "Second" });
    for (const k of ["request_id", "ip", "user_agent"]) assert.equal(typeof changes[k], "string");
  }));

  test(`a bad create is a 422 with one message per field and stores nothing (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { user: SU });
    const response = await client.post(BOOKS, { json: { set: { title: "x".repeat(21), pages: 1.5, status: "gone", meta: 1, nope: 1 } } });
    assert.equal(response.status, 422);
    const fields = response.json().fields;
    assert.deepEqual(fields.title, ["Must have at most 20 characters"]);
    assert.deepEqual(fields.pages, ["Must be a whole number"]);
    assert.deepEqual(fields.status, ["Must be one of the listed values"]);
    assert.deepEqual(fields.author_id, ["This field is required."]);
    assert.deepEqual(fields.nope, ["This field cannot be written."]);
    assert.equal(await w.models["WBook"].objects.using(w.db).count(), 1);
    assert.deepEqual(await audit(w), []);
  }));

  test(`database rules come back as field errors and a named check as a form message (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { user: SU });
    let r = await client.post(AUTHORS, { json: { set: { email: "a@x.io", name: "Cy" } } });
    assert.deepEqual([r.status, r.json().fields.email], [422, ["A record with this value already exists."]]);
    r = await client.post(BOOKS, { json: { set: { author_id: 99, title: "T" } } });
    assert.deepEqual([r.status, r.json().fields.author_id], [422, ["No such record."]]);
    r = await client.post(BOOKS, { json: { set: { author_id: 1, title: "T", pages: -1 } } });
    assert.deepEqual([r.status, r.json().fields.pages], [422, ["This value is not allowed."]]);
    r = await client.post(BOOKS, { json: { set: { author_id: 1, title: "banned" } } });
    assert.deepEqual([r.status, r.json().form], [422, ["That title is not allowed."]]);
    assert.deepEqual(await audit(w), []);
  }));

  test(`an update sends only the changed fields and the audit row holds the diff (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { user: SU });
    const r = await client.patch(`${BOOKS}/1`, { json: { set: { title: "Renamed", pages: 10 }, loaded: { title: "First", pages: 10 } } });
    assert.deepEqual([r.status, r.json().changed], [200, ["title"]], r.text);
    assert.ok(r.queries <= 8, r.sql.join("\n"));
    const Book = w.models["WBook"];
    assert.equal((await Book.objects.using(w.db).filter(Book.id.eq(1)).first()).title, "Renamed");
    assert.deepEqual((await audit(w))[0].changes.diff, { title: { from: "First", to: "Renamed" } });
    const again = await client.patch(`${BOOKS}/1`, { json: { set: { title: "Renamed" }, loaded: { title: "Renamed" } } });
    assert.deepEqual([again.status, again.json().changed], [200, []]);
    assert.equal((await audit(w)).length, 1);
  }));

  test(`a stale loaded value is a 409 with the current value and writes nothing (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { user: SU });
    const r = await client.patch(`${BOOKS}/1`, { json: { set: { title: "Mine" }, loaded: { title: "Old" } } });
    assert.equal(r.status, 409, r.text);
    assert.deepEqual(r.json().conflicts.title, { loaded: "Old", current: "First" });
    const Book = w.models["WBook"];
    assert.equal((await Book.objects.using(w.db).filter(Book.id.eq(1)).first()).title, "First");
    assert.deepEqual(await audit(w), []);
  }));

  test(`an update checks the request before it writes (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { user: SU });
    assert.equal((await client.patch(`${BOOKS}/1`, { json: { set: { title: "x" } } })).status, 400);
    const r = await client.patch(`${BOOKS}/1`, { json: { set: { id: 5 }, loaded: { id: 1 } } });
    assert.deepEqual([r.status, r.json().fields.id], [422, ["The key of a record cannot change."]]);
    assert.equal((await client.patch(`${BOOKS}/9`, { json: { set: {} } })).status, 404);
    assert.equal((await client.patch("/admin/api/resources/Nope/1", { json: { set: {} } })).status, 404);
    assert.equal((await client.patch(`${BOOKS}/1`, { json: [1] })).status, 400);
  }));

  test(`a delete removes the row and a referenced row is a form message (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { user: SU });
    let r = await client.delete(`${AUTHORS}/1`);
    assert.equal(r.status, 422, r.text);
    assert.ok(r.json().form[0].includes("still refer"));
    assert.equal(await w.models["WAuthor"].objects.using(w.db).count(), 2);
    r = await client.delete(`${BOOKS}/1`, { json: { loaded: { title: "Stale" } } });
    assert.deepEqual([r.status, r.json().conflicts.title.current], [409, "First"]);
    assert.equal((await client.delete(`${BOOKS}/1`, { json: { loaded: { title: "First" } } })).status, 200);
    assert.equal((await client.delete(`${BOOKS}/1`)).status, 404);
    assert.deepEqual((await audit(w)).map((e) => [e.action, e.recordKey, e.recordLabel]), [["delete", "1", "First"]]);
  }));

  test(`a protected model is read only with the reason and the metadata says so (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { user: SU });
    const r = await client.post("/admin/api/resources/WLocked", { json: { set: { x: 1 } } });
    assert.deepEqual([r.status, r.json().error.kind], [403, "ReadOnly"]);
    assert.ok(r.json().error.message.includes("@@protected_write"));
    const resources = Object.fromEntries((await client.get("/admin/api/resources")).json().resources.map((m: Loose) => [m.name, m]));
    assert.equal(resources["WLocked"].writable, false);
    assert.equal(resources["WBook"].writable, true);
    assert.equal(typeof resources["WLocked"].read_only_reason, "string");
  }));

  test(`an operation that the resource does not allow is a 403 with an audit row (${provider})`, () => world(provider, async (w) => {
    const site = new Admin(w.registry);
    site.register(w.models["WBook"], { allow: ["read", "create"] });
    const client = site.testClient(w.db, { user: SU });
    assert.equal((await client.patch(`${BOOKS}/1`, { json: { set: {} } })).status, 403);
    assert.equal((await client.delete(`${BOOKS}/1`)).status, 403);
    assert.deepEqual((await audit(w)).map((e) => e.action), ["permission_denied", "permission_denied"]);
  }));

  test(`a write stays inside the queryset and the can rule (${provider})`, () => world(provider, async (w) => {
    const Book = w.models["WBook"];
    await Book.objects.using(w.db).insertMany([{ id: 4, authorId: 1, title: "Four", pages: 4 }, { id: 5, authorId: 2, title: "Five", pages: 5 }]);
    const site = new Admin(w.registry);
    site.register(Book, {
      queryset: Book.objects.filter(Book.authorId.eq(param("user"))).prepare(),
      can: { change: Book.pages.gt(4), delete: Book.pages.gt(3) },
    });
    const client = site.testClient(w.db, { user: SU });
    const edit = (title: string, was: string) => ({ json: { set: { title }, loaded: { title: was } } });
    assert.equal((await client.patch(`${BOOKS}/5`, edit("x", "Five"))).status, 404, "outside the queryset");
    assert.equal((await client.patch(`${BOOKS}/4`, edit("x", "Four"))).status, 403, "the can rule");
    assert.equal((await client.delete(`${BOOKS}/4`)).status, 200);
  }));

  test(`a raw json value counts by what it stores (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { user: SU });
    const edit = (to: unknown, was: unknown) => client.patch(`${BOOKS}/1`, { json: { set: { pages: to }, loaded: { pages: was } } });
    let r = await edit("10", "10");
    assert.deepEqual([r.status, r.json().changed], [200, []], r.text);
    assert.deepEqual(await audit(w), []);
    r = await edit("11", "10");
    assert.deepEqual([r.status, r.json().changed], [200, ["pages"]], r.text);
    assert.deepEqual((await audit(w))[0].changes.diff, { pages: { from: 10, to: 11 } });
    r = await edit(12, "10");
    assert.equal(r.status, 409, r.text);
    assert.deepEqual(r.json().conflicts.pages, { loaded: "10", current: 11 });
  }));

  test(`a delete ignores a loaded key that is not a field (${provider})`, () => world(provider, async (w) => {
    const client = w.site.testClient(w.db, { user: SU });
    const r = await client.delete(`${AUTHORS}/2`, { json: { loaded: { nope: 1 } } });
    assert.equal(r.status, 200, r.text);
  }));

  test(`a check message for a missing check fails the build (${provider})`, () => world(provider, async (w) => {
    const site = new Admin(w.registry);
    site.register(w.models["WBook"], { checkMessages: { no_such_check: "x" } });
    assert.throws(() => site.check(), (e: unknown) => e instanceof AdminConfigError && e.message.includes("no_such_check"));
  }));

  test(`an update of a locked model needs its version and bumps it (${provider})`, { skip: !hasExtension("monorm.locking", "monorm.timestamps") }, async () => {
    const dir = mkdtempSync(join(tmpdir(), "monorm-admin-guard-"));
    writeFileSync(join(dir, "package.json"), "{}");
    writeFileSync(join(dir, "schema.prisma"), GUARDED(provider));
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
      await models["GBook"].objects.using(db).insertMany([{ title: "One" }]);
      const site = new Admin(registry);
      site.register(models["GBook"]);
      const client = site.testClient(db, { user: SU });
      const url = "/admin/api/resources/GBook/1";
      let r = await client.patch(url, { json: { set: { title: "Two" }, loaded: { title: "One" } } });
      assert.ok(r.status === 400 && r.json().error.message.includes('needs the guard field "version"'), r.text);
      r = await client.patch(url, { json: { set: { title: "Two" }, loaded: { title: "One", version: 5 } } });
      assert.equal(r.status, 409, r.text);
      assert.deepEqual(r.json().conflicts.version, { loaded: 5, current: 0 });
      const before = (await models["GBook"].objects.using(db).first()).changed;
      await new Promise((done) => setTimeout(done, 20));
      r = await client.patch(url, { json: { set: { title: "Two" }, loaded: { title: "One", version: 0 } } });
      assert.deepEqual([r.status, r.json().changed], [200, ["title"]], r.text);
      const row = await models["GBook"].objects.using(db).first();
      assert.deepEqual([row.title, row.version], ["Two", 1]);
      assert.ok(row.changed.getTime() > before.getTime(), "the admin update sets the updated_at field");
    } finally {
      await db.dropTables();
      await db.close();
      rmSync(dir, { recursive: true, force: true });
    }
  });
}
