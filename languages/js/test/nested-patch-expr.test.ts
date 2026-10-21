/** An expression as the value of a subfield in a JSON patch, on PostgreSQL and SQLite. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, Decimal, loads, Registry } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const SQLITE = 'datasource db { provider = "sqlite" }\n';
const WHEN = new Date("2024-01-02T03:04:05.000Z");
const LATER = new Date("2025-06-07T08:09:10.000Z");

/* eslint-disable @typescript-eslint/no-explicit-any */
type Any = any;

function schema(sqlite: boolean): string {
  const rent = sqlite ? "" : "  rent Decimal\n";
  const srcRent = sqlite ? "" : "  rentsrc Decimal\n";
  const model = (name: string, storage: string): string => `
model ${name} {
  id      Int     @id
  address Address? ${storage}
  bigsrc  BigInt
  opensrc Boolean
  datesrc DateTime
${srcRent}  metasrc Json
  @@map("${name.toLowerCase()}")
}
`;
  return `
type Address {
  city    String
  country String
  floor   Int
  big     BigInt
  open    Boolean
  opened  DateTime
${rent}  meta    Json
}
${model("JShop", "@storage(json)")}${model("CShop", "")}`;
}

for (const kind of ["postgres", "sqlite"] as const) {
  for (const model of ["JShop", "CShop"]) {
    const sqlite = kind === "sqlite";
    const scenario = (what: string, body: (S: Any, q: Any) => Promise<void>): void => {
      test(`nested patch expression ${what} (${kind}, ${model})`, async () => {
        const registry = new Registry();
        const models = loads((sqlite ? SQLITE : "") + schema(sqlite), { registry }) as Any;
        const db = await connect(sqlite ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false, maxConnections: 2 });
        try {
          await db.dropTables();
          await db.createTables();
          const S = models[model];
          const q = S.objects.using(db);
          const address = { city: "Oslo", country: "Norway", floor: 1, big: 1n, open: false, opened: WHEN, meta: { old: true }, ...(sqlite ? {} : { rent: new Decimal("1.50") }) };
          const src = { bigsrc: 2n ** 60n, opensrc: true, datesrc: LATER, metasrc: [1, 2], ...(sqlite ? {} : { rentsrc: new Decimal("12.50") }) };
          await q.insertMany([{ id: 1, address: new models.$types.Address(address), ...src }]);
          await body(S, q);
        } finally {
          await db.dropTables();
          await db.close();
        }
      });
    };

    scenario("takes a column for each leaf type", async (S, q) => {
      const patch: Record<string, unknown> = { big: S.bigsrc, open: S.opensrc, opened: S.datesrc, meta: S.metasrc };
      if (!sqlite) patch["rent"] = S.rentsrc;
      await q.filter(S.id.eq(1)).update({ address: patch });
      const a = (await q.get(S.id.eq(1))).address;
      assert.equal(a.big, 2n ** 60n);
      assert.equal(a.open, true);
      assert.equal(a.opened.getTime(), LATER.getTime());
      assert.deepEqual(a.meta, [1, 2]);
      assert.deepEqual([a.city, a.country, a.floor], ["Oslo", "Norway", 1]);
      if (!sqlite) assert.ok(a.rent.eq("12.50"));
    });

    scenario("takes an expression over a subfield", async (S, q) => {
      await q.filter(S.id.eq(1)).update({ address: { city: S.address.country, floor: S.address.floor.add(41) } });
      const a = (await q.get(S.id.eq(1))).address;
      assert.deepEqual([a.city, a.floor, a.country], ["Norway", 42, "Norway"]);
    });

    scenario("swaps two leaves", async (S, q) => {
      await q.filter(S.id.eq(1)).update({ address: { city: S.address.country, country: S.address.city } });
      const a = (await q.get(S.id.eq(1))).address;
      assert.deepEqual([a.city, a.country], ["Norway", "Oslo"]);
    });
  }
}
