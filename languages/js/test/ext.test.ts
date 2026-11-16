import assert from "node:assert/strict";
import { test } from "node:test";
import { connect } from "../src/index.js";
import { behaviorValue, canonicalHash, databaseOf, hardDeleteStatement, jsonDiff, LANGUAGE_CONTRACT } from "../src/ext.js";
import { Author, sqliteRegistry } from "./sqlite/models.js";

test("the hard-delete statement is a delete of the query set", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const authors = Author.objects.using(db);
    await authors.insertMany([1, 2, 3].map((n) => ({ email: `a${n}`, name: `A${n}` })));
    assert.equal(await hardDeleteStatement(authors.filter(Author.email.eq("a1"))), 1);
    const [gone] = (await hardDeleteStatement(authors.filter(Author.email.eq("a2"))).returning()) as { email: string }[];
    assert.equal(gone!.email, "a2");
    assert.deepEqual((await authors.all()).map((a) => a.email), ["a3"]);
  } finally {
    await db.close();
  }
  assert.equal(LANGUAGE_CONTRACT, 1);
});

test("a behavior value is undefined without a record", () => {
  assert.equal(behaviorValue("model_overrides", sqliteRegistry), undefined);
  assert.equal(behaviorValue("model_overrides"), undefined);
});

test("databaseOf gives the database that loaded the instance", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    await Author.objects.using(db).insertMany([{ email: "a1", name: "A1" }]);
    const [author] = await Author.objects.using(db).all();
    assert.equal(databaseOf(author!), db);
    assert.equal(databaseOf({}), undefined);
  } finally {
    await db.close();
  }
});

test("canonical hash ignores key order and matches fixed vectors", () => {
  assert.equal(canonicalHash({}), "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a");
  assert.equal(canonicalHash({ b: [1, 2], a: "x" }), "721ef82f2d6c0997bffb7a8ab3f40f8fb45b0b52ce2af3afa6b0f05efbdc317f");
  assert.equal(canonicalHash({ a: "x", b: [1, 2] }), canonicalHash({ b: [1, 2], a: "x" }));
});

test("json diff lists changed added and removed keys with a null for a missing side", () => {
  const old = { a: 1, b: 2, c: 3 };
  assert.deepEqual(jsonDiff(old, { a: 1, b: 5, d: 4 }), { b: { old: 2, new: 5 }, c: { old: 3, new: null }, d: { old: null, new: 4 } });
  assert.deepEqual(jsonDiff(old, old), {});
});
