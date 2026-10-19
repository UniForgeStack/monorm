/** Bound query sets: `of()` / `post.qs()` reads on Postgres and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, define, loads, of, QueryError, Registry, type Database } from "../src/index.js";
import { native } from "../src/native.js";

const source = `
model Writer {
  id      Int     @id
  name    String
  entries Entry[]
  @@map("bq_writer")
}

model Entry {
  id       Int     @id
  writerId Int
  title    String
  views    Int     @default(0)
  live     Boolean @default(false)
  writer   Writer  @relation(fields: [writerId], references: [id])
  @@map("bq_entry")
}

model Part {
  shop  Int
  id    Int
  label String
  @@id([shop, id])
  @@map("bq_part")
}
`;

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"];

  async function setup(): Promise<{ db: Database; Writer: Any; Entry: Any; Part: Any; count: () => number }> {
    const registry = new Registry();
    const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${source}`, { registry }) as Record<string, Any>;
    const db = await connect(url!, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    const { Writer, Entry, Part } = models;
    await Writer.objects.using(db).insertMany([{ id: 1, name: "ann" }, { id: 2, name: "bo" }]);
    await Entry.objects.using(db).insertMany([
      { id: 1, writerId: 1, title: "a", views: 1, live: true },
      { id: 2, writerId: 1, title: "b", views: 2 },
      { id: 3, writerId: 2, title: "c", views: 3, live: true },
    ]);
    await Part.objects.using(db).insertMany([{ shop: 1, id: 1, label: "x" }, { shop: 2, id: 1, label: "y" }, { shop: 1, id: 2, label: "z" }]);
    let n = 0;
    db.onQuery(() => { n++; });
    return { db, Writer, Entry, Part, count: () => n };
  }

  test(`of() fills the objects in place, in input order, in one statement (${provider})`, { skip: !url }, async () => {
    const { db, Entry, count } = await setup();
    try {
      const held = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).filter(Entry.id.in([1, 3])).update({ title: "new", views: 9 });
      const before = count();
      const got = await Entry.objects.of([held[2], held[0], held[1]]);
      assert.equal(count() - before, 1);
      assert.deepEqual(got.map((e: Any) => e.id), [3, 1, 2]);
      assert.ok(got[0] === held[2] && got[1] === held[0]);
      assert.deepEqual(held.map((e: Any) => [e.title, e.views]), [["new", 9], ["b", 2], ["new", 9]]);
      await Entry.objects.using(db).filter(Entry.id.eq(2)).delete();
      const kept = await of([held[1], held[0]]);
      assert.deepEqual(kept.map((e: Any) => e.id), [1]);
      assert.equal(held[1].title, "b");
      const twin = await Entry.objects.using(db).get(Entry.id.eq(1));
      await Entry.objects.using(db).filter(Entry.id.eq(1)).update({ views: 5 });
      const both = await of([held[0], twin]);
      assert.deepEqual(both.map((e: Any) => e.views), [5, 5]);
      assert.ok(both[0] === held[0] && both[1] === twin);
    } finally {
      await db.close();
    }
  });

  test(`filter ANDs with the key filter; count and exists (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const held = await Entry.objects.using(db).orderBy("id");
      assert.deepEqual((await of(held).filter(Entry.live.eq(true))).map((e: Any) => e.id), [1, 3]);
      assert.deepEqual((await of(held).filter(Entry.live.eq(true)).exclude(Entry.id.eq(1))).map((e: Any) => e.id), [3]);
      assert.equal(await of(held).filter(Entry.live.eq(true)).count(), 2);
      assert.equal(await of([held[1]]).filter(Entry.live.eq(true)).exists(), false);
      assert.match(of(held).sql(), /IN/i);
    } finally {
      await db.close();
    }
  });

  test(`a large read runs in chunks of 500 single keys or 250 composite keys (${provider})`, { skip: !url }, async () => {
    const { db, Entry, Part, count } = await setup();
    try {
      await Entry.objects.using(db).insertMany(Array.from({ length: 1200 }, (_, i) => ({ id: 1000 + i, writerId: 1, title: `t${i}` })));
      await Part.objects.using(db).insertMany(Array.from({ length: 1200 }, (_, i) => ({ shop: 5, id: i, label: `l${i}` })));
      const entries = (await Entry.objects.using(db).filter(Entry.id.gte(1000)).orderBy("id")).reverse();
      const parts = await Part.objects.using(db).filter(Part.shop.eq(5)).orderBy("id");
      await Entry.objects.using(db).filter(Entry.id.gte(1000)).update({ title: "changed" });
      await Part.objects.using(db).filter(Part.shop.eq(5)).update({ label: "changed" });
      for (const held of [entries, parts]) {
        const model = held === entries ? Entry : Part;
        const reads = held === entries ? 3 : 5;
        const before = count();
        const got = await model.objects.of(held);
        assert.equal(count() - before, reads);
        assert.ok(got.length === 1200 && got.every((o: Any, i: number) => o === held[i]));
        assert.ok(held.every((o: Any) => (o.title ?? o.label) === "changed"));
        assert.equal(await model.objects.of(held).count(), 1200);
        assert.equal(await model.objects.of(held).exists(), true);
        assert.equal(count() - before, reads * 2 + 1);
      }
    } finally {
      await db.close();
    }
  });

  test(`load(column) refreshes the named fields and keeps the others (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const [first] = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).filter(Entry.id.eq(1)).update({ title: "fresh", views: 77 });
      const [got] = await of(first).load(Entry.title);
      assert.ok(got === first);
      assert.equal(first.title, "fresh");
      assert.equal(first.views, 1);
    } finally {
      await db.close();
    }
  });

  test(`a load of relations only runs no root query (${provider})`, { skip: !url }, async () => {
    const { db, Writer, Entry, count } = await setup();
    try {
      const entries = await Entry.objects.using(db).orderBy("id");
      const writers = await Writer.objects.using(db).orderBy("id");
      await Entry.objects.using(db).delete(); // the roots are gone: only a root query would notice
      const engine = (db as Any).engine;
      const run = engine.prefetch.bind(engine);
      let prefetches = 0;
      engine.prefetch = (...args: unknown[]) => { prefetches++; return run(...args); };
      const before = count();
      const got = await of(entries).load(Entry.writer);
      assert.equal(count() - before, 0);
      assert.equal(prefetches, 1);
      assert.deepEqual(got.map((e: Any) => e.writer.name), ["ann", "ann", "bo"]);
      await of(writers).load(Writer.entries);
      assert.equal(prefetches, 2);
      assert.deepEqual(writers.map((w: Any) => w.entries.cached.length), [0, 0]);
    } finally {
      await db.close();
    }
  });

  test(`a filter on a relation-only load reads the key fields and keeps a partial object's shape (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const partial = await Entry.objects.using(db).load(Entry.id, Entry.title).get(Entry.id.eq(1));
      await Entry.objects.using(db).filter(Entry.id.eq(1)).update({ title: "changed" });
      const [got] = await of([partial]).filter(Entry.live.eq(true)).load(Entry.writer);
      assert.equal(got, partial);
      assert.equal(partial.title, "a");
      assert.throws(() => partial.views, /not loaded/i);
      assert.equal(partial.writer.name, "ann");
    } finally {
      await db.close();
    }
  });

  test(`using("primary") reads the primary of the objects' database (${provider})`, { skip: !url }, async () => {
    const { db, Entry, count } = await setup();
    try {
      const entries = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).filter(Entry.id.eq(1)).update({ title: "changed" });
      const before = count();
      assert.equal((await of(entries).using("primary")).length, 3);
      assert.equal(count() - before, 1);
      assert.equal(entries[0].title, "changed");
    } finally {
      await db.close();
    }
  });

  test(`a mixed load fills columns and relations (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const entries = await Entry.objects.using(db).orderBy("id");
      const got = await of(entries).load(Entry.title, Entry.writer);
      assert.deepEqual(got.map((e: Any) => e.writer.name), ["ann", "ann", "bo"]);
    } finally {
      await db.close();
    }
  });

  test(`a composite key reads the objects' rows (${provider})`, { skip: !url }, async () => {
    const { db, Part, count } = await setup();
    try {
      const held = await Part.objects.using(db).orderBy("shop", "id");
      await Part.objects.using(db).filter(Part.shop.eq(1), Part.id.eq(2)).update({ label: "zz" });
      const before = count();
      const got = await of([held[2], held[1]]);
      assert.equal(count() - before, 1);
      assert.deepEqual(got.map((p: Any) => p.label), ["y", "zz"]);
    } finally {
      await db.close();
    }
  });

  test(`input errors (${provider})`, { skip: !url }, async () => {
    const { db, Writer, Entry } = await setup();
    try {
      const [entry] = await Entry.objects.using(db).orderBy("id");
      const [writer] = await Writer.objects.using(db).orderBy("id");
      assert.throws(() => Entry.objects.of([]), TypeError);
      assert.throws(() => of([]), TypeError);
      assert.throws(() => Entry.objects.of([entry, writer]), TypeError);
      assert.throws(() => Entry.objects.orderBy("id").of(entry), TypeError);
      assert.throws(() => Entry.objects.limit(2).of(entry), TypeError);
      assert.throws(() => writer.entries.of(entry), TypeError);
      assert.throws(() => Writer.entries.objects.of(writer), TypeError);
      assert.throws(() => Entry.objects.of(Object.assign(Object.create(Object.getPrototypeOf(entry)), { title: "x" })), TypeError);
      for (const name of ["orderBy", "first", "get", "limit", "select", "prepare", "batches"]) {
        assert.equal((of(entry) as Any)[name], undefined, name);
      }
    } finally {
      await db.close();
    }
  });

  test(`objects of two databases need using() (${provider})`, { skip: provider !== "sqlite" }, async () => {
    const { db, Entry, Writer } = await setup();
    const other = await connect("sqlite://:memory:", { registry: Entry._meta.registry, default: false });
    try {
      await other.createTables();
      const mine = await Entry.objects.using(db).get(Entry.id.eq(1));
      await Writer.objects.using(other).insert({ id: 1, name: "x" });
      const theirs = await Entry.objects.using(other).insert({ id: 9, writerId: 1, title: "z" });
      await assert.rejects(Promise.resolve(of([mine, theirs])), QueryError);
      assert.equal(await of([mine, theirs]).using(db).count(), 1);
    } finally {
      await other.close();
      await db.close();
    }
  });

  test(`post.qs() is Model.objects.of(post) (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const [first] = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).filter(Entry.id.eq(1)).update({ views: 42 });
      const [got] = await first.qs();
      assert.ok(got === first);
      assert.equal(first.views, 42);
      assert.equal(await first.qs().filter(Entry.live.eq(false)).exists(), false);
    } finally {
      await db.close();
    }
  });
}

const queryDefaults = (JSON.parse(native().nativeArtifact()) as { capabilities?: string[] }).capabilities?.includes("query-defaults") ?? false;

test("of() applies the model's query defaults; withoutDefaults() reads past them", { skip: !queryDefaults }, async () => {
  const registry = new Registry();
  const field = (name: string, type = "string", flags = {}) => ({ name, column: name, type, ...flags });
  const models = define({ dialect: "sqlite", models: [
    { name: "BoundNote", table: "bound_note", fields: [field("id", "int", { primary_key: true }), field("visible", "bool", { default: true }), field("body")], relations: [] },
  ], behavior: { schema_contract: 2, read_defaults: [
    { source: "orm.query-defaults", name: "query", model: "BoundNote", filter: { t: "col", path: [], name: "visible" } },
  ] } } as never, { registry });
  const Note = models["BoundNote"] as Any;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const notes = await Note.objects.using(db).withoutDefaults().insertMany([{ id: 1, visible: true, body: "a" }, { id: 2, visible: false, body: "b" }]).returning();
    assert.deepEqual((await of(notes)).map((n: Any) => n.id), [1]);
    assert.deepEqual((await of(notes).withoutDefaults()).map((n: Any) => n.id), [1, 2]);
    assert.equal(await notes[1].qs().exists(), false);
    assert.equal(await notes[1].qs().withoutDefaults().exists(), true);
  } finally {
    await db.close();
  }
});
