/** Bound query sets: `of()` writes and soft delete on SQLite and Postgres. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, of, Registry, type Database, type QueryEvent } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

const soft = { skip: !hasExtension("monorm.soft-delete") };
type Models = Record<string, any>;
const bound = (b: unknown): any => b;

async function open(dialect: string, source: string): Promise<[Database, Models]> {
  const registry = new Registry();
  const text = (dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + source;
  const models = (await loads(text, { registry })) as unknown as Models;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  return [db, models];
}

const observe = (db: Database): QueryEvent[] => {
  const events: QueryEvent[] = [];
  db.onQuery((e) => events.push(e));
  return events;
};

const plain = `
model Note {
  id    Int    @id
  title String
  views Int    @default(0)
  @@map("bw_note")
}`;

test("a bound write above the parameter cap is a QueryError", async () => {
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
  // case: bound_writes::a_bound_update_writes_one_statement_and_fills_in_input_order
  test(`bound update writes one statement and fills the objects in input order (${dialect})`, async () => {
    const [db, { Note }] = await open(dialect, plain);
    try {
      const held = await Note.objects.using(db).insertMany([0, 1, 2, 3].map((i: number) => ({ id: i + 1, title: `p${i}`, views: i }))).returning();
      const wanted = [held[2], held[0], held[3]];
      const events = observe(db);
      const result = await of(wanted).update({ title: "changed", views: Note.views.add(10) });
      assert.equal(events.length, 1);
      assert.deepEqual(result, wanted);
      assert.ok(result.every((n: any, i: number) => n === wanted[i]));
      assert.deepEqual(wanted.map((n: any) => [n.title, n.views]), [["changed", 12], ["changed", 10], ["changed", 13]]);
      assert.deepEqual([held[1].title, held[1].views], ["p1", 1]);
      assert.deepEqual((await Note.objects.using(db).orderBy(Note.id)).map((n: any) => n.title), ["changed", "p1", "changed", "changed"]);
    } finally { await db.dropTables(); await db.close(); }
  });

  // case: bound_writes::a_bound_update_leaves_out_an_object_with_no_row_and_keeps_the_filters
  test(`a bound update leaves out an object with no row and keeps the filters (${dialect})`, async () => {
    const [db, { Note }] = await open(dialect, plain);
    try {
      const held = await Note.objects.using(db).insertMany([0, 1, 2, 3].map((i: number) => ({ id: i + 1, title: `p${i}`, views: i }))).returning();
      await Note.objects.using(db).filter(Note.id.eq(held[0].id)).delete();
      assert.deepEqual(await of(held).update({ title: "x" }), held.slice(1));
      assert.equal(held[0].title, "p0");
      assert.deepEqual(await of(held).filter(Note.views.gte(2)).update({ title: "y" }), held.slice(2));
      assert.deepEqual(held.slice(1).map((n: any) => n.title), ["x", "y", "y"]);
      assert.deepEqual(await of(held).update({}), []);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a bound update of two objects cannot assign a key field (${dialect}) (${dialect})`, async () => {
    const [db, { Note }] = await open(dialect, plain);
    try {
      const notes = await Note.objects.using(db).insertMany([1, 2, 3].map((id: number) => ({ id, title: `t${id}` }))).returning();
      await assert.rejects(of([notes[0], notes[1]]).update({ id: Note.id.add(100) }), { name: "TypeError", message: /key field/ });
      assert.deepEqual((await Note.objects.using(db).orderBy(Note.id)).map((n: any) => n.id), [1, 2, 3]);
      const [moved] = await of([notes[2]]).update({ id: 103 });
      assert.equal(moved, notes[2]);
      assert.equal(moved.id, 103);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a bound update takes the instance shortcut and a duplicate (${dialect})`, async () => {
    const [db, { Note }] = await open(dialect, plain);
    try {
      const held = await Note.objects.using(db).insertMany([1, 2].map((id: number) => ({ id, title: `t${id}` }))).returning();
      assert.deepEqual(await held[0].qs().update({ views: 7 }), [held[0]]);
      assert.equal(held[0].views, 7);
      const twice = await of([held[1], held[1]]).update({ views: 9 });
      assert.ok(twice.length === 2 && twice[0] === held[1] && twice[1] === held[1]);
      assert.equal(held[1].views, 9);
    } finally { await db.dropTables(); await db.close(); }
  });

  // case: bound_writes::a_bound_delete_gives_the_deleted_objects_and_leaves_the_others
  test(`a bound delete gives the deleted objects and leaves the others (${dialect})`, async () => {
    const [db, { Note }] = await open(dialect, plain);
    try {
      const held = await Note.objects.using(db).insertMany([0, 1, 2, 3].map((i: number) => ({ id: i + 1, title: `p${i}`, views: i }))).returning();
      await Note.objects.using(db).filter(Note.id.eq(held[3].id)).delete();
      const events = observe(db);
      const result = await of([held[2], held[0], held[3]]).delete();
      assert.equal(events.length, 1);
      assert.deepEqual(result, [held[2], held[0]]);
      assert.deepEqual((await Note.objects.using(db).orderBy(Note.id)).map((n: any) => n.id), [held[1].id]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a bound update of a partial object keeps its shape (${dialect}) (${dialect})`, async () => {
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

  test(`bound update and delete of a model without locking: the last write wins (${dialect})`, async () => {
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
model Doc {
  id      Int       @id
  title   String
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("bw_doc")
}`;
  const docs = async () => {
    const [db, { Doc }] = await open(dialect, softSource);
    const held = await Doc.objects.using(db).insertMany([1, 2, 3, 4].map((id: number) => ({ id, title: `d${id}` }))).returning();
    return { db, Doc, held: held.sort((a: any, b: any) => a.id - b.id) };
  };

  // case: bound_writes::a_bound_delete_soft_deletes_and_fills_the_objects
  test(`a bound delete soft-deletes and fills the objects (${dialect})`, soft, async () => {
    const { db, Doc, held } = await docs();
    try {
      const result = await of([held[1], held[0]]).delete();
      assert.deepEqual(result, [held[1], held[0]]);
      assert.ok(held[0].deleted !== null && held[1].deleted !== null && held[2].deleted === null);
      assert.equal(await Doc.objects.using(db).count(), 2);
      assert.deepEqual(await of(held), held);
    } finally { await db.dropTables(); await db.close(); }
  });

  // case: bound_writes::the_soft_delete_members_are_on_the_bound_set
  test(`the soft-delete members are on the bound set (${dialect})`, soft, async () => {
    const { db, Doc, held } = await docs();
    try {
      await of(held.slice(0, 2)).delete();
      const set = bound(of(held));
      assert.deepEqual(await set.deletedOnly(), held.slice(0, 2));
      assert.deepEqual(await set.allWithDeleted(), held);
      assert.equal(await set.allWithDeleted().deletedOnly().count(), 2);
      assert.equal(await of(held).count(), 4);
      assert.equal(await Doc.objects.using(db).count(), 2);
    } finally { await db.dropTables(); await db.close(); }
  });

  // case: bound_writes::a_bound_undelete_restores_only_the_soft_deleted_rows
  test(`a bound undelete restores only the soft-deleted rows (${dialect})`, soft, async () => {
    const { db, Doc, held } = await docs();
    try {
      await of(held.slice(0, 2)).delete();
      assert.deepEqual(await bound(of(held)).undelete(), held.slice(0, 2));
      assert.ok(held.every((d: any) => d.deleted === null));
      assert.equal(await Doc.objects.using(db).count(), 4);
    } finally { await db.dropTables(); await db.close(); }
  });

  // case: bound_writes::a_bound_hard_delete_removes_the_rows_and_gives_the_objects
  test(`a bound hard delete removes the rows and gives the objects (${dialect})`, soft, async () => {
    const { db, Doc, held } = await docs();
    try {
      await of(held.slice(0, 2)).delete();
      assert.deepEqual(await bound(of([held[2], held[0]])).hardDelete(), [held[2], held[0]]);
      assert.ok(held[0].deleted !== null && held[2].deleted === null);
      assert.deepEqual((await Doc.objects.using(db).allWithDeleted().orderBy(Doc.id)).map((d: any) => d.id), [2, 4]);
      assert.deepEqual(await bound(of(held)).allWithDeleted(), [held[1], held[3]]);
    } finally { await db.dropTables(); await db.close(); }
  });
}

// case: bound_writes::a_bound_write_checks_the_database_and_the_input
test("a bound write checks the database and the input", async () => {
  const [db, { Note }] = await open("sqlite", plain);
  try {
    const held = await Note.objects.using(db).insertMany([1, 2].map((id: number) => ({ id, title: `t${id}` }))).returning();
    assert.throws(() => of([]), TypeError);
    assert.deepEqual(await of(held).using("primary").update({ views: 3 }), held);
    assert.equal((of(held) as any).hardDelete, undefined);
    assert.equal((of(held) as any).undelete, undefined);
  } finally { await db.dropTables(); await db.close(); }
});
