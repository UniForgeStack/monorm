/** Nested fields at the bulk entry points: updateMany replaces, getOrInsert looks up plain fields. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, loads, Registry, type Database } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const SCHEMA = `
type Geo {
  lat Float
  lng Float?
}

type Address {
  city   String
  floor  Int @default(7)
  big    BigInt
  open   Boolean
  opened DateTime
  geo    Geo?
}

model JShop {
  id      Int      @id
  address Address? @storage(json)
  @@map("jshop")
}

model CShop {
  id      Int      @id
  address Address?
  @@map("cshop")
}
`;

const SQLITE = 'datasource db { provider = "sqlite" }\n';
const WHEN = new Date("2024-01-02T03:04:05.000Z");

/* eslint-disable @typescript-eslint/no-explicit-any */
type Any = any;

async function world(kind: "postgres" | "sqlite"): Promise<{ db: Database; classes: Any }> {
  const registry = new Registry();
  const classes = loads((kind === "sqlite" ? SQLITE : "") + SCHEMA, { registry }) as Any;
  const db = await connect(kind === "sqlite" ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false, maxConnections: 2 });
  await db.dropTables();
  await db.createTables();
  return { db, classes };
}

for (const kind of ["postgres", "sqlite"] as const) {
  for (const model of ["JShop", "CShop"]) {
    const scenario = (what: string, body: (S: Any, q: Any, A: Any) => Promise<void>): void => {
      test(`nested entry points: ${what} (${kind}, ${model})`, async () => {
        const w = await world(kind);
        try {
          const S = w.classes[model];
          await body(S, S.objects.using(w.db), w.classes.$types.Address);
        } finally {
          await w.db.dropTables();
          await w.db.close();
        }
      });
    };

    scenario("updateMany refuses an object for a nested field", async (S, q, A) => {
      const mk = (city: string): Any => new A({ city, big: 1n, open: true, opened: WHEN });
      await q.insertMany([{ id: 1, address: mk("Oslo") }, { id: 2, address: null }]);
      const before = (await q.orderBy(S.id)).map((s: Any) => s.address);
      const full = { city: "Full", big: 1n, open: false, opened: WHEN };
      for (const given of [{ city: "Partial" }, full]) {
        await assert.rejects(q.updateMany([{ id: 1, address: given }]), /address is an object.*update\(\)/);
      }
      assert.deepEqual((await q.orderBy(S.id)).map((s: Any) => s.address), before);
      await q.updateMany([{ id: 1, address: mk("Many") }, { id: 2, address: null }]);
      const rows = await q.orderBy(S.id);
      assert.equal(rows[0].address.city, "Many");
      assert.equal(rows[1].address, null);
    });

    scenario("getOrInsert rejects a nested key", async (_S, q, A) => {
      const a = new A({ city: "Oslo", big: 1n, open: true, opened: WHEN });
      for (const key of [a, { city: "Oslo" }, null]) {
        await assert.rejects(q.getOrInsert({ address: key }), /address is a nested field/);
      }
    });
  }
}
