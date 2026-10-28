/** Value conversion of every column type, on a schema loaded at runtime (`loads()`,
 * untyped) with a registry of its own. */

import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import { Decimal, Registry, connect, loads, type Database, type ModelClass, type ModelSpec } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const SCHEMA = `
model Doc {
  id      String    @id @default(dbgenerated("gen_random_uuid()")) @db.Uuid
  ref     String?   @db.Uuid
  extra   Json?
  data    Json      @default("{\\"a\\": 1}")
  day     DateTime? @db.Date
  at      DateTime?
  big     BigInt?
  small   Int?
  ratio   Float?
  amount  Decimal?  @db.Decimal(20, 6)
  flag    Boolean?
  note    String?
  ids     BigInt[]   @default([])
  days    DateTime[] @default([]) @db.Date
  @@map("value_docs")
}
`;

const registry = new Registry();
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const Doc = loads(SCHEMA, { registry })["Doc"] as ModelClass<ModelSpec> & Record<string, any>;
let db: Database;

before(async () => {
  db = await connect(DATABASE_URL, { default: false, registry, maxConnections: 2 });
  await db.dropTables();
  await db.createTables();
});

after(async () => {
  await db.dropTables();
  await db.close();
});

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const objects = () => Doc.objects.using(db) as any;

test("every column type round-trips", async () => {
  const at = new Date("2026-01-02T03:04:05.678Z");
  const doc = await objects().insert({
    data: { list: [1, 2.5, "x", null, { deep: true }] },
    day: new Date("2026-03-04T00:00:00Z"),
    at,
    big: 9007199254740993n, // past 2^53: exact as a bigint
    small: -7,
    ratio: 0.25,
    amount: new Decimal("123456789.123456"),
    flag: true,
    ids: [1n, 2, 3n],
    days: [new Date("2026-01-01T12:00:00Z")],
  });
  assert.match(doc.id, /^[0-9a-f-]{36}$/);
  assert.deepEqual(doc.data, { list: [1, 2.5, "x", null, { deep: true }] });
  assert.equal(doc.day.toISOString(), "2026-03-04T00:00:00.000Z");
  assert.equal(doc.at.getTime(), at.getTime());
  assert.equal(doc.big, 9007199254740993n);
  assert.equal(doc.small, -7);
  assert.equal(doc.ratio, 0.25);
  assert.ok(doc.amount.eq("123456789.123456"));
  assert.equal(doc.flag, true);
  assert.deepEqual(doc.ids, [1n, 2n, 3n]);
  assert.equal(doc.days[0].toISOString(), "2026-01-01T00:00:00.000Z"); // the UTC day
  // the defaults and NULLs
  const empty = await objects().insert({});
  assert.deepEqual(empty.data, { a: 1 });
  assert.deepEqual([empty.day, empty.at, empty.big, empty.amount, empty.flag], [null, null, null, null, null]);
  // filters by every type
  const one = (q: unknown) => objects().filter(q).count();
  assert.equal(await one(Doc.id.eq(doc.id)), 1);
  assert.equal(await one(Doc.big.eq(9007199254740993n)), 1);
  assert.equal(await one(Doc.at.eq(at)), 1);
  assert.equal(await one(Doc.day.eq(new Date("2026-03-04T00:00:00Z"))), 1);
  assert.equal(await one(Doc.amount.gt(100)), 1);
  assert.equal(await one(Doc.ids.has(2)), 1);
  assert.equal(await one(Doc.data.eq({ a: 1 })), 1);
});

test("fetched rows decode every type, nulls in every column, through the packed rows", async () => {
  await objects().delete();
  const micros = new Date("2026-05-06T07:08:09.123Z");
  Object.defineProperty(micros, "monorm:micros", { value: 456 });
  Object.defineProperty(micros, "monorm:micros-ms", { value: micros.getTime() });
  await objects().insertMany([
    {
      data: { list: [1, 2.5, "é", null, { deep: true }], s: "日本", ["__proto__"]: 7 },
      day: new Date("2026-03-04T00:00:00Z"),
      at: micros,
      ref: "12345678-1234-1234-1234-123456789abc",
      extra: { nested: true },
      big: -9007199254740993n,
      small: 2147483647,
      ratio: -0.5,
      amount: new Decimal("-0.000001"),
      flag: false,
      note: "naïve ☃ text",
      ids: [1n, 2n],
      days: [new Date("2026-01-01T00:00:00Z"), new Date("2026-01-02T00:00:00Z")],
    },
    { note: "", flag: true, at: new Date("2026-01-01T00:00:00Z") },
    { note: "ASCII text" },
    {},
  ]);
  // PostgreSQL allows NULL array elements even though model writes reject them.
  await db.execute("UPDATE value_docs SET ids = ARRAY[1::bigint, NULL, 2::bigint] WHERE note = 'naïve ☃ text'");
  const rows = (await objects().fetch()) as Record<string, any>[];
  assert.equal(rows.length, 4);
  const byNote = (note: string | null) => rows.find((r) => r.note === note)!;
  const [none, blank, full] = [byNote(null), byNote(""), byNote("naïve ☃ text")];
  // Cover SQL NULL separately from JSON null and empty arrays.
  for (const r of [none, blank]) {
    assert.deepEqual([r.day, r.big, r.small, r.ratio, r.amount, r.ref, r.extra], [null, null, null, null, null, null, null]);
    assert.deepEqual([r.ids, r.days, r.data], [[], [], { a: 1 }]);
    assert.match(r.id, /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
  }
  assert.equal(none.note, null);
  assert.equal(none.at, null);
  assert.equal(none.flag, null);
  assert.equal(blank.flag, true);
  assert.equal(blank.at.toISOString(), "2026-01-01T00:00:00.000Z");
  assert.equal(byNote("ASCII text").note, "ASCII text");
  assert.equal(full.ref, "12345678-1234-1234-1234-123456789abc");
  assert.deepEqual(full.extra, { nested: true });
  assert.equal(blank.note, "");
  assert.deepEqual(full.data.list, [1, 2.5, "é", null, { deep: true }]);
  assert.equal(full.data.s, "日本");
  assert.equal(Object.getOwnPropertyDescriptor(full.data, "__proto__")?.value, 7); // an own key, not the prototype
  assert.equal(full.day.toISOString(), "2026-03-04T00:00:00.000Z");
  assert.equal(full.at.getTime(), micros.getTime());
  assert.equal(full.at["monorm:micros"], 456);
  assert.equal(full.at["monorm:micros-ms"], micros.getTime());
  assert.equal(Object.keys(full.at).length, 0); // the hidden properties do not enumerate
  assert.equal(full.big, -9007199254740993n);
  assert.equal(full.small, 2147483647);
  assert.equal(full.ratio, -0.5);
  assert.ok(full.amount instanceof Decimal && full.amount.eq("-0.000001"));
  assert.equal(full.flag, false);
  assert.equal(full.note, "naïve ☃ text");
  assert.deepEqual(full.ids, [1n, null, 2n]);
  assert.deepEqual(full.days.map((d: Date) => d.toISOString()), ["2026-01-01T00:00:00.000Z", "2026-01-02T00:00:00.000Z"]);
});

test("wrong value types are TypeErrors", async () => {
  const bad = async (values: object, re: RegExp) =>
    assert.rejects(objects().insert(values), (e: unknown) => e instanceof TypeError && re.test(e.message));
  await bad({ big: 1.5 }, /expected an integer/);
  await bad({ big: 2 ** 60 }, /expected an integer/); // unsafe as a number
  await bad({ big: "1" }, /expected a bigint or an integer number/);
  await bad({ small: 2 ** 40 }, /int32 range/);
  await bad({ ratio: "1" }, /expected a number/);
  await bad({ flag: 1 }, /expected a boolean/);
  await bad({ at: "2026-01-01" }, /expected a Date/);
  await bad({ at: new Date("nope") }, /invalid Date/);
  await bad({ id: "not-a-uuid" }, /invalid UUID/);
  await bad({ amount: Infinity }, /finite decimal/);
  await bad({ ids: 1n }, /expected an array/);
  await bad({ data: () => 1 }, /JSON value/);
});
