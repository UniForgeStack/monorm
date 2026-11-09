// Admin row scope, `can=` rules, the bypass of a read default and a tenant read default that the admin ignores, on PostgreSQL and SQLite.
import assert from "node:assert/strict";
import { test } from "node:test";

import { Admin, AdminConfigError } from "../src/admin.js";
import { connect, loads, param, Registry } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";

const SOURCE = `
datasource db {
  provider = "%(provider)s"
}

model ScopeOwner {
  id    Int         @id
  name  String
  items ScopeItem[]
  @@map("scope_owners")
}

model ScopeItem {
  id       Int        @id
  owner_id Int
  title    String
  rank     Int
  owner    ScopeOwner @relation(fields: [owner_id], references: [id])
  %(extra)s
  @@map("scope_items")
}
`;

const ITEMS = "/admin/api/resources/ScopeItem";
const SU1 = { key: 1, is_superuser: true };
const SU2 = { key: 2, is_superuser: true };
const PROVIDERS = ["sqlite", "postgres"] as const;

type Loose = any;
type Database = Awaited<ReturnType<typeof connect>>;
interface World {
  registry: Registry;
  db: Database;
  Owner: Loose;
  Item: Loose;
}

const titles = (response: { status: number; json(): any }): string[] => {
  assert.equal(response.status, 200, JSON.stringify(response.json()));
  return response.json().rows.map((row: unknown[]) => row[0]);
};

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>, extra = "", after?: (w: World) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const models = loads(SOURCE.replace("%(provider)s", provider).replace("%(extra)s", extra), { registry }) as Record<string, Loose>;
  const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : PG_URL, { registry, default: false });
  try {
    await db.dropTables();
    await db.createTables();
    const Owner = models["ScopeOwner"], Item = models["ScopeItem"];
    await Owner.objects.using(db).insertMany([{ id: 1, name: "Ada" }, { id: 2, name: "Bo" }]);
    await Item.objects.using(db).insertMany([
      { id: 1, ownerId: 1, title: "one", rank: 1 },
      { id: 2, ownerId: 1, title: "two", rank: 2 },
      { id: 3, ownerId: 2, title: "three", rank: 3 },
      { id: 4, ownerId: 1, title: "four", rank: 4 },
    ]);
    const w = { registry, db, Owner, Item };
    await after?.(w);
    await run(w);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

function siteOf(w: World, item: Record<string, unknown> = {}, options: ConstructorParameters<typeof Admin>[1] = {}) {
  const site = new Admin(w.registry, options);
  site.register(w.Item, { list: { columns: [w.Item.title, w.Item.rank], order: [w.Item.id], search: [w.Item.title] }, ...item } as never);
  site.register(w.Owner, { list: { columns: [w.Owner.name], order: [w.Owner.id] } });
  return site;
}

const mine = (w: World) => w.Item.objects.filter(w.Item.ownerId.eq(param("user"))).prepare();

for (const provider of PROVIDERS) {
  test(`the queryset filter limits the list the count the search and the detail (${provider})`, () => world(provider, async (w) => {
    const client = siteOf(w, { queryset: mine(w) }).testClient(w.db, { user: SU1 });
    assert.deepEqual(titles(await client.get(ITEMS)), ["one", "two", "four"]);
    assert.equal((await client.get("/admin/api/counts/ScopeItem?exact=1")).json().count, 3);
    assert.deepEqual(titles(await client.get(`${ITEMS}?q=t`)), ["two"]);
    assert.deepEqual(titles(await client.get(`${ITEMS}?q=fo`)), ["four"]);
    assert.equal((await client.get(`${ITEMS}/1`)).status, 200);
    assert.equal((await client.get(`${ITEMS}/3`)).status, 404);
    const other = siteOf(w, { queryset: mine(w) }).testClient(w.db, { user: SU2 });
    assert.deepEqual(titles(await other.get(ITEMS)), ["three"]);
  }));

  test(`the queryset order is the order of the list (${provider})`, () => world(provider, async (w) => {
    const queryset = w.Item.objects.filter(w.Item.ownerId.eq(param("user"))).orderBy(w.Item.id.desc()).prepare();
    const client = siteOf(w, { queryset, list: { columns: [w.Item.title] } }).testClient(w.db, { user: SU1 });
    assert.deepEqual(titles(await client.get(ITEMS)), ["four", "two", "one"]);
  }));

  test(`a parent outside the queryset has no tabs (${provider})`, () => world(provider, async (w) => {
    const site = new Admin(w.registry);
    const own = w.Owner.objects.filter(w.Owner.id.eq(param("user"))).prepare();
    site.register(w.Owner, { queryset: own, list: { columns: [w.Owner.name], order: [w.Owner.id] } });
    site.register(w.Item, { queryset: mine(w), list: { columns: [w.Item.title], order: [w.Item.id] } });
    const client = site.testClient(w.db, { user: SU1 });
    assert.deepEqual(titles(await client.get("/admin/api/resources/ScopeOwner/1/items")), ["one", "two", "four"]);
    assert.equal((await client.get("/admin/api/resources/ScopeOwner/2/items")).status, 404);
    assert.equal((await client.get("/admin/api/counts/ScopeOwner/2/items")).status, 404);
  }));

  test(`can rules are flags of every row in the list and the detail (${provider})`, () => world(provider, async (w) => {
    const Item = w.Item;
    const client = siteOf(w, { can: { change: Item.rank.gt(1), delete: Item.rank.gt(3) } }).testClient(w.db, { user: SU1 });
    const body = (await client.get(ITEMS)).json();
    assert.deepEqual(body.flags, [
      { change: false, delete: false },
      { change: true, delete: false },
      { change: true, delete: false },
      { change: true, delete: true },
    ]);
    assert.equal(body.rows[0].length, 3);
    assert.deepEqual((await client.get(`${ITEMS}/4`)).json().flags, { change: true, delete: true });
    const meta = (await client.get("/admin/api/resources")).json();
    assert.deepEqual(meta.resources.find((r: { name: string }) => r.name === "ScopeItem").can, ["change", "delete"]);
  }));

  test(`a callable queryset is one host call for each request (${provider})`, () => world(provider, async (w) => {
    const seen: unknown[][] = [];
    const scope = async (user: unknown) => {
      seen.push([user]);
      return w.Item.ownerId.eq(2);
    };
    const client = siteOf(w, { queryset: scope }).testClient(w.db, { user: { key: 5, is_superuser: true } });
    assert.deepEqual(titles(await client.get(ITEMS)), ["three"]);
    assert.deepEqual(seen, [[5]]);
  }));

  test(`a tenant read default is off in every admin read (${provider})`, () => world(provider, async (w) => {
    const client = siteOf(w, { queryset: mine(w) }).testClient(w.db, { user: SU1 });
    assert.deepEqual(titles(await client.get(ITEMS)), ["one", "two", "four"]);
    assert.equal((await client.get(`${ITEMS}/2`)).status, 200);
    assert.equal((await client.get("/admin/api/counts/ScopeItem?exact=1")).json().count, 3);
    assert.deepEqual(titles(await client.get(`${ITEMS}?q=t`)), ["two"]);
    const every = siteOf(w).testClient(w.db, { user: SU1 });
    assert.deepEqual(titles(await every.get(ITEMS)), ["one", "two", "three", "four"]);
    assert.equal((await every.get(`${ITEMS}?without=tenant`)).status, 400);
    const meta = (await every.get("/admin/api/resources")).json();
    assert.equal("tenants" in meta, false);
    assert.ok(!meta.resources.find((r: { name: string }) => r.name === "ScopeItem").bypass.includes("tenant"));
  }, '@@query.filter("tenant", "owner_id == scope.tenant")'));

  test(`a bypass permission switches a read default off (${provider})`, { skip: !hasExtension("monorm.soft-delete") }, () => world(provider, async (w) => {
    const client = siteOf(w).testClient(w.db, { user: SU1 });
    assert.deepEqual(titles(await client.get(ITEMS)), ["one", "three", "four"]);
    assert.equal((await client.get(`${ITEMS}/2`)).status, 404);
    assert.deepEqual(titles(await client.get(`${ITEMS}?without=deleted`)), ["one", "two", "three", "four"]);
    assert.equal((await client.get(`${ITEMS}/2?without=deleted`)).status, 200);
    assert.equal((await client.get("/admin/api/counts/ScopeItem?without=deleted&exact=1")).json().count, 4);
    assert.equal((await client.get(`${ITEMS}?without=nope`)).status, 400);
    const meta = (await client.get("/admin/api/resources")).json();
    assert.deepEqual(meta.resources.find((r: { name: string }) => r.name === "ScopeItem").bypass, ["deleted"]);
    const plain = siteOf(w).testClient(w.db, { user: { key: 9 } });
    assert.ok([401, 403].includes((await plain.get(`${ITEMS}?without=deleted`)).status));
  }, '@@soft_delete.deleted_at("deleted")', async (w) => {
    await w.Item.objects.using(w.db).filter(w.Item.id.eq(2)).delete();
  }));

  test(`a bad queryset or can rule is a config error (${provider})`, () => world(provider, async (w) => {
    const Item = w.Item;
    for (const bad of [
      { queryset: Item.objects.prepare() },
      { queryset: Item.objects.filter(Item.rank.gt(1)).limit(2).prepare() },
      { queryset: w.Owner.objects.filter(w.Owner.id.gt(0)).prepare() },
      { queryset: Item.objects.filter(Item.rank.gt(param("rank"))).prepare() },
      { can: { fly: Item.rank.gt(1) } },
    ]) {
      assert.throws(() => siteOf(w, bad).check(), (e) => e instanceof AdminConfigError);
    }
  }));
}
