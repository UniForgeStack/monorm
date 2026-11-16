// Admin CSV export: the list state as a stream of keyset batches, the cells, the audit row, on PostgreSQL and SQLite.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin } from "../src/admin.js";
import { connect, func, load, param, Registry, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;

const SCHEMA = (provider: string) => `datasource db {
  provider = "${provider}"
}
import "app:admin" (prefix: "admin_")

model ExpOwner {
  id    Int       @id
  name  String
  items ExpItem[]
  @@map("exp_owners")
}

model ExpItem {
  id       Int      @id
  owner_id Int
  title    String
  rank     Int
  big      BigInt
  owner    ExpOwner @relation(fields: [owner_id], references: [id])
  @@index([title])
  @@map("exp_items")
}
`;

const ITEMS = "/admin/api/export/ExpItem";
const OWNERS = "/admin/api/export/ExpOwner";
const SU = { key: 1, is_superuser: true };

type Loose = any;
interface World {
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
  readonly Owner: Loose;
  readonly Item: Loose;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-export-"));
  writeFileSync(join(dir, "package.json"), "{}");
  writeFileSync(join(dir, "schema.prisma"), SCHEMA(provider));
  const registry = new Registry();
  const models = load(join(dir, "schema.prisma"), { registry }) as unknown as Record<string, Loose>;
  let db: Database;
  try {
    db = await connect(provider === "sqlite" ? "sqlite://:memory:" : PG_URL, { registry, default: false });
  } catch (error) {
    rmSync(dir, { recursive: true, force: true });
    if (process.env["ORM_TEST_DATABASE_URL"] !== undefined) throw error;
    return;
  }
  try {
    await db.dropTables();
    await db.createTables();
    const Owner = models["ExpOwner"], Item = models["ExpItem"];
    await Owner.objects.using(db).insertMany([{ id: 1, name: "Ada" }, { id: 2, name: "Bo" }]);
    await Item.objects.using(db).insertMany([
      { id: 1, ownerId: 1, title: "one", rank: 1, big: 1n },
      { id: 2, ownerId: 1, title: "two", rank: 2, big: 2n },
      { id: 3, ownerId: 2, title: "three", rank: 3, big: 9007199254740993n },
      { id: 4, ownerId: 1, title: "four", rank: 3, big: 4n },
    ]);
    await run({ registry, db, models, Owner, Item });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

/** Reads CSV text as rows of cells: a quoted cell may hold a comma, a quote ("") or a line break. */
function parse(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [], cell = "", quoted = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i]!;
    if (quoted) {
      if (c !== '"') cell += c;
      else if (text[i + 1] === '"') (cell += '"', i++);
      else quoted = false;
    } else if (c === '"') quoted = true;
    else if (c === ",") (row.push(cell), (cell = ""));
    else if (c === "\r") continue;
    else if (c === "\n") (row.push(cell), rows.push(row), (row = []), (cell = ""));
    else cell += c;
  }
  if (cell !== "" || row.length > 0) (row.push(cell), rows.push(row));
  return rows;
}

const table = (response: { status: number; text: string }): string[][] => {
  assert.equal(response.status, 200, response.text);
  return parse(response.text);
};
const titles = (response: { status: number; text: string }): string[] => table(response).slice(1).map((row) => row[0]!);

function siteOf(w: World, item: Record<string, unknown> = {}, owner: Record<string, unknown> = {}, queryset?: unknown): Admin {
  const site = new Admin(w.registry);
  const I = w.Item, O = w.Owner;
  site.register(I, {
    list: {
      columns: [I.title, I.rank, I.big, I.owner.name, "title_length"],
      computed: [{ name: "title_length", type: "int", nullable: false, sortable: true, expr: func.length(I.title) }],
      order: [I.id],
      search: [I.title],
      filters: [I.rank, I.owner],
      ...item,
    },
    ...(queryset === undefined ? {} : { queryset }),
  } as never);
  site.register(O, {
    list: { columns: [O.name, "item_count"], aggregates: [{ name: "item_count", label: "Items", relation: O.items, fn: "count" }], order: [O.id], ...owner },
  } as never);
  return site;
}

const clientOf = (w: World, ...args: Parameters<typeof siteOf> extends [World, ...infer R] ? R : never) => siteOf(w, ...args).testClient(w.db, { user: SU });

const exportsOf = async (w: World): Promise<[string, string, string, Loose][]> => {
  const entry = w.models["AdminLogEntry"];
  const rows = await entry.objects.using(w.db).filter(entry.action.eq("export")).orderBy(entry.id);
  return rows.map((r: Loose) => [r.action, r.resource, r.recordKey, r.changes]);
};

for (const provider of PROVIDERS) {
  test(`the export is the list as csv with a to one and a computed column (${provider})`, () => world(provider, async (w) => {
    const r = await clientOf(w).get(ITEMS);
    assert.equal(r.status, 200, r.text);
    assert.equal(r.header("content-type"), "text/csv; charset=utf-8");
    assert.equal(r.header("content-disposition"), 'attachment; filename="ExpItem.csv"');
    assert.ok(r.text.endsWith("\r\n"));
    assert.deepEqual(table(r), [
      ["Title", "Rank", "Big", "Owner name", "Title length", "Id"],
      ["one", "1", "1", "Ada", "3", "1"],
      ["two", "2", "2", "Ada", "3", "2"],
      ["three", "3", "9007199254740993", "Bo", "5", "3"],
      ["four", "3", "4", "Ada", "4", "4"],
    ]);
  }));

  test(`an aggregate column is in the export (${provider})`, () => world(provider, async (w) => {
    assert.deepEqual(table(await clientOf(w).get(OWNERS)), [["Name", "Items", "Id"], ["Ada", "3", "1"], ["Bo", "1", "2"]]);
  }));

  test(`the search the filters and the sort of the list apply (${provider})`, () => world(provider, async (w) => {
    const client = clientOf(w);
    assert.deepEqual(titles(await client.get(`${ITEMS}?q=t`)), ["two", "three"]);
    assert.deepEqual(titles(await client.get(`${ITEMS}?f.rank=3`)), ["three", "four"]);
    assert.deepEqual(titles(await client.get(`${ITEMS}?f.owner=1&f.rank=3`)), ["four"]);
    assert.deepEqual(titles(await client.get(`${ITEMS}?sort=-title_length`)), ["three", "four", "one", "two"]);
    assert.deepEqual(titles(await client.get(`${ITEMS}?sort=-id&q=t`)), ["three", "two"]);
    assert.deepEqual(titles(await client.get(`${ITEMS}?cursor=zz&page=9&limit=1`)), ["one", "two", "three", "four"]);
  }));

  test(`the queryset filter limits the export (${provider})`, () => world(provider, async (w) => {
    const mine = w.Item.objects.filter(w.Item.ownerId.eq(param("user"))).prepare();
    assert.deepEqual(titles(await clientOf(w, {}, {}, mine).get(ITEMS)), ["one", "two", "four"]);
    const other = siteOf(w, {}, {}, mine).testClient(w.db, { user: { key: 2, is_superuser: true } });
    assert.deepEqual(titles(await other.get(ITEMS)), ["three"]);
  }));

  test(`a cell that starts like a formula is escaped and a special cell is quoted (${provider})`, () => world(provider, async (w) => {
    await w.Item.objects.using(w.db).insertMany([
      { id: 5, ownerId: 1, title: "=SUM(A1)", rank: 5, big: 5n },
      { id: 6, ownerId: 1, title: "+1 @x", rank: 5, big: 6n },
      { id: 7, ownerId: 1, title: "-9", rank: 5, big: 7n },
      { id: 8, ownerId: 1, title: 'a,b "c"\nd', rank: 5, big: 8n },
      { id: 9, ownerId: 1, title: "fine=", rank: 5, big: 9n },
    ]);
    const r = await clientOf(w).get(`${ITEMS}?f.rank=5`);
    assert.deepEqual(titles(r), ["'=SUM(A1)", "'+1 @x", "'-9", 'a,b "c"\nd', "fine="]);
    assert.ok(r.text.includes('"a,b ""c""\nd"'), "a cell with a comma, a quote or a line break is quoted");
    assert.deepEqual(table(r).slice(1).map((row) => row[2]), ["5", "6", "7", "8", "9"], "a number is never escaped");
  }));

  test(`a big export is read in keyset batches of a thousand rows and a batch function runs once per batch (${provider})`, () => world(provider, async (w) => {
    const batches: number[] = [];
    const doubled = async (keys: number[]) => {
      batches.push(keys.length);
      return keys.map((key) => key * 2);
    };
    const rows = Array.from({ length: 2500 }, (_, i) => ({ id: 10 + i, ownerId: 1, title: `m${String(i).padStart(4, "0")}`, rank: 7, big: BigInt(i) }));
    await w.Item.objects.using(w.db).insertMany(rows);
    const computed = [{ name: "doubled", type: "int", nullable: false, function: doubled }];
    const r = await clientOf(w, { columns: [w.Item.title, "doubled"], computed }).get(`${ITEMS}?f.rank=7`);
    const got = table(r);
    assert.deepEqual(batches, [1000, 1000, 500]);
    assert.deepEqual(got[0], ["Title", "Doubled", "Id"]);
    assert.equal(got.length, 2501);
    assert.deepEqual([got[1], got[1000], got[1001], got[2500]].map((row) => [row![0], row![1], row![2]]), [
      ["m0000", "20", "10"],
      ["m0999", "2018", "1009"],
      ["m1000", "2020", "1010"],
      ["m2499", "5018", "2509"],
    ]);
    assert.deepEqual(got.slice(1).map((row) => row[2]), rows.map((row) => String(row.id)), "every row once, in keyset order");
  }));

  test(`each export writes one audit row with the query and a refused export writes none (${provider})`, () => world(provider, async (w) => {
    const client = clientOf(w);
    assert.equal((await client.get(`${ITEMS}?q=t&sort=-id`)).status, 200);
    assert.deepEqual(await exportsOf(w), [["export", "ExpItem", "", { query: "q=t&sort=-id" }]]);
    assert.equal((await client.get(`${ITEMS}?sort=nothing`)).status, 400);
    assert.equal((await siteOf(w).testClient(w.db, { user: { key: 7 } }).get(ITEMS)).status, 403);
    assert.equal((await siteOf(w).testClient(w.db).get(ITEMS)).status, 401);
    assert.equal((await exportsOf(w)).length, 1);
  }));

  test(`a bad request is a plain json error before the file starts (${provider})`, () => world(provider, async (w) => {
    const client = clientOf(w);
    const bad = await client.get(`${ITEMS}?sort=nothing`);
    assert.equal(bad.status, 400);
    assert.equal(bad.json().error.kind, "BadRequest");
    assert.equal((await client.get(`${ITEMS}?f.rank=x`)).status, 422);
    assert.equal((await client.get("/admin/api/export/Nothing")).status, 404);
  }));
}
