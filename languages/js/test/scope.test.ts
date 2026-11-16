/** `@@scope.by`: one scope for the app and the database; needs the `monorm.scope` package. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, loads, param, QueryError, Registry, scope, ScopeMismatch, ScopeMissing, skipTriggers, unscoped, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const SCHEMA = `
model Order {
  id Int @id @default(autoincrement())
  shop_id Int
  total Int
  @@scope.by(shop: shop_id)
  @@timestamps.updated_at("changed")
  @@map("sc_js_orders")
}
`;

const skip = !hasExtension("monorm.scope", "monorm.timestamps");
const urlOf = (provider: string) => (provider === "sqlite" ? "sqlite://:memory:" : (process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test"));

async function withOrders(provider: string, body: (db: Database, Order: Any) => Promise<void>, datasource = ""): Promise<void> {
  const registry = new Registry();
  const models = loads(`datasource db {\n  provider = "${provider}"\n${datasource}}\n${SCHEMA}`, { registry }) as Record<string, Any>;
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

  const seed = async (db: Database, Order: Any) => {
    for (const shop of [1, 2]) await scope({ shop }, () => Order.objects.using(db).insert({ total: shop * 10 }));
  };
  const totals = async (db: Database, Order: Any) => ((await Order.objects.using(db)) as Any[]).map((o) => o.total).sort((a, b) => a - b);

  scenario("unscoped opens one key for reads and writes and closes after the block", async (db, Order) => {
    await seed(db, Order);
    await scope({ shop: 1 }, async () => {
      await unscoped(["shop"], async () => {
        assert.deepEqual(await totals(db, Order), [10, 20]);
        await Order.objects.using(db).insert({ shopId: 2, total: 21 });
        await Order.objects.using(db).filter(Order.shopId.eq(2)).update({ shopId: 3 });
      });
      assert.deepEqual(await totals(db, Order), [10]);
      await assert.rejects(Order.objects.using(db).insert({ shopId: 2, total: 1 }), ScopeMismatch);
    });
    await unscoped(["shop"], () => Order.objects.using(db).insert({ shopId: 9, total: 90 }));
    await assert.rejects(Order.objects.using(db).insert({ total: 1 }), ScopeMissing);
    assert.deepEqual(await rows(db, Order), [[1, 10], [3, 20], [3, 21], [9, 90]]);
  });

  scenario("unscoped needs a declared key", async () => {
    await assert.rejects(unscoped([], async () => 0), /at least one key/);
    await assert.rejects(unscoped(["nope"], async () => 0), /no key nope/);
    await assert.rejects(unscoped(["shop", "nope"], async () => 0), /no key nope/);
  });

  scenario("nested unscoped blocks add keys and a prepared read never keeps a block", async (db, Order) => {
    await seed(db, Order);
    const q = Order.objects.using(db).orderBy(Order.id).prepare();
    const all = async () => ((await q.all({})) as Any[]).map((o) => o.total);
    await unscoped(["shop"], async () => {
      await unscoped(["shop"], async () => assert.deepEqual(await all(), [10, 20]));
      assert.deepEqual(await all(), [10, 20]);
    });
    await scope({ shop: 2 }, async () => {
      assert.deepEqual(await all(), [20]);
      await unscoped(["shop"], async () => assert.deepEqual(await all(), [10, 20]));
      assert.deepEqual(await all(), [20]);
    });
    await scope({ shop: 1 }, async () => assert.deepEqual(await all(), [10]));
  });

  scenario("a prepared write checks the scope unless unscoped", async (db, Order) => {
    await seed(db, Order);
    const edit = () => Order.objects.using(db).filter(Order.total.eq(param("t"))).update({ total: param("n") }).prepare();
    await scope({ shop: 1 }, () => edit().run({ t: 10, n: 11 }));
    await unscoped(["shop"], () => edit().run({ t: 20, n: 21 }));
    assert.deepEqual(await rows(db, Order), [[1, 11], [2, 21]]);
  });

  scenario("unscoped publishes the bypass setting on postgresql", async (db, Order) => {
    const events: Any[] = [];
    db.onQuery((e) => void events.push(e));
    await scope({ shop: 1 }, () => unscoped(["shop"], () => Order.objects.using(db)));
    await Order.objects.using(db).withoutDefaults();
    assert.equal(events[0].settings["monorm.bypass"], provider === "postgresql" ? "{shop}" : undefined);
    assert.ok(!("monorm.bypass" in events[1].settings));
  });

  scenario("a block in an open transaction changes the setting and restores it", async (db, Order) => {
    if (provider !== "postgresql") return;
    const events: Any[] = [];
    db.onQuery((e) => void events.push(e));
    await db.transaction(async () => {
      await Order.objects.using(db).withoutDefaults();
      await unscoped(["shop"], () => Order.objects.using(db).withoutDefaults());
      await Order.objects.using(db).withoutDefaults();
    });
    const seen = events.filter((e) => e.sql.includes("sc_js_orders")).map((e) => e.settings["monorm.bypass"]);
    assert.deepEqual(seen, [undefined, "{shop}", undefined]);
  });

  scenario("skipTriggers raises on sqlite and publishes the setting on postgresql", async (db, Order) => {
    if (provider === "sqlite") {
      await assert.rejects(skipTriggers(["timestamps"], () => Order.objects.using(db).withoutDefaults()), /sqlite has none/);
      return;
    }
    const events: Any[] = [];
    db.onQuery((e) => void events.push(e));
    await skipTriggers(["timestamps"], () => Order.objects.using(db).withoutDefaults());
    assert.equal(events[0].settings["monorm.skip"], "{timestamps}");
    await assert.rejects(skipTriggers(["soft-delete"], async () => 0), /no skippable triggers/);
    await assert.rejects(skipTriggers([], async () => 0), /at least one/);
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

test("skipTriggers switches the timestamps trigger off for a bypass role", { skip: skip || !process.env["ORM_TEST_DATABASE_URL"] }, () =>
  withOrders("postgresql", async (db, Order) => {
    const year = "SELECT extract(year FROM changed)::int AS y FROM sc_js_orders";
    await scope({ shop: 1 }, () => Order.objects.using(db).insert({ total: 1 }));
    await db.execute("UPDATE sc_js_orders SET changed = '2000-01-01'");
    await db.execute("UPDATE sc_js_orders SET total = 2");
    const touched = Number((await db.fetch(year))[0]!["y"]);
    await db.execute("UPDATE sc_js_orders SET changed = '2000-01-01'");
    await skipTriggers(["timestamps"], () => db.execute("UPDATE sc_js_orders SET total = 3"));
    const kept = Number((await db.fetch(year))[0]!["y"]);
    assert.ok(touched > 2000, String(touched));
    assert.equal(kept, 2000);
  }, '  bypass_roles = ["postgres"]\n'));
