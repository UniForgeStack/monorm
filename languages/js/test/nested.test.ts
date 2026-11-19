/** Nested types through the TypeScript API: one scenario on JSON and columns storage, PostgreSQL and SQLite. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, DecodeError, loads, NotLoaded, Registry, type Database } from "../src/index.js";
import { extraMicros, keepMicros } from "../src/date-micros.js";
import { Decimal } from "../src/decimal.js";
import { DATABASE_URL } from "./helpers.js";
import { Address as ExampleAddress, Geo as ExampleGeo, Shop as ExampleShop } from "./nested/models.js";

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
const LATER = new Date("2025-06-07T08:09:10.000Z");

/* eslint-disable @typescript-eslint/no-explicit-any */
type Any = any;

interface World {
  db: Database;
  classes: Any;
  Geo: Any;
  Address: Any;
}

async function world(kind: "postgres" | "sqlite"): Promise<World> {
  const registry = new Registry();
  const models = loads((kind === "sqlite" ? SQLITE : "") + SCHEMA, { registry }) as Any;
  const db = await connect(kind === "sqlite" ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false, maxConnections: 2 });
  await db.dropTables();
  await db.createTables();
  return { db, classes: models, Geo: models.$types.Geo, Address: models.$types.Address };
}

const address = (w: World, over: Record<string, unknown> = {}): Any =>
  new w.Address({ city: "Oslo", big: 2n ** 60n, open: true, opened: WHEN, geo: new w.Geo({ lat: 1.5 }), ...over });

async function seed(w: World, S: Any): Promise<void> {
  await S.objects.using(w.db).insertMany([
    { id: 1, address: address(w) },
    { id: 2, address: address(w, { city: "Bergen", floor: 3, geo: new w.Geo({ lat: 9.0, lng: 2.5 }) }) },
    { id: 3, address: address(w, { city: "Tromso", geo: null }) },
    { id: 4, address: null },
  ]);
}

for (const kind of ["postgres", "sqlite"] as const) {
  for (const model of ["JShop", "CShop"]) {
    const name = (what: string): string => `nested ${what} (${kind}, ${model})`;
    const scenario = (what: string, body: (w: World, S: Any, q: Any) => Promise<void>, timeout?: number): void => {
      test(name(what), { timeout }, async () => {
        const w = await world(kind);
        try {
          const S = w.classes[model];
          await body(w, S, S.objects.using(w.db));
        } finally {
          await w.db.dropTables();
          await w.db.close();
        }
      });
    };
    const ids = async (q: Any): Promise<number[]> => (await q).map((s: Any) => s.id);

    scenario("insert round-trips values and nulls", async (w, S, q) => {
      await seed(w, S);
      const rows = new Map<number, Any>((await q.orderBy(S.id)).map((s: Any) => [s.id, s.address]));
      assert.deepEqual(rows.get(1), address(w));
      assert.ok(rows.get(1).equals(address(w)));
      assert.equal(rows.get(1).floor, 7);
      assert.equal(rows.get(1).big, 2n ** 60n);
      assert.equal(rows.get(1).opened.getTime(), WHEN.getTime());
      assert.ok(rows.get(1).geo.equals(new w.Geo({ lat: 1.5, lng: null })));
      assert.equal(rows.get(2).geo.lng, 2.5);
      assert.equal(rows.get(3).geo, null);
      assert.equal(rows.get(4), null);
    });

    scenario("a timestamp subfield keeps its microseconds, as a flat field does", async (w, S, q) => {
      const opened = new Date("2024-01-02T03:04:05.123Z");
      keepMicros(opened, 456);
      await q.insert({ id: 1, address: address(w, { opened }) });
      const read = (await q.get(S.id.eq(1))).address.opened;
      assert.equal(read.getTime(), opened.getTime());
      assert.equal(extraMicros(read), 456);
      // A read value goes back unchanged.
      await q.insert({ id: 2, address: address(w, { opened: read }) });
      assert.equal(extraMicros((await q.get(S.id.eq(2))).address.opened), 456);
      // A time that changed after the microseconds were set drops them.
      const moved = new Date(opened);
      keepMicros(moved, 456);
      moved.setTime(moved.getTime() + 1);
      await q.insert({ id: 3, address: address(w, { opened: moved }) });
      assert.equal(extraMicros((await q.get(S.id.eq(3))).address.opened), 0);
    });

    scenario("insert takes subfields with defaults and rejects a bad one", async (_w, _S, q) => {
      const one = await q.insert({ id: 1, address: { city: "A", big: 1n, open: false, opened: WHEN } });
      assert.equal(one.address.floor, 7);
      assert.equal(one.address.geo, null);
      await assert.rejects(q.insert({ id: 2, address: { big: 1n, open: false, opened: WHEN } }), /address\.city/);
      await assert.rejects(q.insert({ id: 2, address: { nope: 1 } }), /no subfield/);
      await assert.rejects(q.insert({ id: 2, address: { city: 5, big: 1n, open: false, opened: WHEN } }), /expected/);
    });

    scenario("filter by subfield, whole value and null", async (w, S, q) => {
      await seed(w, S);
      const pks = (cond: Any): Promise<number[]> => ids(q.filter(cond).orderBy(S.id));
      assert.deepEqual(await pks(S.address.city.eq("Oslo")), [1]);
      assert.deepEqual(await pks(S.address.floor.lt(7)), [2]);
      assert.deepEqual(await pks(S.address.geo.lat.gt(1)), [1, 2]);
      assert.deepEqual(await pks(S.address.geo.lng.eq(2.5)), [2]);
      assert.deepEqual(await pks(S.address.opened.eq(WHEN)), [1, 2, 3]);
      assert.deepEqual(await pks(S.address.opened.gt(WHEN)), []);
      assert.deepEqual(await pks(S.address.opened.in([WHEN])), [1, 2, 3]);
      assert.deepEqual(await pks(S.address.opened.notIn([WHEN]).andNotNull()), []);
      assert.deepEqual(await pks(S.address.isNull()), [4]);
      assert.deepEqual(await pks(S.address.isNotNull()), [1, 2, 3]);
      assert.deepEqual(await pks(S.address.geo.isNull()), [3, 4]);
      assert.deepEqual(await pks(S.address.eq(address(w))), [1]);
      assert.deepEqual(await pks(S.address.geo.eq({ lat: 1.5 })), [1]);
      assert.deepEqual(await pks(S.address.geo.eq(new w.Geo({ lat: 9.0, lng: 2.5 }))), [2]);
    });

    scenario("ne on a whole field is three-valued", async (w, S, q) => {
      await seed(w, S);
      // The null row matches neither eq nor ne.
      assert.deepEqual(await ids(q.filter(S.address.ne(address(w)).andNotNull()).orderBy(S.id)), [2, 3]);
    });

    scenario("order by a subfield and never by a whole field", async (w, S, q) => {
      await seed(w, S);
      assert.deepEqual(await ids(q.filter(S.address.isNotNull()).orderBy(S.address.city)), [2, 1, 3]);
      assert.deepEqual(await ids(q.filter(S.address.geo.lat.gt(0)).orderBy(S.address.geo.lat.desc())), [2, 1]);
      await assert.rejects(Promise.resolve().then(() => q.orderBy(S.address)), /order/);
      await assert.rejects(Promise.resolve().then(() => q.orderBy(S.address.geo)), /order/);
    });

    scenario("select gives values and scalars", async (w, S, q) => {
      await seed(w, S);
      const rows = await q.select({ id: S.id, city: S.address.city, big: S.address.big, geo: S.address.geo, all: S.address }).orderBy(S.id);
      assert.equal(rows[0].city, "Oslo");
      assert.equal(rows[0].big, 2n ** 60n);
      assert.ok(rows[0].geo.equals(new w.Geo({ lat: 1.5 })));
      assert.ok(rows[0].all.equals(address(w)));
      assert.equal(rows[3].city, null);
      assert.equal(rows[3].geo, null);
      assert.equal(rows[3].all, null);
      assert.equal(rows[2].geo, null);
    });

    scenario("leaf names are hidden", async (w, S, q) => {
      await seed(w, S);
      const s = await q.get(S.id.eq(1));
      assert.deepEqual(Object.keys(s.toJSON()).sort(), ["address", "id"]);
      assert.ok(!String(JSON.stringify(s)).includes("address_city"));
      assert.ok(!("addressCity" in s));
      if (model === "CShop") {
        await assert.rejects(q.insert({ id: 9, addressCity: "x" }), /no field/);
        await assert.rejects(q.filter(S.id.eq(1)).update({ addressCity: "x" }), /no field/);
      }
    });

    scenario("patch changes only the named subfields", async (w, S, q) => {
      await seed(w, S);
      await q.filter(S.id.eq(1)).update({ address: { city: "Bergen", geo: { lng: 4.0 } } });
      let a = (await q.get(S.id.eq(1))).address;
      assert.equal(a.city, "Bergen");
      assert.equal(a.floor, 7);
      assert.equal(a.big, 2n ** 60n);
      assert.ok(a.geo.equals(new w.Geo({ lat: 1.5, lng: 4.0 })));
      await q.filter(S.id.eq(1)).update({ address: { floor: 9, opened: LATER } });
      a = (await q.get(S.id.eq(1))).address;
      assert.equal(a.floor, 9);
      assert.equal(a.opened.getTime(), LATER.getTime());
      assert.equal(a.city, "Bergen");
      // a null leaf: SQLite deletes the key, PostgreSQL writes JSON null; both read as null
      await q.filter(S.id.eq(1)).update({ address: { geo: { lng: null } } });
      assert.ok((await q.get(S.id.eq(1))).address.geo.equals(new w.Geo({ lat: 1.5, lng: null })));
    });

    scenario("patch validation", async (w, S, q) => {
      await seed(w, S);
      const patch = (address: Any): Promise<unknown> => Promise.resolve().then(() => q.filter(S.id.eq(1)).update({ address }));
      await assert.rejects(patch({}), /at least one/);
      await assert.rejects(patch({ city: null }), /not nullable/);
      await assert.rejects(patch({ zip: "1" }), /no subfield/);
      await assert.rejects(patch({ floor: "x" }), /expected/);
    }, 30_000);

    scenario("replace sets the whole value and resets omitted subfields", async (w, S, q) => {
      await seed(w, S);
      await q.filter(S.id.eq(2)).update({ address: address(w, { city: "Replaced", geo: null }) });
      const a = (await q.get(S.id.eq(2))).address;
      assert.ok(a.equals(address(w, { city: "Replaced", geo: null })));
      assert.equal(a.floor, 7);
      await q.filter(S.id.eq(4)).update({ address: address(w, { city: "Filled" }) }); // replace sets a null field
      assert.equal((await q.get(S.id.eq(4))).address.city, "Filled");
    });

    scenario("null clears the whole value", async (w, S, q) => {
      await seed(w, S);
      await q.filter(S.id.eq(1)).update({ address: null });
      assert.equal((await q.get(S.id.eq(1))).address, null);
      assert.equal(await q.filter(S.address.isNull()).count(), 2);
      await q.filter(S.id.eq(2)).update({ address: { geo: null } });
      assert.equal((await q.get(S.id.eq(2))).address.geo, null);
    });

    scenario("updateMany and instance update", async (w, S, q) => {
      await seed(w, S);
      await q.updateMany([{ id: 1, address: address(w, { city: "Many" }) }, { id: 2, address: null }]);
      assert.equal((await q.get(S.id.eq(1))).address.city, "Many");
      assert.equal((await q.get(S.id.eq(2))).address, null);
      const s = await q.get(S.id.eq(3));
      await s.update({ address: { city: "Inst" } });
      assert.equal(s.address.city, "Inst");
      assert.equal((await q.get(S.id.eq(3))).address.city, "Inst");
    });

    scenario("load keeps a group whole", async (w, S, q) => {
      await seed(w, S);
      const only = await q.load(S.address.city).orderBy(S.id);
      assert.ok(only[0].address.equals(address(w)));
      assert.throws(() => only[0].id, NotLoaded);
      const whole = await q.load(S.address).orderBy(S.id);
      assert.ok(whole[0].address.equals(address(w)));
      assert.equal(whole[3].address, null);
    });

    scenario("two levels deep with a nullable middle level", async (w, S, q) => {
      await seed(w, S);
      await q.filter(S.address.geo.lat.eq(9.0)).update({ address: { geo: { lat: 10.0 } } });
      assert.ok((await q.get(S.id.eq(2))).address.geo.equals(new w.Geo({ lat: 10.0, lng: 2.5 })));
      await q.filter(S.id.eq(3)).update({ address: { geo: new w.Geo({ lat: 0.5 }) } });
      assert.ok((await q.get(S.id.eq(3))).address.geo.equals(new w.Geo({ lat: 0.5 })));
      await q.filter(S.id.eq(3)).update({ address: { geo: null } });
      assert.equal((await q.get(S.id.eq(3))).address.geo, null);
    });
  }

  // case: nested::value_classes_are_immutable_and_equal_by_value
  test(`value classes are immutable and equal by value (${kind})`, async () => {
    const w = await world(kind);
    try {
      const a = address(w);
      assert.ok(Object.isFrozen(a));
      assert.ok(a.equals(address(w)));
      assert.ok(!a.equals(address(w, { city: "Other" })));
      assert.throws(() => { a.city = "x"; }, TypeError);
      assert.deepEqual(JSON.parse(JSON.stringify(a)).city, "Oslo");
    } finally {
      await w.db.dropTables();
      await w.db.close();
    }
  });

  test(`a JSON cell that breaks the type raises DecodeError with the path (${kind})`, async () => {
    const w = await world(kind);
    try {
      const S = w.classes["JShop"];
      const q = S.objects.using(w.db);
      await w.db.execute(
        `INSERT INTO jshop (id, address) VALUES
          (1, '{"city": 5, "floor": 7, "big": "1", "open": true, "opened": "2024-01-02T03:04:05"}'),
          (2, '{"city": "x", "floor": 7, "big": "1", "open": true, "opened": "2024-01-02T03:04:05", "geo": {"lat": "no"}}'),
          (3, '{"city": "x", "floor": 7, "big": 9007199254740993, "open": true, "opened": "2024-01-02T03:04:05"}'),
          (4, '{"city": "x", "floor": 7, "big": "9007199254740993", "open": true, "opened": "2024-01-02T03:04:05.123456Z"}')`,
      );
      await assert.rejects(q.get(S.id.eq(1)), (e: unknown) => e instanceof DecodeError && /address\.city/.test(e.message) && e.path[0] === "city");
      await assert.rejects(q.get(S.id.eq(2)), (e: unknown) => e instanceof DecodeError && /address\.geo\.lat/.test(e.message));
      await assert.rejects(q.get(S.id.eq(3)), (e: unknown) => e instanceof DecodeError && /big/.test(e.message));
      const row = await q.get(S.id.eq(4));
      assert.equal(row.address.big, 9007199254740993n);
      assert.equal(row.address.opened.getTime(), Date.parse("2024-01-02T03:04:05.123Z"));
    } finally {
      await w.db.dropTables();
      await w.db.close();
    }
  });
}

test("JSON values keep decimal, bigint and datetime exact", () => {
  // Value conversion needs no database: BigInt above 2**53 and Decimal go as text, DateTime as UTC text.
  const registry = new Registry();
  loads(
    SQLITE +
      `
type Money {
  amount Decimal
  big    BigInt
  at     DateTime
}
model Wallet {
  id    Int   @id
  money Money @storage(json)
}
`,
    { registry },
  );
  const nt = registry.nestedType("Money");
  const out = nt.toJson({ amount: new Decimal("12345678901234567890.123456789"), big: 2n ** 62n, at: WHEN }, "Wallet.money");
  assert.deepEqual(out, { amount: "12345678901234567890.123456789", big: String(2n ** 62n), at: "2024-01-02T03:04:05.000000Z" });
  const back = nt.fromJson(out, "Wallet", "money") as Any;
  assert.ok(back.amount.equals(new Decimal("12345678901234567890.123456789")));
  assert.equal(back.big, 2n ** 62n);
  assert.equal(back.at.getTime(), WHEN.getTime());
  // the stored text of either storage decodes: PostgreSQL adds fraction digits, SQLite keeps what was written
  assert.equal((nt.fromJson({ ...out, at: "2024-01-02T03:04:05Z" }, "Wallet", "money") as Any).at.getTime(), WHEN.getTime());
  assert.throws(() => nt.fromJson({ ...out, amount: "abc" }, "Wallet", "money"), (e: unknown) => e instanceof DecodeError && /amount/.test(e.message));
});

test("generated value classes", () => {
  const address = new ExampleAddress({ city: "Oslo", big: 1n, opened: WHEN, geo: new ExampleGeo({ lat: 1.5 }) });
  assert.equal(address.floor, 7);
  assert.equal(address.geo?.lat, 1.5);
  assert.equal(address.geo?.lng, null);
  assert.deepEqual(address.toJSON()["city"], "Oslo");
  assert.equal(typeof ExampleShop.address.geo.lat.eq, "function");
});

test("nested types in a non-empty registry", async () => {
  const registry = new Registry();
  loads(SQLITE + "model Plain { id Int @id }", { registry });
  const classes = loads(SQLITE + SCHEMA, { registry }) as Any;
  assert.equal(typeof classes["CShop"].address.city.eq, "function");
  assert.ok(registry.nestedType("Address"));
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await classes["CShop"].objects.using(db).insert({ id: 1, address: { city: "Oslo", big: 1n, open: true, opened: WHEN } });
    assert.equal((await classes["CShop"].objects.using(db).get(classes["CShop"].id.eq(1))).address.city, "Oslo");
  } finally {
    await db.close();
  }
});
