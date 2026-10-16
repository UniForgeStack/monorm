/** LIKE on a `char(n)` column compares the text, not the space-padded value (Postgres). */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, loads, Registry } from "../src/index.js";

test("like on a char column compares the text: postgres", async () => {
  const registry = new Registry();
  const { Padded } = loads('model Padded {\n  id Int @id\n  k String @db.Char(6)\n  @@map("char_like_js_padded")\n}', { registry }) as any;
  const db = await connect(process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test", { registry, default: false });
  await db.dropTables(); await db.createTables();
  const qs = Padded.objects.using(db);
  const ids = async (cond: unknown) => (await qs.filter(cond).select({ id: Padded.id }).scalars()).map(Number).sort();
  try {
    await qs.insertMany(["ab", "abc", "a c"].map((k, i) => ({ id: i + 1, k })));
    const k = Padded.k;
    assert.deepEqual(await ids(k.endsWith("b")), [1]);
    assert.deepEqual(await ids(k.like("ab")), [1]);
    assert.deepEqual(await ids(k.ilike("%B")), [1]);
    assert.deepEqual(await ids(k.contains(" ")), [3]);
    assert.deepEqual(await ids(k.startsWith("ab")), [1, 2]);
    assert.deepEqual(await ids(k.like("ab").not()), [2, 3]);
  } finally {
    await db.dropTables(); await db.close();
  }
});
