import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";
import { behaviorField, behaviorRecords, behaviorValue, canonicalHash, databaseOf, hardDeleteStatement, jsonDiff, LANGUAGE_CONTRACT } from "../src/ext.js";
import { Author, sqliteRegistry } from "./sqlite/models.js";
import { Post, User } from "./blog/models.js";

const soft = { skip: !hasExtension("monorm.soft-delete") };
const SOFT = `datasource db { provider = "sqlite" }
model Doc {
  id Int @id
  title String
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("ext_doc")
}`;

test("the hard-delete statement is a hard delete of the query set", soft, async () => {
  const registry = new Registry();
  const { Doc } = loads(SOFT, { registry }) as any;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const docs = Doc.objects.using(db);
    await docs.insertMany([1, 2, 3, 4].map((n) => ({ id: n, title: `d${n}` })));
    // The plain delete is the soft delete: the row stays.
    await docs.filter(Doc.id.eq(4)).delete();
    assert.deepEqual((await docs.allWithDeleted().orderBy(Doc.id)).map((d: any) => d.id), [1, 2, 3, 4]);
    assert.equal(await hardDeleteStatement(docs.filter(Doc.id.eq(1))), 1);
    const [gone] = (await hardDeleteStatement(docs.filter(Doc.id.eq(2))).returning()) as { id: number }[];
    assert.equal(gone!.id, 2);
    assert.deepEqual((await docs.allWithDeleted().orderBy(Doc.id)).map((d: any) => d.id), [3, 4]);
    assert.deepEqual((await docs.all()).map((d: any) => d.id), [3]);
  } finally {
    await db.close();
  }
  assert.equal(LANGUAGE_CONTRACT, 1);
});

test("a behavior value is absent without a record", () => {
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

test("behavior field takes the IR field name", () => {
  const record = { model: "User", field: "created_at" };
  assert.equal(behaviorField(User._meta, record).name, "createdAt");
});

test("behavior lookup is empty without records", () => {
  assert.deepEqual(behaviorRecords(Post.objects, "delete_rewrites"), []);
  assert.equal(LANGUAGE_CONTRACT, 1);
});
