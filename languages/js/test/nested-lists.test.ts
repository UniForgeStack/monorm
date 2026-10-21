/** List subfields of a nested type on columns and JSON storage (PostgreSQL: SQLite has no list columns). */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, loads, Registry } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const SCHEMA = `
type Item {
  name String
  tags String[]
  ids  BigInt[]
}

model LJ {
  id   Int   @id
  item Item? @storage(json)
  @@map("lj")
}

model LC {
  id   Int   @id
  item Item?
  @@map("lc")
}
`;

const BIG = 2n ** 60n + 1n;

/* eslint-disable @typescript-eslint/no-explicit-any */
type Any = any;

for (const model of ["LJ", "LC"]) {
  const scenario = (what: string, body: (S: Any, q: Any, Item: Any) => Promise<void>): void => {
    test(`nested lists ${what} (postgres, ${model})`, async () => {
      const registry = new Registry();
      const models = loads(SCHEMA, { registry }) as Any;
      const db = await connect(DATABASE_URL, { registry, default: false, maxConnections: 2 });
      await db.dropTables();
      await db.createTables();
      try {
        const S = models[model];
        await body(S, S.objects.using(db), models.$types.Item);
      } finally {
        await db.dropTables();
        await db.close();
      }
    });
  };

  scenario("a list subfield on columns storage round-trips", async (S, q, Item) => {
    await q.insertMany([
      { id: 1, item: new Item({ name: "a", tags: ["x", "y"], ids: [1n] }) },
      { id: 2, item: { name: "b", tags: ["z"], ids: [2n] } },
    ]);
    const rows = new Map<number, Any>((await q.orderBy(S.id)).map((s: Any) => [s.id, s.item]));
    assert.deepEqual([...rows.get(1).tags], ["x", "y"]);
    assert.deepEqual([...rows.get(2).tags], ["z"]);
  });

  scenario("a list of typed elements keeps them exact", async (_S, q) => {
    await q.insertMany([{ id: 1, item: { name: "a", tags: [], ids: [BIG, 3n] } }]);
    const [row] = await q;
    assert.deepEqual([...row.item.ids], [BIG, 3n]);
  });

  scenario("an empty list round-trips", async (_S, q, Item) => {
    await q.insertMany([{ id: 1, item: new Item({ name: "a", tags: [], ids: [] }) }]);
    const [row] = await q;
    assert.equal(row.item.tags.length, 0);
    assert.equal(row.item.ids.length, 0);
  });
}
