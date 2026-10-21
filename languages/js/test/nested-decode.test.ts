/** Nested leaves selected, decoded and checked on JSON and columns storage (PostgreSQL and SQLite). */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, DecodeError, loads, Registry } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const SCHEMA = `
enum Color {
  RED
  GREEN
  @@storage(text)
}

type Leaves {
  price Decimal
  at    DateTime
  big   BigInt
  color Color
}

type Plain {
  at    DateTime
  big   BigInt
  color Color
}

model TJ {
  id   Int    @id
  item Leaves @storage(json)
  @@map("tj")
}

model TC {
  id   Int    @id
  item Plain
  @@map("tc")
}

`;

const LISTS = `
type Tagged {
  tags String[]
}

model TL {
  id   Int     @id
  item Tagged? @storage(json)
  @@map("tl")
}

model TLC {
  id   Int     @id
  item Tagged?
  @@map("tlc")
}
`;

const SQLITE = 'datasource db { provider = "sqlite" }\n';
const WHEN = new Date("2024-01-02T03:04:05.000Z");
const BIG = 2n ** 60n + 1n;
const PLAIN = { at: WHEN, big: BIG, color: "RED" };

/* eslint-disable @typescript-eslint/no-explicit-any */
type Any = any;

async function world(kind: "postgres" | "sqlite", schema: string) {
  const registry = new Registry();
  const models = loads((kind === "sqlite" ? SQLITE : "") + schema, { registry }) as Any;
  const db = await connect(kind === "sqlite" ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false, maxConnections: 2 });
  await db.dropTables();
  await db.createTables();
  return { models, db };
}

async function scenario(kind: "postgres" | "sqlite", body: (m: Any, db: Any) => Promise<void>, schema = SCHEMA) {
  const { models, db } = await world(kind, schema);
  try {
    await body(models, db);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

for (const kind of ["postgres", "sqlite"] as const) {
  test(`select of every leaf type on json storage (${kind})`, async () => {
    await scenario(kind, async (m, db) => {
      const J = m.TJ;
      await J.objects.using(db).insertMany([{ id: 1, item: { ...PLAIN, price: "12.50" } }]);
      const [{ price, at, big, color }] = await J.objects.using(db).select({ price: J.item.price, at: J.item.at, big: J.item.big, color: J.item.color });
      assert.equal(price.toString(), "12.5");
      assert.equal(at.getTime(), WHEN.getTime());
      assert.equal(big, BIG);
      assert.equal(color, "RED");
      const C = m.TC;
      await C.objects.using(db).insertMany([{ id: 1, item: PLAIN }]);
      assert.deepEqual(await C.objects.using(db).select({ big: C.item.big, color: C.item.color }), [{ big: BIG, color: "RED" }]);
    });
  });

  test(`an unknown enum value in a json cell raises a decode error (${kind})`, async () => {
    await scenario(kind, async (m, db) => {
      const J = m.TJ;
      await J.objects.using(db).insertMany([{ id: 1, item: { ...PLAIN, price: "1" } }]);
      const cast = kind === "postgres" ? "::jsonb" : "";
      const payload = '{"price":"1","at":"2024-01-02T03:04:05.000000Z","big":"1","color":"BLUE"}';
      await db.execute(`UPDATE tj SET item = '${payload}'${cast} WHERE id = 1`);
      await assert.rejects(async () => J.objects.using(db).all(), (e: Error) => e instanceof DecodeError && /item\.color/.test(e.message));
      await assert.rejects(async () => J.objects.using(db).select({ color: J.item.color }), (e: Error) => e instanceof DecodeError && /item\.color/.test(e.message));
    });
  });

  test(`an unknown enum value is refused on write (${kind})`, async () => {
    await scenario(kind, async (m, db) => {
      await assert.rejects(m.TJ.objects.using(db).insert({ id: 1, item: { ...PLAIN, price: "1", color: "BLUE" } }), /not a value of Color|BLUE/);
      await assert.rejects(m.TC.objects.using(db).insert({ id: 1, item: { ...PLAIN, color: "BLUE" } }), /not a value of Color|BLUE/);
    });
  });
}

for (const model of ["TL", "TLC"]) {
  test(`select of a list leaf (postgres, ${model})`, async () => {
    await scenario("postgres", async (m, db) => {
      const S = m[model];
      await S.objects.using(db).insertMany([{ id: 1, item: { tags: ["x", "y"] } }]);
      const [{ tags }] = await S.objects.using(db).select({ tags: S.item.tags });
      assert.deepEqual([...tags], ["x", "y"]);
    }, LISTS);
  });
}
