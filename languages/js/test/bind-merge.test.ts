/** Equal bind values share a slot in ORDER BY / DISTINCT statements only when Postgres cannot tell them apart.
 * A JS `Decimal` loses its scale before it binds, so the scale cases live in languages/python/tests/test_grouped_expressions.py. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, func, loads, Registry } from "../src/index.js";

test("a float -0 keeps its own bind: postgres", async () => {
  const registry = new Registry();
  const { Num } = loads(
    'model Num {\n  id Int @id\n  f Float?\n  a Float @default(1)\n  b Float @default(1)\n  @@map("bind_merge_js_nums")\n}',
    { registry },
  ) as any;
  const db = await connect(process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test", { registry, default: false });
  await db.dropTables(); await db.createTables();
  const qs = Num.objects.using(db);
  try {
    await qs.insertMany([{ id: 1 }]);
    const [f] = await qs.select({ x: func.coalesce(Num.f, 0.0), y: func.coalesce(Num.f, -0.0) }).orderBy(Num.id);
    assert.deepEqual([Object.is(f.x, 0), Object.is(f.y, -0)], [true, true]);
    await qs.filter(Num.id.in(qs.select({ id: Num.id }).distinct())).update({ a: 0, b: -0 });
    assert.deepEqual(await db.fetchText("SELECT a::text, b::text FROM bind_merge_js_nums"), [["0", "-0"]]);
  } finally {
    await db.dropTables(); await db.close();
  }
});
