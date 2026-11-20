/** `@@scope.by` in the database: row-level security on PostgreSQL, tested as roles that are not superusers. */

import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { test } from "node:test";

import { connect, loads, Registry, scope, unscoped, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const SCHEMA = (n: Record<string, string>) => `
datasource db {
  provider = "postgresql"
  bypass_roles = ["${n["admin"]}"]
  exempt_roles = ["${n["auditor"]}"]
}
model Order {
  id Int @id @default(autoincrement())
  shop_id Int
  total Int
  @@scope.by(shop: shop_id)
  @@map("rls_js_orders")
}
`;

const base = process.env["ORM_TEST_DATABASE_URL"];
const skip = !hasExtension("monorm.scope") || !base;

function urlAs(role: string): string {
  const url = new URL(base!);
  url.username = role;
  url.password = "pw";
  return url.toString();
}

async function world(body: (owner: Database, connectAs: (kind: string) => Promise<Database>, Order: Any, names: Record<string, string>) => Promise<void>): Promise<void> {
  const tag = randomBytes(4).toString("hex");
  const names = Object.fromEntries(["app", "admin", "boss", "auditor"].map((k) => [k, `rls_js_${k}_${tag}`])) as Record<string, string>;
  const registry = new Registry();
  const models = loads(SCHEMA(names), { registry }) as Record<string, Any>;
  const owner = await connect(base!, { registry, default: false });
  const opened: Database[] = [];
  await owner.dropTables();
  for (const role of Object.values(names)) {
    await owner.execute(`DROP ROLE IF EXISTS ${role}`);
    await owner.execute(`CREATE ROLE ${role} LOGIN PASSWORD 'pw'`);
  }
  await owner.execute(`GRANT ${names["admin"]} TO ${names["boss"]}`);
  await owner.createTables();
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
    await body(owner, connectAs, models["Order"], names);
  } finally {
    for (const db of opened) await db.close();
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

/** Raw SQL runs with no scope of its own, so the helper sets the setting for one transaction. */
const raw = <T>(db: Database, shop: number, fn: () => Promise<T>): Promise<T> =>
  db.transaction(async () => {
    await db.execute(`SELECT set_config('scope.shop', '${shop}', true)`);
    return await fn();
  });

async function seed(owner: Database, Order: Any): Promise<void> {
  await scope({ shop: 1 }, () => Order.objects.using(owner).insertMany([{ total: 10 }, { total: 11 }]));
  await scope({ shop: 2 }, () => Order.objects.using(owner).insert({ total: 20 }));
}

const scenario = (what: string, body: Parameters<typeof world>[0]) => test(what, { skip }, () => world(body));
const refused = /row-level security|null value/;

scenario("the policy and the flags are on the table", async (owner, _connectAs, _Order, names) => {
  const flags = await owner.fetch("SELECT relrowsecurity AS on, relforcerowsecurity AS forced FROM pg_class WHERE relname = 'rls_js_orders'");
  assert.deepEqual([flags[0]!["on"], flags[0]!["forced"]], [true, true]);
  const policies = (await owner.fetch("SELECT policyname FROM pg_policies WHERE tablename = 'rls_js_orders'")).map((r) => r["policyname"]).sort();
  assert.deepEqual(policies, ["monorm_scope", `monorm_scope_exempt_${names["auditor"]}`]);
});

scenario("a role with no scope sees and writes nothing", async (owner, connectAs, Order) => {
  await seed(owner, Order);
  const app = await connectAs("app");
  assert.deepEqual(await app.fetch("SELECT * FROM rls_js_orders"), []);
  await assert.rejects(app.execute("INSERT INTO rls_js_orders (shop_id, total) VALUES (1, 1)"), refused);
});

scenario("a scope shows its rows and the client equals a raw read", async (owner, connectAs, Order) => {
  await seed(owner, Order);
  const app = await connectAs("app");
  const totals = async (shop: number) => ((await scope({ shop }, () => Order.objects.using(app))) as Any[]).map((o) => o.total).sort((a, b) => a - b);
  const viaTheClient = await totals(1);
  const rawRows = (await scope({ shop: 1 }, () => app.fetch("SELECT total FROM rls_js_orders"))).map((r) => Number(r["total"])).sort((a, b) => a - b);
  const rawInTx = (await scope({ shop: 1 }, () => app.transaction(() => app.fetch("SELECT total FROM rls_js_orders")))).map((r) => Number(r["total"])).sort((a, b) => a - b);
  assert.deepEqual(viaTheClient, rawRows);
  assert.deepEqual(viaTheClient, rawInTx);
  assert.deepEqual(viaTheClient, [10, 11]);
  assert.deepEqual(await totals(2), [20]);
});

scenario("the database refuses a write for another scope", async (_owner, connectAs) => {
  const app = await connectAs("app");
  await raw(app, 1, () => app.execute("INSERT INTO rls_js_orders (shop_id, total) VALUES (1, 1)"));
  for (const sql of ["UPDATE rls_js_orders SET shop_id = 2", "INSERT INTO rls_js_orders (shop_id, total) VALUES (2, 1)"]) {
    await assert.rejects(raw(app, 1, () => app.execute(sql)), /row-level security/);
  }
});

scenario("the fill trigger takes a null column from the setting", async (owner, connectAs) => {
  const app = await connectAs("app");
  await raw(app, 7, () => app.execute("INSERT INTO rls_js_orders (total) VALUES (1)"));
  assert.deepEqual((await owner.fetch("SELECT shop_id FROM rls_js_orders")).map((r) => r["shop_id"]), [7]);
});

scenario("unscoped opens the rows only for a bypass role", async (owner, connectAs, Order) => {
  await seed(owner, Order);
  const app = await connectAs("app");
  const boss = await connectAs("boss");
  const count = (db: Database) => Order.objects.using(db).withoutDefaults().count() as Promise<number>;
  await unscoped(["shop"], async () => {
    assert.equal(await count(app), 0);
    assert.equal(await count(boss), 3);
  });
  assert.equal(await count(boss), 0);
});

scenario("an exempt role reads every row and writes none", async (owner, connectAs, Order) => {
  await seed(owner, Order);
  const auditor = await connectAs("auditor");
  assert.equal((await auditor.fetch("SELECT * FROM rls_js_orders")).length, 3);
  await assert.rejects(auditor.execute("INSERT INTO rls_js_orders (shop_id, total) VALUES (1, 1)"), /row-level security/);
});

scenario("a bypass role that is gone does not break a read", async (owner, connectAs, Order, names) => {
  await seed(owner, Order);
  const app = await connectAs("app");
  for (const kind of ["boss", "admin"]) {
    await owner.execute(`DROP OWNED BY ${names[kind]}`);
    await owner.execute(`DROP ROLE ${names[kind]}`);
  }
  const totals = (await raw(app, 1, () => app.fetch("SELECT total FROM rls_js_orders"))).map((r) => Number(r["total"])).sort((a, b) => a - b);
  assert.deepEqual(totals, [10, 11]);
});

scenario("the setting read is one initplan", async (_owner, connectAs) => {
  const app = await connectAs("app");
  const plan = (await raw(app, 1, () => app.fetch("EXPLAIN SELECT * FROM rls_js_orders"))).map((r) => String(r["QUERY PLAN"])).join("\n");
  assert.ok(plan.includes("InitPlan"), plan);
  assert.ok(!plan.split("InitPlan")[0]!.includes("current_setting"), plan);
});

const COMPOSED = `
datasource db {
  provider = "postgresql"
}
model Person {
  id Int @id @default(autoincrement())
  shop_id Int
  @@scope.by(shop: shop_id)
  @@map("rls_comp_js_people")
}
model Employee {
  salary Int
  @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
  @@map("rls_comp_js_employees")
}
`;

test("a scope on the root of a composed chain secures every level table and scopes the insert", { skip: skip || !hasExtension("monorm.composition") }, async () => {
  const registry = new Registry();
  const models = loads(COMPOSED, { registry });
  const employees = (db: Database) => (models["Employee"] as Any).objects.using(db);
  const db = await connect(base!, { registry, default: false });
  try {
    await db.dropTables();
    await db.createTables();
    for (const table of ["rls_comp_js_people", "rls_comp_js_employees"]) {
      const policies = (await db.fetch(`SELECT policyname FROM pg_policies WHERE tablename = '${table}'`)).map((r) => r["policyname"]);
      assert.ok(policies.includes("monorm_scope"), `${table}: ${policies.join(",")}`);
    }
    await scope({ shop: 1 }, async () => {
      const made = (await employees(db).insertMany([{ salary: 1 }, { salary: 2 }]).returning()) as Any[];
      assert.deepEqual(made.map((e) => [e.shopId, e.salary]), [[1, 1], [1, 2]]);
      await assert.rejects(employees(db).insertMany([{ salary: 3, shopId: 2 }]).returning(), /scoped by shop/);
    });
    assert.equal(await scope({ shop: 2 }, () => employees(db).count()), 0);
    assert.equal(await scope({ shop: 1 }, () => employees(db).count()), 2);
  } finally {
    await db.dropTables();
    await db.close();
  }
});
