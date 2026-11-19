/** Bound query sets: `of()` / `post.qs()` reads on Postgres and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, NotLoaded, of, QueryError, Registry, TransactionRequired, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

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
  const url = provider === "sqlite" ? "sqlite://:memory:" : (process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test");

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

  // case: bound_reads::a_bound_read_fills_the_objects_in_place_in_input_order
  test(`a bound read fills the objects in place, in input order (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const held = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).filter(Entry.id.in([1, 3])).update({ title: "new", views: 9 });
      const wanted = [held[2], held[0]];
      const got = await Entry.objects.of(wanted);
      assert.deepEqual(got.map((e: Any) => e.id), [3, 1]);
      assert.ok(got.every((e: Any, i: number) => e === wanted[i]));
      assert.deepEqual(wanted.map((e: Any) => [e.title, e.views]), [["new", 9], ["new", 9]]);
      assert.equal(held[1].title, "b");
    } finally {
      await db.close();
    }
  });

  // case: bound_reads::a_bound_read_runs_one_statement_for_many_objects
  test(`a bound read runs one statement for many objects (${provider})`, { skip: !url }, async () => {
    const { db, Entry, count } = await setup();
    try {
      const held = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).all().update({ title: "new" });
      const before = count();
      await Entry.objects.of(held);
      assert.equal(count() - before, 1);
    } finally {
      await db.close();
    }
  });

  // case: bound_reads::an_unmatched_object_stays_unchanged_and_is_left_out
  test(`an unmatched object stays unchanged and is left out (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const held = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).all().update({ title: "new" });
      await Entry.objects.using(db).filter(Entry.id.eq(2)).delete();
      const kept = await of(held);
      assert.ok(kept.length === 2 && kept[0] === held[0] && kept[1] === held[2]);
      assert.equal(held[1].title, "b");
    } finally {
      await db.close();
    }
  });

  // case: bound_reads::two_objects_with_one_key_are_both_filled_and_returned
  test(`two objects with one key are both filled and returned (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const first = await Entry.objects.using(db).get(Entry.id.eq(1));
      const second = await Entry.objects.using(db).get(Entry.id.eq(1));
      await Entry.objects.using(db).filter(Entry.id.eq(1)).update({ title: "changed" });
      const both = await of([first, second]);
      assert.ok(both.length === 2 && both[0] === first && both[1] === second);
      assert.deepEqual([first.title, second.title], ["changed", "changed"]);
    } finally {
      await db.close();
    }
  });

  test(`a relation-only read gives its statements to onQuery (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const held = await Entry.objects.using(db).orderBy("id");
      const seen: string[] = [];
      db.onQuery((e: Any) => { seen.push(e.sql); });
      await of(held).load(Entry.writer);
      assert.ok(seen.length >= 1);
    } finally {
      await db.close();
    }
  });

  // case: bound_reads::filter_narrows_the_bound_rows
  test(`filter narrows the bound rows (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const held = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).all().update({ title: "new" });
      const result = await of(held).filter(Entry.id.in([1, 3]));
      assert.ok(result.length === 2 && result[0] === held[0] && result[1] === held[2]);
      assert.equal(held[1].title, "b");
      assert.equal(await of(held).exclude(Entry.id.eq(1)).count(), 2);
      assert.match(of(held).sql(), /IN/i);
    } finally {
      await db.close();
    }
  });

  // case: bound_reads::count_and_exists_count_the_matching_bound_rows
  test(`count and exists count the matching bound rows (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const held = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).filter(Entry.id.eq(1)).delete();
      assert.equal(await of(held).count(), 2);
      assert.equal(await of(held).exists(), true);
      assert.equal(await of(held[0]).exists(), false);
      assert.equal(await of(held[0]).count(), 0);
    } finally {
      await db.close();
    }
  });

  // case: bound_reads::a_large_bound_read_runs_in_chunks_of_500_keys
  test(`a large bound read runs in chunks of 500 keys (${provider})`, { skip: !url }, async () => {
    const { db, Entry, count } = await setup();
    try {
      await Entry.objects.using(db).insertMany(Array.from({ length: 1200 }, (_, i) => ({ id: 1000 + i, writerId: 1, title: `t${i}` })));
      const held = await Entry.objects.using(db).filter(Entry.id.gte(1000)).orderBy("id");
      await Entry.objects.using(db).filter(Entry.id.gte(1000)).update({ title: "changed" });
      const wanted = [...held].reverse();
      const before = count();
      const got = await Entry.objects.of(wanted);
      assert.equal(await Entry.objects.of(wanted).count(), 1200);
      assert.equal(await Entry.objects.of(wanted).exists(), true);
      assert.equal(count() - before, 3 + 3 + 1);
      assert.ok(got.length === 1200 && got.every((o: Any, i: number) => o === wanted[i]));
      assert.ok(held.every((o: Any) => o.title === "changed"));
      const seen: string[] = [];
      db.onQuery((e: Any) => { seen.push(e.sql); });
      assert.equal((await of(wanted).filter(Entry.id.gt(0)).load(Entry.writer)).length, 1200);
      assert.equal(seen.filter((q) => !q.includes("bq_writer")).length, 3);
      const text = of(wanted).sql();
      assert.equal(text.slice(text.lastIndexOf("IN (")).split(")")[0]!.split(",").length, 500);
    } finally {
      await db.close();
    }
  });

  // case: bound_reads::a_large_composite_key_bound_read_runs_in_chunks_of_250_keys
  test(`a large composite key bound read runs in chunks of 250 keys (${provider})`, { skip: !url }, async () => {
    const { db, Part, count } = await setup();
    try {
      await Part.objects.using(db).insertMany(Array.from({ length: 1200 }, (_, i) => ({ shop: 5, id: i, label: `l${i}` })));
      const held = await Part.objects.using(db).filter(Part.shop.eq(5)).orderBy("id");
      await Part.objects.using(db).filter(Part.shop.eq(5)).update({ label: "changed" });
      const before = count();
      const got = await Part.objects.of(held);
      assert.equal(await Part.objects.of(held).count(), 1200);
      assert.equal(count() - before, 10);
      assert.ok(got.length === 1200 && got.every((o: Any, i: number) => o === held[i]));
      assert.ok(held.every((o: Any) => o.label === "changed"));
    } finally {
      await db.close();
    }
  });

  // case: bound_reads::the_merge_rule_keeps_other_loaded_fields
  test(`the merge rule keeps other loaded fields (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const partial = await Entry.objects.using(db).load(Entry.title, Entry.views).get(Entry.id.eq(1));
      await Entry.objects.using(db).filter(Entry.id.eq(1)).update({ title: "fresh", views: 77, live: false });
      const [got] = await of(partial).load(Entry.title);
      assert.ok(got === partial);
      assert.ok(partial.title === "fresh" && partial.views === 1);
      assert.throws(() => partial.live, NotLoaded);
      await of(partial);
      assert.ok(partial.live === false && partial.views === 77);
    } finally {
      await db.close();
    }
  });

  test(`a relation-only load runs no root query (${provider}) (${provider})`, { skip: !url }, async () => {
    const { db, Writer, Entry, count } = await setup();
    try {
      const entries = await Entry.objects.using(db).orderBy("id");
      const writers = await Writer.objects.using(db).orderBy("id");
      await Entry.objects.using(db).all().delete(); // the roots are gone: only a root query would notice
      const engine = (db as Any).engine;
      const run = engine.prefetch.bind(engine);
      let prefetches = 0;
      engine.prefetch = (...args: unknown[]) => { prefetches++; return run(...args); };
      const before = count();
      const got = await of(entries).load(Entry.writer);
      assert.equal(count() - before, 1);
      assert.equal(prefetches, 1);
      assert.deepEqual(got.map((e: Any) => e.writer.name), ["ann", "ann", "bo"]);
      await of(writers).load(Writer.entries);
      assert.equal(prefetches, 2);
      assert.deepEqual(writers.map((w: Any) => w.entries.cached.length), [0, 0]);
    } finally {
      await db.close();
    }
  });

  // case: bound_reads::a_relation_only_load_with_a_filter_reads_the_keys_first
  test(`a relation-only load with a filter reads the keys first (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const partial = await Entry.objects.using(db).load(Entry.id, Entry.title).get(Entry.id.eq(1));
      await Entry.objects.using(db).filter(Entry.id.eq(1)).update({ title: "changed" });
      const other = await Entry.objects.using(db).load(Entry.id, Entry.title).get(Entry.id.eq(3));
      const [got, ...rest] = await of([partial, other]).filter(Entry.id.eq(1)).load(Entry.writer);
      assert.equal(got, partial);
      assert.equal(rest.length, 0);
      assert.throws(() => other.writer, NotLoaded);
      assert.equal(partial.title, "a");
      assert.throws(() => partial.views, /not loaded/i);
      assert.equal(partial.writer.name, "ann");
    } finally {
      await db.close();
    }
  });

  // case: bound_reads::using_names_the_database_to_read
  test(`using() names the database to read (${provider})`, { skip: !url }, async () => {
    const { db, Entry, Writer, count } = await setup();
    const other = await connect(url!, { registry: Entry._meta.registry, default: false });
    try {
      const entries = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).filter(Entry.id.eq(1)).update({ title: "changed" });
      if (provider === "sqlite") {
        await other.createTables();
        await Writer.objects.using(other).insertMany([{ id: 1, name: "ann" }, { id: 2, name: "bo" }]);
        const rows = (await Entry.objects.using(db).orderBy("id")).map((e: Any) => ({ id: e.id, writerId: e.writerId, title: e.title, views: e.views, live: e.live }));
        await Entry.objects.using(other).insertMany(rows);
      }
      let seen = 0;
      other.onQuery(() => { seen++; });
      await of(entries).using(other);
      assert.ok(seen === 1 && entries[0].title === "changed");
      const before = count();
      assert.equal((await of(entries).using("primary")).length, 3);
      assert.equal(count() - before, 1);
      assert.throws(() => of(entries).using("replica" as never));
    } finally {
      await other.close();
      await db.close();
    }
  });

  // case: bound_reads::a_load_with_columns_joins_a_to_one_relation_in_the_one_statement
  test(`a load with columns joins a to-one relation in the one statement (${provider})`, { skip: !url }, async () => {
    const { db, Entry, count } = await setup();
    try {
      const entries = await Entry.objects.using(db).orderBy("id");
      const before = count();
      const got = await of(entries).load(Entry.title, Entry.writer);
      assert.equal(count() - before, 1);
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
      assert.throws(() => of([entry, writer]), TypeError);
      assert.throws(() => Writer.objects.of(entry), TypeError);
      assert.throws(() => Entry.objects.of(Object.assign(Object.create(Object.getPrototypeOf(entry)), { title: "x" })), TypeError);
    } finally {
      await db.close();
    }
  });

  // case: bound_reads::a_query_set_with_shape_state_is_refused
  test(`a query set with shape state is refused (${provider})`, { skip: !url }, async () => {
    const { db, Writer, Entry } = await setup();
    try {
      const [entry] = await Entry.objects.using(db).orderBy("id");
      const [writer] = await Writer.objects.using(db).orderBy("id");
      for (const [label, shaped] of [["orderBy", Entry.objects.orderBy("id")], ["limit", Entry.objects.limit(1)], ["offset", Entry.objects.offset(1)], ["require", Entry.objects.require()]]) {
        assert.throws(() => (shaped as Any).of([entry]), TypeError, label);
      }
      assert.throws(() => writer.entries.of(entry), TypeError);
      assert.throws(() => Writer.entries.objects.of(writer), TypeError);
    } finally {
      await db.close();
    }
  });

  test(`objects from two databases need using() (${provider}) (${provider})`, { skip: provider !== "sqlite" }, async () => {
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

  // case: bound_reads::one_object_and_the_instance_shortcut
  test(`one object and the instance shortcut (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const held = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).all().update({ views: 42 });
      const one = await of(held[0]);
      assert.ok(one.length === 1 && one[0] === held[0]);
      assert.equal(held[0].views, 42);
      const [got] = await held[1].qs();
      assert.ok(got === held[1]);
      assert.equal(held[1].views, 42);
      const [model] = await Entry.objects.of(held[2]);
      assert.ok(model === held[2]);
      assert.equal(await held[0].qs().filter(Entry.live.eq(false)).exists(), false);
    } finally {
      await db.close();
    }
  });

  test(`a cached relation whose key changed is dropped (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const entry = await Entry.objects.using(db).load(Entry.writer).get(Entry.id.eq(1));
      assert.equal(entry.writer.name, "ann");
      await Entry.objects.using(db).filter(Entry.id.eq(1)).update({ writerId: 2 });
      await of(entry);
      assert.throws(() => entry.writer, NotLoaded);
    } finally {
      await db.close();
    }
  });

  test(`load selects only what it names plus the hidden keys (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const held = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).all().update({ title: "changed", views: 100 });
      const bare = [await Entry.objects.using(db).load(Entry.id).get(Entry.id.eq(1)), await Entry.objects.using(db).load(Entry.id).get(Entry.id.eq(2))];
      await of(bare).load(Entry.title);
      assert.deepEqual(bare.map((e: Any) => e.title), ["changed", "changed"]);
      assert.throws(() => bare[0].views, NotLoaded);
      assert.equal(held[0].title, "a");
    } finally {
      await db.close();
    }
  });

  test(`a default query set filter stays on the bound set (${provider})`, { skip: !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const held = await Entry.objects.using(db).orderBy("id");
      await Entry.objects.using(db).all().update({ title: "changed", views: 100 });
      const published = Entry.objects.using(db).filter(Entry.views.gt(1000));
      assert.deepEqual(await published.of(held), []);
      assert.equal(held[0].title, "a");
    } finally {
      await db.close();
    }
  });

  test(`lock needs a transaction and locks the rows (${provider})`, { skip: provider !== "postgresql" || !url }, async () => {
    const { db, Entry } = await setup();
    try {
      const held = await Entry.objects.using(db).orderBy("id");
      await assert.rejects(async () => { await of(held).lock(); }, TransactionRequired);
      await db.transaction(async () => {
        const query = of(held).lock({ exclusive: false, skipLocked: true });
        const text = query.sql();
        assert.ok(text.includes("FOR SHARE OF") && text.endsWith("SKIP LOCKED"), text);
        assert.equal((await query).length, 3);
      });
      assert.throws(() => of(held).lock({ nowait: true, skipLocked: true } as never), TypeError);
    } finally {
      await db.close();
    }
  });

  test(`a relation-only read with a lock reads the keys with FOR UPDATE (${provider})`, { skip: provider !== "postgresql" || !url }, async () => {
    const { db, Writer } = await setup();
    try {
      const held = await Writer.objects.using(db).orderBy("id");
      const seen: string[] = [];
      db.onQuery((e: Any) => { seen.push(e.sql); });
      await db.transaction(async () => {
        await of(held).lock().load(Writer.entries);
      });
      assert.ok(seen.some((q) => q.includes("FOR UPDATE")));
    } finally {
      await db.close();
    }
  });
}

test("a column named qs is reserved", () => {
  assert.throws(() => loads("datasource db {\n  provider = \"sqlite\"\n}\nmodel Q {\n  id Int @id\n  qs Int\n}\n", { registry: new Registry() }), /qs|reserved/);
});

// case: bound_reads::a_bound_query_set_has_no_shape_methods
test("a bound query set has no shape methods", async () => {
  const registry = new Registry();
  const { Q } = loads("datasource db {\n  provider = \"sqlite\"\n}\nmodel Q {\n  id Int @id\n  @@map(\"bq_shape\")\n}\n", { registry }) as Record<string, Any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const row = await Q.objects.using(db).insert({ id: 1 });
    for (const name of [
      "orderBy", "limit", "offset", "first", "last", "get", "paginate", "select", "values", "groupBy", "cte", "union",
      "insert", "insertMany", "getOrInsert", "inBulk", "prepare", "batches", "require", "label", "asPrefetch",
    ]) {
      assert.equal((of(row) as Any)[name], undefined, name);
    }
  } finally {
    await db.close();
  }
});

const queryDefaults = hasExtension("monorm.query-defaults");

test("of() applies no query default on the root; withoutDefaults() stays accepted", { skip: !hasExtension("monorm.soft-delete") }, async () => {
  const source = 'datasource db {\n  provider = "sqlite"\n}\nmodel Doc {\n  id Int @id\n  title String\n  @@soft_delete.deleted_at("deleted", mode: "application")\n  @@map("br_doc")\n}\n';
  const registry = new Registry();
  const { Doc } = loads(source, { registry }) as Record<string, Any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const docs = Doc.objects.using(db);
    await docs.insertMany([{ id: 1, title: "a" }, { id: 2, title: "b" }]);
    const held = await docs.orderBy("id");
    await docs.filter(Doc.id.eq(1)).update({ title: "changed" });
    await docs.filter(Doc.id.eq(1)).delete();
    await docs.filter(Doc.id.eq(2)).update({ title: "changed" });
    const same = (got: Any[], want: Any[]) => got.length === want.length && got.every((d, i) => d === want[i]);
    assert.ok(same(await Doc.objects.of(held).using(db), held));
    assert.ok(held[0].title === "changed" && held[1].title === "changed");
    assert.ok(same(await Doc.objects.of(held).using(db).withoutDefaults("deleted"), held));
    assert.ok(same(await Doc.objects.of(held[0]).using(db).withoutDefaults(), [held[0]]));
    assert.equal(await Doc.objects.of(held).using(db).count(), 2);
    assert.ok(same(await Doc.objects.of(held).using(db).filter(Doc.id.eq(2)), [held[1]]));
  } finally {
    await db.close();
  }
});
