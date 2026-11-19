/** CASE and JSON paths in GROUP BY, HAVING, ORDER BY and DISTINCT: each copy has equal SQL text. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { Registry, connect, func, loads } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const PLAIN = `
model Item {
  id  BigInt @id @default(autoincrement())
  qty Int
  @@map("grp_items_js")
}
`;
const SQLITE = 'datasource db {\n  provider = "sqlite"\n}\n';
const JSON_DOC = `
model Doc {
  id   BigInt @id @default(autoincrement())
  meta Json
  @@map("grp_docs_js")
}
`;
const NUM = `
model Num {
  id BigInt  @id @default(autoincrement())
  f  Float?
  @@map("grp_nums_js")
}
`;

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Model = any;

async function open(source: string, name: string, url: string) {
  const registry = new Registry();
  const model = loads(source, { registry })[name] as Model;
  const db = await connect(url, { default: false, registry, maxConnections: 2 });
  await db.dropTables();
  await db.createTables();
  return { db, model };
}

for (const provider of ["postgres", "sqlite"] as const) {
  const url = provider === "postgres" ? DATABASE_URL : "sqlite://:memory:";
  const source = provider === "postgres" ? PLAIN : SQLITE + PLAIN;

  async function withItems(run: (objects: Model, Item: Model) => Promise<void>) {
    const { db, model } = await open(source, "Item", url);
    try {
      await model.objects.using(db).insertMany([1, 2, 3, 20, 30].map((qty) => ({ qty })));
      await run(model.objects.using(db), model);
    } finally {
      await db.dropTables();
      await db.close();
    }
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const size = (Item: Model): any => func.case([Item.qty.lt(10), "small"], { default: "large" });

  // case: grouped_expressions::case_in_group_by_having_and_order_by
  test(`case in group by, having and order by (${provider})`, async () => {
    await withItems(async (objects, Item) => {
      const kase = size(Item);
      const pairs = (rows: { size: string; n: unknown }[]) => rows.map((r) => [r.size, Number(r.n)]);
      const filtered = await objects.select({ size: kase, n: func.count() }).groupBy(kase).having(func.count().gt(2)).orderBy(kase).all();
      assert.deepEqual(pairs(filtered), [["small", 3]]);
      const ordered = await objects.select({ size: kase, n: func.count() }).groupBy(kase).orderBy(kase.desc()).all();
      assert.deepEqual(pairs(ordered), [["small", 3], ["large", 2]]);
    });
  });

  // case: grouped_expressions::case_in_having_matching_the_group
  test(`case in having matching the group (${provider})`, async () => {
    await withItems(async (objects, Item) => {
      const kase = size(Item);
      const rows = await objects.select({ size: kase }).groupBy(kase).having(kase.eq("large")).all();
      assert.deepEqual(rows.map((r: { size: string }) => r.size), ["large"]);
    });
  });

  // case: grouped_expressions::case_in_distinct_and_order_by
  test(`case in distinct and order by (${provider})`, async () => {
    await withItems(async (objects, Item) => {
      const kase = size(Item);
      assert.deepEqual(await objects.select({ size: kase }).distinct().orderBy(kase).scalars(), ["large", "small"]);
    });
  });
}

async function withDocs(run: (objects: Model, Doc: Model) => Promise<void>) {
  const { db, model } = await open(JSON_DOC, "Doc", DATABASE_URL);
  try {
    await model.objects.using(db).insertMany(
      [["a", 1], ["a", 2], ["b", 3], ["it's \\ $1", 4]].map(([kind, n]) => ({ meta: { kind, n } })),
    );
    await run(model.objects.using(db), model);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

// case: grouped_expressions::json_key_with_quote_backslash_and_dollar_reaches_postgres
test("json key with quote backslash and dollar reaches postgres", async () => {
  await withDocs(async (objects, Doc) => {
    await objects.insertMany([{ meta: { "it's \\ $1 --": "v" } }, { meta: { "it's \\ $1 --": "v" } }]);
    const key = Doc.meta.get("it's \\ $1 --").asText();
    const rows = await objects.select({ k: key, n: func.count() }).groupBy(key).orderBy(key).all();
    assert.deepEqual(rows.map((r: { k: string | null; n: unknown }) => [r.k, Number(r.n)]), [["v", 2], [null, 4]]);
  });
});

// case: grouped_expressions::json_paths_in_group_by_having_order_by_distinct
test("json paths in group by, having, order by and distinct", async () => {
  await withDocs(async (objects, Doc) => {
    const kind = Doc.meta.get("kind").asText();
    const grouped = await objects.select({ kind, n: func.count() }).groupBy(kind).orderBy(kind).all();
    assert.deepEqual(grouped.map((r: { kind: string; n: unknown }) => [r.kind, Number(r.n)]), [["a", 2], ["b", 1], ["it's \\ $1", 1]]);
    const having = await objects.select({ kind }).groupBy(kind).having(kind.eq("a")).all();
    assert.deepEqual(having.map((r: { kind: string }) => r.kind), ["a"]);
    assert.deepEqual(await objects.select({ kind }).distinct().orderBy(kind).scalars(), ["a", "b", "it's \\ $1"]);
    assert.deepEqual(await objects.filter(kind.eq("it's \\ $1")).select({ id: Doc.id }).scalars(), [4n]);
    const nested = Doc.meta.get("kind");
    assert.equal((await objects.select({ nested }).distinct().orderBy(nested).all()).length, 3);
  });
});

// case: grouped_expressions::typed_read_of_a_real_column
test("typed read of a real column", async () => {
  const { db, model: Num } = await open(NUM, "Num", DATABASE_URL);
  try {
    await db.execute("ALTER TABLE grp_nums_js ALTER COLUMN f TYPE real");
    const objects = Num.objects.using(db);
    await objects.insertMany([{ f: 0.5 }]);
    assert.deepEqual((await objects.all()).map((n: { f: number }) => n.f), [0.5]);
    assert.deepEqual(await objects.select({ f: Num.f }).scalars(), [0.5]);
    assert.deepEqual(await objects.select({ a: func.arrayAgg(Num.f) }).scalars(), [[0.5]]);
  } finally {
    await db.dropTables();
    await db.close();
  }
});
