/** `@@scope.by` next to another ORM: a prefix, a bypass value, the role check flag and unmanaged tables.
 * The fixture tables use the SQL of the other ORM, and run as roles that are not superusers. */

import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { test } from "node:test";

import { connect, loads, Registry, scope, ScopeMissing, unscoped, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const SCHEMA = (check: string, admin: string) => `
datasource db {
 provider = "postgresql"
 scope_prefix = "tenant"
 scope_bypass = { setting = "tenant.bypass", value = "on" }
 bypass_roles = ["${admin}"]
 scope_role_check = ${check}
}
model Order {
 id Int @id @default(autoincrement())
 shop_id Int
 total Int
 @@scope.by(shop: shop_id)
 @@map("co_js_orders")
}
model Note {
 id Int @id @default(autoincrement())
 user_id Int
 @@scope.by(user: user_id)
 @@map("co_js_notes")
}
model Guitar {
 id Int @id
 shop_id Int
 name String
 @@managed(false)
 @@scope.by(shop: shop_id)
 @@map("co_js_guitars")
}
`;

// What the other ORM keeps in its own migrations: a table, a policy and a fill trigger.
const GUITARS = [
  "CREATE TABLE co_js_guitars (id serial PRIMARY KEY, shop_id integer NOT NULL, name text NOT NULL)",
  "ALTER TABLE co_js_guitars ENABLE ROW LEVEL SECURITY",
  "ALTER TABLE co_js_guitars FORCE ROW LEVEL SECURITY",
  `CREATE POLICY tenant_scope ON co_js_guitars USING (
    current_setting('tenant.bypass', true) = 'on'
    OR shop_id::text = ANY(string_to_array(current_setting('tenant.shop', true), ',')))`,
  `CREATE FUNCTION co_js_guitars_fill() RETURNS trigger LANGUAGE plpgsql AS $$
   BEGIN
     IF NEW.shop_id IS NULL THEN NEW.shop_id := NULLIF(current_setting('tenant.shop', true), '')::integer; END IF;
     RETURN NEW;
   END $$`,
  "CREATE TRIGGER co_js_guitars_fill BEFORE INSERT ON co_js_guitars FOR EACH ROW EXECUTE FUNCTION co_js_guitars_fill()",
];

const base = process.env["ORM_TEST_DATABASE_URL"];
const skip = !hasExtension("monorm.scope") || !base;

function urlAs(role: string): string {
  const url = new URL(base!);
  url.username = role;
  url.password = "pw";
  return url.toString();
}

interface World {
  owner: Database;
  connectAs: (kind: string) => Promise<Database>;
  Order: Any;
  Guitar: Any;
}

async function world(check: string, body: (w: World) => Promise<void>): Promise<void> {
  const tag = randomBytes(4).toString("hex");
  const names = Object.fromEntries(["app", "admin", "boss"].map((k) => [k, `co_js_${k}_${tag}`])) as Record<string, string>;
  const registry = new Registry();
  const models = loads(SCHEMA(check, names["admin"]!), { registry }) as Record<string, Any>;
  const owner = await connect(base!, { registry, default: false });
  const opened: Database[] = [];
  const dropFixture = async () => {
    await owner.execute("DROP TABLE IF EXISTS co_js_guitars");
    await owner.execute("DROP FUNCTION IF EXISTS co_js_guitars_fill()");
  };
  await dropFixture();
  await owner.dropTables();
  for (const role of Object.values(names)) {
    await owner.execute(`DROP ROLE IF EXISTS ${role}`);
    await owner.execute(`CREATE ROLE ${role} LOGIN PASSWORD 'pw'`);
  }
  await owner.execute(`GRANT ${names["admin"]} TO ${names["boss"]}`);
  await owner.createTables();
  for (const sql of GUITARS) await owner.execute(sql);
  for (const role of Object.values(names)) {
    await owner.execute(`GRANT ALL ON ALL TABLES IN SCHEMA public TO ${role}`);
    await owner.execute(`GRANT ALL ON ALL SEQUENCES IN SCHEMA public TO ${role}`);
  }
  const connectAs = async (kind: string) => {
    const db = await connect(urlAs(names[kind]!), { registry, default: false });
    opened.push(db);
    return db;
  };
  try {
    await body({ owner, connectAs, Order: models["Order"], Guitar: models["Guitar"] });
  } finally {
    for (const db of opened) await db.close();
    await dropFixture();
    await owner.dropTables();
    for (const role of Object.values(names)) {
      if ((await owner.fetch(`SELECT 1 FROM pg_roles WHERE rolname = '${role}'`)).length > 0) {
        await owner.execute(`DROP OWNED BY ${role}`);
        await owner.execute(`DROP ROLE ${role}`);
      }
    }
    await owner.close();
  }
}

/** Raw SQL in a transaction that sets `tenant.shop` by hand, as the other ORM does. */
const raw = <T>(db: Database, shop: number | string, fn: () => Promise<T>): Promise<T> =>
  db.transaction(async () => {
    await db.execute(`SELECT set_config('tenant.shop', '${shop}', true)`);
    return await fn();
  });

async function seed({ owner, Order }: World): Promise<void> {
  await scope({ shop: 1 }, () => Order.objects.using(owner).insertMany([{ total: 10 }, { total: 11 }]));
  await scope({ shop: 2 }, () => Order.objects.using(owner).insert({ total: 20 }));
  await owner.execute("INSERT INTO co_js_guitars (shop_id, name) VALUES (1, 'strat'), (2, 'tele'), (3, 'les paul')");
}

const scenario = (what: string, body: (w: World) => Promise<void>, check = "false") => test(what, { skip }, () => world(check, body));
const totals = (rows: Any[]) => rows.map((r) => Number(r["total"] ?? r.total)).sort((a, b) => a - b);
const count = (db: Database, Model: Any) => Model.objects.using(db).withoutDefaults().count() as Promise<number>;

scenario("a scope reads one shop from a monorm table and a fixture table", async (w) => {
  await seed(w);
  const app = await w.connectAs("app");
  await scope({ shop: 1 }, async () => {
    assert.deepEqual(totals((await w.Order.objects.using(app)) as Any[]), [10, 11]);
    assert.deepEqual(((await w.Guitar.objects.using(app)) as Any[]).map((g) => g.name), ["strat"]);
  });
  await scope({ shop: 2 }, async () => assert.deepEqual(totals((await w.Order.objects.using(app)) as Any[]), [20]));
  assert.deepEqual(await app.fetch("SELECT * FROM co_js_orders"), []);
  assert.deepEqual(await app.fetch("SELECT * FROM co_js_guitars"), []);
});

scenario("unscoped with no key opens every key with the bypass value", async (w) => {
  await seed(w);
  const app = await w.connectAs("app");
  const events: Any[] = [];
  app.onQuery((e) => void events.push(e));
  await unscoped([], async () => {
    assert.equal(await count(app, w.Order), 3);
    assert.equal(await count(app, w.Guitar), 3);
  });
  assert.equal(events[0].settings["tenant.bypass"], "on");
  assert.ok(!("monorm.bypass" in events[0].settings));
});

scenario("the scope value goes to the renamed setting", async (w) => {
  const app = await w.connectAs("app");
  const events: Any[] = [];
  app.onQuery((e) => void events.push(e));
  await scope({ shop: 1 }, () => w.Order.objects.using(app));
  assert.equal(events[0].settings["tenant.shop"], "1");
  assert.ok(!("scope.shop" in events[0].settings));
});

scenario("a raw list setting reads both shops from both tables", async (w) => {
  await seed(w);
  const app = await w.connectAs("app");
  await raw(app, "1,2", async () => {
    assert.deepEqual(totals(await app.fetch("SELECT total FROM co_js_orders")), [10, 11, 20]);
    const names = (await app.fetch("SELECT name FROM co_js_guitars")).map((r) => r["name"]).sort();
    assert.deepEqual(names, ["strat", "tele"]);
  });
  await raw(app, 1, async () => assert.deepEqual(totals(await app.fetch("SELECT total FROM co_js_orders")), [10, 11]));
});

scenario("an insert with no shop takes it from the setting and a list gives no fill", async ({ owner, connectAs }) => {
  const app = await connectAs("app");
  await raw(app, 7, async () => {
    await app.execute("INSERT INTO co_js_orders (total) VALUES (1)");
    await app.execute("INSERT INTO co_js_guitars (name) VALUES ('jazzmaster')");
  });
  assert.deepEqual((await owner.fetch("SELECT shop_id FROM co_js_orders")).map((r) => r["shop_id"]), [7]);
  assert.deepEqual((await owner.fetch("SELECT shop_id FROM co_js_guitars")).map((r) => r["shop_id"]), [7]);
  await assert.rejects(raw(app, "7,8", () => app.execute("INSERT INTO co_js_orders (total) VALUES (2)")), /null value|row-level security/);
});

scenario("a scope list sends comma text and reads every listed shop", async (w) => {
  await seed(w);
  const app = await w.connectAs("app");
  await scope({ shop: [1, 2] }, async () => {
    assert.deepEqual(totals((await w.Order.objects.using(app)) as Any[]), [10, 11, 20]);
    await app.transaction(async () => {
      assert.equal((await app.fetch("SELECT current_setting('tenant.shop', true) AS s"))[0]!["s"], "1,2");
      assert.deepEqual(totals(await app.fetch("SELECT total FROM co_js_orders")), [10, 11, 20]);
      const names = (await app.fetch("SELECT name FROM co_js_guitars")).map((r) => r["name"]).sort();
      assert.deepEqual(names, ["strat", "tele"]);
    });
  });
  await scope({ shop: new Set([1, 3]) }, async () => assert.deepEqual(totals((await w.Order.objects.using(app)) as Any[]), [10, 11]));
});

scenario("a held read keeps one statement for a value and one for a list", async (w) => {
  await seed(w);
  const app = await w.connectAs("app");
  const q = w.Order.objects.using(app).orderBy(w.Order.total).prepare();
  const all = async () => ((await q.all({})) as Any[]).map((o) => o.total);
  await scope({ shop: 2 }, async () => assert.deepEqual(await all(), [20]));
  await scope({ shop: [1, 2] }, async () => assert.deepEqual(await all(), [10, 11, 20]));
  await scope({ shop: 1 }, async () => assert.deepEqual(await all(), [10, 11]));
});

scenario("a write under a scope list needs a cell that is a member", async ({ owner, connectAs, Order }) => {
  const app = await connectAs("app");
  const orders = Order.objects.using(app);
  await scope({ shop: [1, 2] }, async () => {
    await orders.insert({ shopId: 2, total: 5 });
    await orders.insertMany([{ shopId: 1, total: 6 }, { shopId: 2, total: 7 }]);
    await assert.rejects(orders.insert({ total: 8 }), (e: Error) => e instanceof ScopeMissing && /shop/.test(e.message));
    await assert.rejects(orders.insert({ shopId: 3, total: 9 }), /shop/);
  });
  const rows = (await owner.fetch("SELECT shop_id, total FROM co_js_orders")).map((r) => `${r["shop_id"]}:${r["total"]}`).sort();
  assert.deepEqual(rows, ["1:6", "2:5", "2:7"]);
});

scenario("a scope list with an empty list or an item that holds a comma raises", async () => {
  for (const bad of [[], [""], ["1,2"], [1, null]]) {
    await assert.rejects(scope({ shop: bad }, async () => 0), /scope\(\{? ?shop/);
  }
});

test("a scope list raises at scope when no datasource sets the prefix", { skip: !hasExtension("monorm.scope") }, async () => {
  const registry = new Registry();
  loads(
    `datasource db { provider = "sqlite" }
model Order {
 id Int @id @default(autoincrement())
 shop_id Int
 @@scope.by(shop: shop_id)
}`,
    { registry },
  );
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await scope({ shop: 1 }, async () => 0);
    await assert.rejects(scope({ shop: [1, 2] }, async () => 0), /scope_prefix/);
  } finally {
    await db.close();
  }
});

scenario("two permissive policies are ored with the policy of the other orm", async (w) => {
  await seed(w);
  await w.owner.execute("CREATE POLICY tenant_scope ON co_js_orders USING (shop_id = 2)");
  const app = await w.connectAs("app");
  await scope({ shop: 1 }, async () => assert.deepEqual(totals((await w.Order.objects.using(app).withoutDefaults()) as Any[]), [10, 11, 20]));
  await raw(app, 1, async () => assert.deepEqual(totals(await app.fetch("SELECT total FROM co_js_orders")), [10, 11, 20]));
  const names = (await w.owner.fetch("SELECT policyname FROM pg_policies WHERE tablename = 'co_js_orders'")).map((r) => r["policyname"]).sort();
  assert.deepEqual(names, ["monorm_scope", "tenant_scope"]);
});

scenario("unscoped with some keys raises with the bypass value", async (w) => {
  const app = await w.connectAs("app");
  await unscoped(["shop"], () => assert.rejects(count(app, w.Order), /opens only some scope keys.*user/));
  await unscoped(["shop", "user"], async () => assert.equal(await count(app, w.Order), 0));
});

scenario("with the role check off a role outside bypass roles opens the bypass", async (w) => {
  await seed(w);
  const app = await w.connectAs("app");
  const boss = await w.connectAs("boss");
  await unscoped([], async () => {
    assert.equal(await count(app, w.Order), 3);
    assert.equal(await count(boss, w.Order), 3);
  });
  const policy = await w.owner.fetch("SELECT pg_get_expr(polqual, polrelid) AS q FROM pg_policy WHERE polname = 'monorm_scope' AND polrelid = 'co_js_orders'::regclass");
  assert.ok(!String(policy[0]!["q"]).includes("pg_roles"));
});

scenario("with the role check on only a member of bypass roles opens the bypass", async (w) => {
  await seed(w);
  const app = await w.connectAs("app");
  const boss = await w.connectAs("boss");
  await unscoped([], async () => {
    assert.equal(await count(app, w.Order), 0);
    assert.equal(await count(boss, w.Order), 3);
    // the policy of the other ORM on its own table is not changed by the flag
    assert.equal(await count(app, w.Guitar), 3);
  });
}, "true");

test("an unmanaged scoped model needs a key in the datasource", () => {
  const source = SCHEMA("false", "a")
    .replace(' scope_prefix = "tenant"\n', "")
    .replace(' scope_bypass = { setting = "tenant.bypass", value = "on" }\n', "");
  assert.throws(() => loads(source, { registry: new Registry() }), /Guitar.*unmanaged.*`scope_prefix` or `scope_bypass`/);
});
