// Admin lists: keyset paging, the capped count, sort, search, and the statement timeout, on PostgreSQL and SQLite.
import assert from "node:assert/strict";
import { test } from "node:test";

import { Admin, AdminConfigError, type ListConfig } from "../src/admin.js";
import { connect, loads, param, Registry } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";

const SOURCE = `
datasource db {
  provider = "%(provider)s"
}

model AdmNote {
  id    BigInt   @id @default(autoincrement())
  title String
  body  String
  rank  Int
  at    DateTime
  @@index([title])
  @@map("adm_notes")
}
`;

const SU = { is_superuser: true };
const NOTES = "/admin/api/resources/AdmNote";
const TITLES = ["alpha", "beta", "gamma", "delta", "epsilon"];
const BODIES = ["red apple", "green pear", "blue plum", "red plum", "white fig"];
const RANKS = [1, 2, 2, 3, 1];
const PROVIDERS = ["sqlite", "postgres"] as const;

// Models are loose here: the columns are reached by name.
type Loose = any;
type Database = Awaited<ReturnType<typeof connect>>;
interface World {
  provider: (typeof PROVIDERS)[number];
  db: Database;
  Note: Loose;
  make: (options?: ConstructorParameters<typeof Admin>[1]) => Admin;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const Note = (loads(SOURCE.replace("%(provider)s", provider), { registry }) as Record<string, Loose>)["AdmNote"];
  const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : PG_URL, { registry, default: false });
  try {
    await db.dropTables();
    await db.createTables();
    const at = new Date("2026-01-02T03:04:05Z");
    await Note.objects.using(db).insertMany(TITLES.map((title, i) => ({ title, body: BODIES[i], rank: RANKS[i], at })));
    await run({ provider, db, Note, make: (options) => new Admin(registry, options) });
  } finally {
    await db.dropTables();
    await db.close();
  }
}

function register(w: World, list: ListConfig | ((Note: Loose) => ListConfig) = {}, options: ConstructorParameters<typeof Admin>[1] = {}) {
  const site = w.make(options);
  const extra = typeof list === "function" ? list(w.Note) : list;
  site.register(w.Note, { list: { columns: [w.Note.title, w.Note.rank], order: [w.Note.id], pageSize: 2, ...extra } });
  return { site, client: site.testClient(w.db, { user: SU }) };
}

const titles = (response: { json(): any }): string[] => response.json().rows.map((row: unknown[]) => row[0]);

for (const provider of PROVIDERS) {
  test(`first last and before walk the keyset both ways in one statement each (${provider})`, () => world(provider, async (w) => {
    const { client } = register(w);
    const preamble = (await client.get("/admin/api/resources")).queries;
    const last = await client.get(`${NOTES}?last=1`);
    assert.equal(last.queries, preamble + 1);
    assert.deepEqual(titles(last), ["delta", "epsilon"]);
    assert.equal(last.json().next, null);
    const middle = await client.get(`${NOTES}?before=${last.json().prev}`);
    assert.deepEqual(titles(middle), ["beta", "gamma"]);
    const first = await client.get(`${NOTES}?before=${middle.json().prev}`);
    assert.deepEqual(titles(first), ["alpha"]);
    assert.equal(first.json().prev, null);
    const forth = await client.get(`${NOTES}?cursor=${first.json().next}`);
    assert.deepEqual(titles(forth), ["beta", "gamma"]);
  }));

  test(`sort orders rows and a cursor belongs to its sort (${provider})`, () => world(provider, async (w) => {
    const { client } = register(w, (Note) => ({ sortable: [Note.title, Note.rank] }));
    const page = await client.get(`${NOTES}?sort=-title`);
    assert.equal(page.status, 200);
    assert.deepEqual(titles(page), ["gamma", "epsilon"]);
    assert.deepEqual(titles(await client.get(`${NOTES}?sort=-title&cursor=${page.json().next}`)), ["delta", "beta"]);
    assert.equal((await client.get(`${NOTES}?sort=title&cursor=${page.json().next}`)).status, 400);
    assert.equal((await client.get(`${NOTES}?sort=body`)).status, 400);
    assert.equal((await client.get(`${NOTES}?sort=nope`)).status, 400);
  }));

  test(`the page size stops at 200 (${provider})`, () => world(provider, async (w) => {
    const { client } = register(w);
    assert.equal((await client.get(`${NOTES}?limit=200`)).status, 200);
    assert.equal((await client.get(`${NOTES}?limit=201`)).status, 400);
  }));

  test(`the count is its own request and a capped count has a limit (${provider})`, () => world(provider, async (w) => {
    const { client } = register(w);
    const rows = await client.get(NOTES);
    assert.equal("count" in rows.json(), false);
    const capped = await client.get("/admin/api/counts/AdmNote");
    assert.deepEqual([capped.json().count, capped.json().capped], [5, false]);
    assert.match(capped.sql.at(-1)!, /LIMIT/);
    const exact = await client.get("/admin/api/counts/AdmNote?exact=1");
    assert.equal(exact.json().count, 5);
    assert.doesNotMatch(exact.sql.at(-1)!, /LIMIT/);
  }));

  test(`a page jump uses an offset when the count is exact (${provider})`, () => world(provider, async (w) => {
    const { client } = register(w);
    const page = await client.get(`${NOTES}?page=3`);
    assert.equal(page.status, 200);
    assert.deepEqual(titles(page), ["epsilon"]);
    assert.match(page.sql.at(-1)!, /OFFSET/);
    assert.doesNotMatch((await client.get(NOTES)).sql.at(-1)!, /OFFSET/);
  }));

  test(`search uses a prefix on an indexed field and a term prefix picks the strategy (${provider})`, () => world(provider, async (w) => {
    const { client } = register(w, (Note) => ({ search: [Note.title] }));
    const search = (q: string) => `${NOTES}?q=${q}`;
    assert.deepEqual(titles(await client.get(search("al"))), ["alpha"], "an index on the field gives a prefix");
    assert.deepEqual(titles(await client.get(search("lp"))), [], "a prefix is not contains");
    assert.deepEqual(titles(await client.get(search("%3Dbeta"))), ["beta"], "= is exact");
    assert.deepEqual(titles(await client.get(search("%5Ede"))), ["delta"], "^ is a prefix");
    assert.deepEqual(titles(await client.get(search("%25"))), [], "a wildcard is text");
    assert.equal((await client.get(search("%40word"))).status, 400, "@ needs a full-text field");
    assert.deepEqual(titles(await client.get(search("3"))), ["gamma"], "a number also tries the primary key");
    assert.equal(((await client.get(search("%5Ede"))).sql.at(-1)!.match(/LIKE/g) ?? []).length, 1);
  }));

  test(`a search field with no index warns and one with no strategy is an error (${provider})`, () => world(provider, async (w) => {
    const indexed = w.make();
    indexed.register(w.Note, { list: { search: [w.Note.title] } });
    assert.deepEqual(indexed.check(), []);
    const plain = w.make();
    plain.register(w.Note, { list: { search: [w.Note.body] } });
    const warnings = plain.check();
    assert.equal(warnings.length, 1);
    assert.match(warnings[0]!, /AdmNote/);
    assert.match(warnings[0]!, /body/);
    const broken = w.make();
    broken.register(w.Note, { list: { search: [w.Note.at] } });
    assert.throws(() => broken.check(), (e: unknown) => e instanceof AdminConfigError && /no search strategy/.test(e.message));
  }));

  test(`search with a function makes one bridge call and a prepared query set works too (${provider})`, () => world(provider, async (w) => {
    const calls: [string, number][] = [];
    const byBody = async (q: string, limit: number) => {
      calls.push([q, limit]);
      return (await w.Note.objects.using(w.db).filter(w.Note.body.contains(q)).orderBy(w.Note.id)).map((row: Loose) => row.id);
    };
    let { client } = register(w, { searchWith: byBody });
    assert.deepEqual(titles(await client.get(`${NOTES}?q=plum`)), ["gamma", "delta"]);
    assert.equal(calls.length, 1);
    assert.equal(calls[0]![0], "plum");
    const prepared = w.Note.objects.using(w.db).filter(w.Note.title.eq(param("q"))).prepare();
    ({ client } = register(w, { searchWith: prepared }));
    assert.deepEqual(titles(await client.get(`${NOTES}?q=beta`)), ["beta"]);
    assert.deepEqual(titles(await client.get(`${NOTES}?q=nobody`)), []);
  }));

  test(`each request runs in a statement timeout on postgres (${provider})`, () => world(provider, async (w) => {
    const { client } = register(w);
    const first = (await client.get(NOTES)).sql[0]!;
    assert.equal(w.provider === "postgres", first.startsWith("SET LOCAL statement_timeout = 15000"));
    if (w.provider !== "postgres") return;
    const slow = async () => {
      await w.db.fetchText("SELECT pg_sleep(5)::text");
      return [];
    };
    const { client: slowClient } = register(w, { searchWith: slow }, { statementTimeout: 0.2 });
    const page = await slowClient.get(`${NOTES}?q=x`);
    assert.ok([500, 504].includes(page.status));
    assert.equal(page.sql[0], "SET LOCAL statement_timeout = 200");
  }));
}
