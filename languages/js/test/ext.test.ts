import assert from "node:assert/strict";
import { test } from "node:test";
import { connect } from "../src/index.js";
import { behaviorValue, databaseOf, hardDeleteStatement, LANGUAGE_CONTRACT } from "../src/ext.js";
import { Author, sqliteRegistry } from "./sqlite/models.js";

test("the hard-delete statement is a delete of the query set", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const authors = Author.objects.using(db);
    await authors.insertMany([1, 2, 3].map((n) => ({ email: `a${n}`, name: `A${n}` })));
    assert.equal(await hardDeleteStatement(authors.filter(Author.email.eq("a1"))), 1);
    const [gone] = (await hardDeleteStatement(authors.filter(Author.email.eq("a2")), { returning: true })) as { email: string }[];
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
