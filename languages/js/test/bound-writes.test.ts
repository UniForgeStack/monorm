/** Bound query sets: `of()` writes, no version check and soft delete on SQLite and Postgres. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, of, Registry, type Database } from "../src/index.js";
import { native } from "../src/native.js";
import { loadsSoftDelete } from "./soft-delete-ext.js";

const capabilities = JSON.parse(native().nativeArtifact()).capabilities as string[];
const soft = { skip: !capabilities.includes("soft-delete") };
type Models = Record<string, any>;
const bound = (b: unknown): any => b;

async function open(dialect: string, source: string, extended = false): Promise<[Database, Models]> {
  const registry = new Registry();
  const text = (dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + source;
  const models = (extended ? await loadsSoftDelete(text, { registry }) : await loads(text, { registry })) as unknown as Models;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  return [db, models];
}

const plain = `
model Note {
  id    Int    @id
  title String
  views Int    @default(0)
  @@map("bw_note")
}`;

test("a bound write above the parameter cap is a QueryError (sqlite)", async () => {
  const [db, { Note }] = await open("sqlite", plain);
  try {
    const rows = Array.from({ length: 33_000 }, (_, i) => ({ id: i + 1, title: "t" }));
    const notes = await Note.objects.using(db).insertMany(rows).returning();
    await assert.rejects(of(notes).delete(), { name: "QueryError", message: /parameters/ });
    await assert.rejects(of(notes).update({ views: 1 }), { name: "QueryError", message: /parameters/ });
    assert.equal(await Note.objects.using(db).count(), 33_000);
  } finally { await db.dropTables(); await db.close(); }
});

for (const dialect of ["sqlite", "postgres"]) {
  test(`bound update fills the objects and gives them in input order (${dialect})`, async () => {
    const [db, { Note }] = await open(dialect, plain);
    try {
      const notes = await Note.objects.using(db).insertMany([1, 2, 3].map((id: number) => ({ id, title: `t${id}` }))).returning();
      const held = [notes[2], notes[0]];
      const done = await of(held).update({ views: 7 });
      assert.deepEqual(done.map((n: any) => n.id), [3, 1]);
      assert.equal(done[0], held[0]);
      assert.deepEqual([notes[2].views, notes[0].views, notes[1].views], [7, 7, 0]);
      assert.equal(await Note.objects.using(db).filter(Note.views.eq(7)).count(), 2);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`bound update skips an object whose row the filter hides, and an empty update runs nothing (${dialect})`, async () => {
    const [db, { Note }] = await open(dialect, plain);
    try {
      const notes = await Note.objects.using(db).insertMany([{ id: 1, title: "a" }, { id: 2, title: "b" }]).returning();
      const done = await of(notes).filter(Note.title.eq("b")).update({ views: 3 });
      assert.deepEqual(done.map((n: any) => n.id), [2]);
      assert.deepEqual(notes.map((n: any) => n.views), [0, 3]);
      assert.deepEqual(await of(notes).update({}), []);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`bound delete gives the objects whose row was deleted (${dialect})`, async () => {
    const [db, { Note }] = await open(dialect, plain);
    try {
      const notes = await Note.objects.using(db).insertMany([1, 2, 3].map((id: number) => ({ id, title: "x" }))).returning();
      await Note.objects.using(db).filter(Note.id.eq(2)).delete();
      const done = await of(notes).delete();
      assert.deepEqual(done.map((n: any) => n.id), [1, 3]);
      assert.equal(await Note.objects.using(db).count(), 0);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`bound update of a partial object keeps its shape (${dialect})`, async () => {
    const [db, { Note }] = await open(dialect, plain);
    try {
      await Note.objects.using(db).insertMany([{ id: 1, title: "a" }, { id: 2, title: "b" }]);
      const partial = await Note.objects.using(db).load(Note.id, Note.title).filter(Note.id.eq(1)).get();
      const [done] = await of(partial).update({ views: 5 });
      assert.equal(done, partial);
      assert.throws(() => partial.views, /not loaded/i);
      assert.equal((await Note.objects.using(db).get(Note.id.eq(1))).views, 5);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`bound update and delete do not check versions: the last write wins (${dialect})`, async () => {
    const [db, { Note }] = await open(dialect, plain);
    try {
      const notes = Note.objects.using(db);
      const held = await notes.insertMany([1, 2].map((id: number) => ({ id, title: "a" }))).returning();
      await notes.filter(Note.id.eq(2)).update({ title: "other" });
      const done = await of(held).update({ title: "b" });
      assert.equal(done.length, 2);
      assert.deepEqual((await notes.all()).map((n: any) => n.title), ["b", "b"]);
      assert.equal((await of(held).delete()).length, 2);
      assert.equal(await notes.count(), 0);
    } finally { await db.dropTables(); await db.close(); }
  });

  const softSource = `
model Item {
  id      Int       @id
  name    String
  deleted DateTime? @soft_delete.deleted_at(mode: "application")
  @@map("bw_item")
}`;

  test(`bound delete is a soft delete; undelete and hardDelete work on the bound set (${dialect})`, soft, async () => {
    const [db, { Item }] = await open(dialect, softSource, true);
    try {
      const items = Item.objects.using(db);
      const held = await items.insertMany([1, 2, 3].map((id: number) => ({ id, name: `i${id}` }))).returning();
      const gone = await of(held).delete();
      assert.equal(gone.length, 3);
      assert.ok(held.every((i: any) => i.deleted !== null));
      assert.equal(await items.count(), 0);
      const back = await bound(of([held[0], held[1]])).undelete();
      assert.deepEqual(back.map((i: any) => i.id), [1, 2]);
      assert.ok(back.every((i: any) => i.deleted === null));
      assert.equal(await items.count(), 2);
      const removed = await bound(of(held)).hardDelete();
      assert.deepEqual(removed.map((i: any) => i.id), [1, 2, 3]);
      assert.equal(await items.allWithDeleted().count(), 0);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`bound allWithDeleted and deletedOnly read soft-deleted rows (${dialect})`, soft, async () => {
    const [db, { Item }] = await open(dialect, softSource, true);
    try {
      const items = Item.objects.using(db);
      const held = await items.insertMany([1, 2].map((id: number) => ({ id, name: `i${id}` }))).returning();
      await of(held[0]).delete();
      assert.equal(await of(held).count(), 1);
      assert.equal(await bound(of(held)).allWithDeleted().count(), 2);
      assert.equal(await bound(of(held)).deletedOnly().count(), 1);
    } finally { await db.dropTables(); await db.close(); }
  });
}

test("a model without the extension has no hardDelete on its bound set", async () => {
  const [db, { Note }] = await open("sqlite", plain);
  try {
    const [note] = await Note.objects.using(db).insertMany([{ id: 1, title: "a" }]).returning();
    assert.equal((of(note) as any).hardDelete, undefined);
  } finally { await db.dropTables(); await db.close(); }
});
