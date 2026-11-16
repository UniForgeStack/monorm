/** `@@scope.by`: one scope for the app and the database; needs the `monorm.scope` package. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, loads, QueryError, Registry, scope, ScopeMismatch, ScopeMissing, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const SCHEMA = `
model Order {
  id Int @id @default(autoincrement())
  shop_id Int
  total Int
  @@scope.by(shop: shop_id)
  @@map("sc_js_orders")
}
`;

const skip = !hasExtension("monorm.scope");
const urlOf = (provider: string) => (provider === "sqlite" ? "sqlite://:memory:" : (process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test"));

async function withOrders(provider: string, body: (db: Database, Order: Any) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${SCHEMA}`, { registry }) as Record<string, Any>;
  const db = await connect(urlOf(provider), { registry, default: false });
  await db.dropTables();
  await db.createTables();
  try {
    await body(db, models["Order"]);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

const rows = async (db: Database, Order: Any) =>
  ((await Order.objects.using(db).withoutDefaults()) as Any[]).map((r) => [r.shopId, r.total]).sort((a, b) => a[0] - b[0] || a[1] - b[1]);

for (const provider of ["sqlite", "postgresql"]) {
  const scenario = (what: string, body: (db: Database, Order: Any) => Promise<void>) => test(what, { skip }, () => withOrders(provider, body));

  scenario("an insert takes the scope value and reads see the scope", async (db, Order) => {
    const mine: Any = await scope({ shop: 1 }, () => Order.objects.using(db).insert({ total: 10 }));
    await scope({ shop: 2 }, () => Order.objects.using(db).insert({ total: 20 }));
    assert.equal(mine.shopId, 1);
    assert.deepEqual(await scope({ shop: 1 }, async () => (await Order.objects.using(db)).map((o: Any) => o.total)), [10]);
    assert.deepEqual(await scope({ shop: 2 }, async () => (await Order.objects.using(db)).map((o: Any) => o.total)), [20]);
    assert.deepEqual(await rows(db, Order), [[1, 10], [2, 20]]);
  });

  scenario("an explicit equal value passes and another value raises", async (db, Order) => {
    await scope({ shop: 1 }, async () => {
      await Order.objects.using(db).insert({ shopId: 1, total: 1 });
      await assert.rejects(Order.objects.using(db).insert({ shopId: 2, total: 2 }), (e: Any) => {
        assert.ok(e instanceof ScopeMismatch);
        assert.deepEqual([e.model, e.key, e.expected, e.actual], ["Order", "shop", "1", "2"]);
        return true;
      });
    });
    assert.deepEqual(await rows(db, Order), [[1, 1]]);
  });

  scenario("an update that changes the scope column raises", async (db, Order) => {
    await scope({ shop: 1 }, async () => {
      const row = await Order.objects.using(db).insert({ total: 1 });
      await assert.rejects(Order.objects.using(db).filter(Order.id.eq(row.id)).update({ shopId: 2 }), ScopeMismatch);
      await Order.objects.using(db).filter(Order.id.eq(row.id)).update({ total: 5, shopId: 1 });
    });
    assert.deepEqual(await rows(db, Order), [[1, 5]]);
  });

  scenario("insertMany fills and checks every row", async (db, Order) => {
    await scope({ shop: 3 }, async () => {
      await Order.objects.using(db).insertMany([{ total: 1 }, { total: 2, shopId: 3 }]);
      await assert.rejects(Order.objects.using(db).insertMany([{ total: 3 }, { total: 4, shopId: 4 }]), (e: Any) => e instanceof ScopeMismatch && e.actual === "4");
    });
    assert.deepEqual(await rows(db, Order), [[3, 1], [3, 2]]);
  });

  scenario("an upsert checks the row and keeps the scope column", async (db, Order) => {
    await scope({ shop: 1 }, async () => {
      const row = await Order.objects.using(db).insert({ total: 1 });
      await assert.rejects(Order.objects.using(db).insert({ id: row.id, shopId: 9, total: 2 }).onConflict(Order.id, { update: true }), ScopeMismatch);
      await Order.objects.using(db).insert({ id: row.id, total: 7 }).onConflict(Order.id, { update: true });
    });
    assert.deepEqual(await rows(db, Order), [[1, 7]]);
  });

  scenario("a write without the key raises ScopeMissing", async (db, Order) => {
    await assert.rejects(Order.objects.using(db).insert({ total: 1 }), (e: Any) => e instanceof ScopeMissing && e.model === "Order" && e.key === "shop");
    assert.deepEqual(await rows(db, Order), []);
  });
}

test("the errors are query errors", () => {
  assert.ok(new ScopeMissing("x") instanceof QueryError);
  assert.ok(new ScopeMismatch("x") instanceof QueryError);
});

test("scope refuses what the schema does not declare", { skip }, () =>
  withOrders("sqlite", async () => {
    await assert.rejects(scope({ nope: 1 }, async () => 0), /no key/);
    await assert.rejects(scope({ shop: null }, async () => 0), /never null/);
    await assert.rejects(scope({ shop: [1, 2] }, async () => 0), /not available yet/);
  }));
