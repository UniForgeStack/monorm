/** Lazy statements use the async stores at their first await. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry, WriteProtected, allowWrites, debug, InspectedStatement } from "../src/index.js";
import { active, capture, internalLoop, record, relationLoad } from "../src/debug.js";
import { inTransaction } from "../src/db.js";
import { DATABASE_URL } from "./helpers.js";

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any
const source = (provider: string, protectedWrite = false) => `
datasource db {
  provider = "${provider}"
}
model LazyContextEntry {
  id Int @id
  name String
  @@map("lazy_context_entry")
  ${protectedWrite ? "@@protected_write" : ""}
}`;
async function setup(url: string, protectedWrite = false) {
  const registry = new Registry();
  const models = loads(source(url.startsWith("sqlite:") ? "sqlite" : "postgresql", protectedWrite), { registry }) as Record<string, Any>;
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  const Entry = models["LazyContextEntry"];
  return { db, Entry, rows: () => Entry.objects.using(db) };
}
const writes: Record<string, (Entry: Any, rows: () => Any) => Promise<unknown>> = {
  insert: (_Entry, rows) => rows().insert({ id: 3, name: "new" }),
  insertMany: (_Entry, rows) => rows().insertMany([{ id: 3, name: "new" }, { id: 4, name: "bulk" }], { batchSize: 1 }).returning(),
  upsert: (Entry, rows) => rows().insert({ id: 1, name: "changed" }).onConflict(Entry.id, { update: true }).returning(),
  bulkUpsert: (Entry, rows) => rows().insertMany([{ id: 1, name: "changed" }]).onConflict(Entry.id, { update: true }),
  update: (_Entry, rows) => rows().update({ name: "changed" }, { returning: true }),
  updateMany: (_Entry, rows) => rows().updateMany([{ id: 1, name: "changed" }]),
  delete: (_Entry, rows) => rows().delete({ returning: true }),
  hardDelete: (_Entry, rows) => rows().hardDelete(),
};
for (const [dialect, url] of [["Postgres", DATABASE_URL], ["SQLite", "sqlite://:memory:"]] as const) {
  for (const [method, build] of Object.entries(writes)) {
    test(`${dialect} lazy ${method} joins the transaction at await`, async () => {
      const { db, Entry, rows } = await setup(url);
      try {
        await rows().insertMany([{ id: 1, name: "one" }, { id: 2, name: "two" }]);
        const statement = build(Entry, rows);
        const rollback = new Error("rollback");
        await assert.rejects(db.transaction(async () => {
          await statement;
          throw rollback;
        }), (error) => error === rollback);
        assert.deepEqual((await rows().orderBy(Entry.id)).map((row: Any) => row.name), ["one", "two"]);
      } finally { await db.dropTables(); await db.close(); }
    });
  }
  test(`${dialect} lazy writes built in a closed transaction run in autocommit`, async () => {
    const { db, Entry, rows } = await setup(url);
    try {
      await rows().insertMany([{ id: 1, name: "one" }, { id: 2, name: "two" }]);
      for (const build of Object.values(writes)) {
        await rows().delete();
        await rows().insertMany([{ id: 1, name: "one" }, { id: 2, name: "two" }]);
        let statement!: Promise<unknown>;
        await db.transaction(async () => { statement = build(Entry, rows); });
        await statement;
        assert.notDeepEqual((await rows().orderBy(Entry.id)).map((row: Any) => row.name), ["one", "two"]);
      }
      assert.equal(await rows().count(), 0);
    } finally { await db.dropTables(); await db.close(); }
  });
  test(`${dialect} transaction awaits returned reads and writes inside its store`, async () => {
    const { db, Entry, rows } = await setup(url);
    try {
      const rollback = new Error("rollback");
      await assert.rejects(db.transaction(() => rows().insert({ id: 1, name: "one" }).then(() => { throw rollback; })), (e) => e === rollback);
      assert.equal(await rows().count(), 0);
      const row = await db.transaction<Any>(() => rows().insert({ id: 1, name: "one" }));
      assert.equal(row.id, 1);
      // The callback returns the thenable itself, so its execution must remain in the store.
      const statement = new InspectedStatement(async () => {
        assert.ok(db.tx());
        return await rows().filter(Entry.id.eq(1));
      }, () => { throw new Error("unused inspection"); });
      assert.equal((await db.transaction(() => statement)).length, 1);
      if (dialect === "Postgres") assert.equal((await db.transaction<Any[]>(() => rows().lock())).length, 1);
    } finally { await db.dropTables(); await db.close(); }
  });
  test(`${dialect} every lazy write reads allowWrites at await`, async () => {
    const { db, Entry, rows } = await setup(url, true);
    try {
      for (const build of Object.values(writes)) {
        await allowWrites([Entry], () => rows().delete());
        await allowWrites([Entry], () => rows().insertMany([{ id: 1, name: "one" }, { id: 2, name: "two" }]));
        let escaped!: Promise<unknown>;
        await allowWrites([Entry], async () => { escaped = build(Entry, rows); });
        await assert.rejects(escaped, WriteProtected);
        const outside = build(Entry, rows);
        await allowWrites([Entry], () => outside);
      }
    } finally { await db.dropTables(); await db.close(); }
  });
}

test("inTransaction awaits a returned statement inside its transaction", async () => {
  const { db, rows } = await setup("sqlite://:memory:");
  const tx = await db.engine.begin(null);
  try {
    await inTransaction(db, tx, () => new InspectedStatement(async () => {
      assert.equal(db.tx(), tx);
      return await rows().insert({ id: 1, name: "one" });
    }, () => { throw new Error("unused inspection"); }));
  } finally { await tx.rollback(); await db.close(); }
});

test("debug runners await returned statements inside all their stores", async () => {
  const statement = new InspectedStatement(async () => {
    assert.equal(active(), true);
    const origin = capture()!;
    record("read", "SELECT 1", origin);
    record("read", "SELECT 1", origin);
  }, () => { throw new Error("unused inspection"); });
  await assert.rejects(debug.nPlusOne(() => statement, { threshold: 1, fail: true }), debug.NPlusOne);
  const seen = new Set<string>();
  await debug.nPlusOne(() => internalLoop(seen, () => new InspectedStatement(async () => {
    const origin = capture()!;
    record("read", "SELECT 1", origin);
    record("read", "SELECT 1", origin);
  }, () => { throw new Error("unused inspection"); })), { threshold: 1, fail: true });
  assert.deepEqual([...seen], ["read"]);
  await debug.nPlusOne(() => relationLoad("Post", "author", "load", () => new InspectedStatement(async () => {
    assert.equal(capture()!.fix, "load(Post.author)");
  }, () => { throw new Error("unused inspection"); })));
});

test("query hooks await returned statements with hook tracing suppressed", async () => {
  const { db, rows } = await setup("sqlite://:memory:");
  let calls = 0;
  const remove = db.onQuery(() => {
    calls++;
    if (calls > 1) return;
    return new InspectedStatement(async () => { await rows().count(); }, () => { throw new Error("unused inspection"); });
  });
  try {
    await rows().insert({ id: 1, name: "one" });
    assert.equal(calls, 1);
  } finally { remove(); await db.close(); }
});
