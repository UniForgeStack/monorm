// Admin lists: filters, computed and aggregate columns, and to-many tabs, on PostgreSQL and SQLite.
import assert from "node:assert/strict";
import { test } from "node:test";

import { Admin, AdminConfigError, type ListConfig } from "../src/admin.js";
import { connect, func, loads, param, Registry } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";

const SOURCE = `
datasource db {
  provider = "%(provider)s"
}

enum FltLevel {
  low
  high

  @@storage(text)
}

model FltAuthor {
  id       Int          @id
  name     String
  active   Boolean
  nick     String?
  comments FltComment[]
  @@map("flt_authors")
}

model FltComment {
  id        Int        @id
  body      String
  author_id Int?
  level     FltLevel
  rank      Int
  at        DateTime
  author    FltAuthor? @relation(fields: [author_id], references: [id])
  @@map("flt_comments")
}
`;

const SU = { is_superuser: true };
const COMMENTS = "/admin/api/resources/FltComment";
const AUTHORS = "/admin/api/resources/FltAuthor";
const PROVIDERS = ["sqlite", "postgres"] as const;

// Models are loose here: the columns are reached by name.
type Loose = any;
type Database = Awaited<ReturnType<typeof connect>>;
interface World {
  registry: Registry;
  db: Database;
  Author: Loose;
  Comment: Loose;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const models = loads(SOURCE.replace("%(provider)s", provider), { registry }) as Record<string, Loose>;
  const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : PG_URL, { registry, default: false });
  try {
    await db.dropTables();
    await db.createTables();
    const Author = models["FltAuthor"], Comment = models["FltComment"];
    await Author.objects.using(db).insertMany([
      { id: 1, name: "Ada", active: true, nick: null },
      { id: 2, name: "Bo", active: false, nick: "" },
      { id: 3, name: "Cy", active: true, nick: "cy" },
    ]);
    const day = (...a: number[]) => new Date(Date.UTC(a[0]!, a[1]! - 1, a[2], a[3] ?? 0, a[4] ?? 0, a[5] ?? 0));
    await Comment.objects.using(db).insertMany([
      { id: 1, body: "one", authorId: 1, level: "high", rank: 1, at: day(2026, 1, 2, 3, 4, 5) },
      { id: 2, body: "two", authorId: null, level: "low", rank: 1, at: day(2026, 2, 1) },
      { id: 3, body: "three", authorId: 2, level: "high", rank: 2, at: day(2026, 3, 1) },
      { id: 4, body: "four", authorId: 1, level: "low", rank: 3, at: day(2026, 3, 15, 12) },
    ]);
    await run({ registry, db, Author, Comment });
  } finally {
    await db.dropTables();
    await db.close();
  }
}

function siteOf(w: World, comment: ListConfig = {}, author: ListConfig = {}, options: ConstructorParameters<typeof Admin>[1] = {}) {
  const site = new Admin(w.registry, options);
  const C = w.Comment, A = w.Author;
  site.register(C, { list: { columns: [C.body], order: [C.id], filters: [C.level, C.rank, C.at, C.author, C.author.name, C.author.active], ...comment } });
  site.register(A, { list: { columns: [A.name, A.active, A.nick], order: [A.id], filters: [A.active, A.nick, A.name], ...author } });
  return { site, client: site.testClient(w.db, { user: SU }) };
}

const first = (response: { json(): any }): string[] => response.json().rows.map((row: unknown[]) => row[0]);
const resourceOf = async (client: ReturnType<typeof siteOf>["client"], name: string) =>
  (await client.get("/admin/api/resources")).json().resources.find((r: { name: string }) => r.name === name);

for (const provider of PROVIDERS) {
  test(`an enum a bool and a field of a related model filter and several values mean or (${provider})`, () => world(provider, async (w) => {
    const { client } = siteOf(w);
    const ask = (q: string) => client.get(`${COMMENTS}?${q}`);
    assert.deepEqual(first(await ask("f.level=high")), ["one", "three"]);
    assert.deepEqual(first(await ask("f.level=high&f.level=low")), ["one", "two", "three", "four"]);
    assert.deepEqual(first(await ask("f.author.active=true")), ["one", "four"]);
    assert.deepEqual(first(await ask("f.author.name=Bo")), ["three"]);
    assert.deepEqual(first(await ask("f.level=high&f.author.name=Ada")), ["one"]);
    const related = await ask("f.author.name=Bo");
    assert.equal(related.sql.length, (await client.get("/admin/api/resources")).sql.length + 1);
    assert.deepEqual(first(await client.get(`${AUTHORS}?f.active=false`)), ["Bo"]);
  }));

  test(`a range and a relation filter and the empty operators (${provider})`, () => world(provider, async (w) => {
    const { client } = siteOf(w);
    const ask = (q: string) => client.get(`${COMMENTS}?${q}`);
    assert.deepEqual(first(await ask("f.rank:gte=2")), ["three", "four"]);
    assert.deepEqual(first(await ask("f.rank:gt=1&f.rank:lt=3")), ["three"]);
    assert.deepEqual(first(await ask("f.at:gte=2026-03-01&f.at:lt=2026-03-02")), ["three"]);
    assert.deepEqual(first(await ask("f.at:gte=2026-03-15T12:00:00Z")), ["four"]);
    assert.deepEqual(first(await ask("f.author=1")), ["one", "four"]);
    assert.deepEqual(first(await ask("f.author=1&f.author=2")), ["one", "three", "four"]);
    assert.deepEqual(first(await ask("f.author:empty")), ["two"]);
    assert.deepEqual(first(await ask("f.author:notempty")), ["one", "three", "four"]);
    assert.deepEqual(first(await client.get(`${AUTHORS}?f.nick:empty`)), ["Ada", "Bo"]);
    assert.deepEqual(first(await client.get(`${AUTHORS}?f.nick:notempty`)), ["Cy"]);
  }));

  test(`a wrong value is 422 and an unknown filter is 400 (${provider})`, () => world(provider, async (w) => {
    const { client } = siteOf(w);
    for (const q of ["f.level=middle", "f.rank=x", "f.rank:gte=1&f.rank:gte=2", "f.at=yesterday", "f.author=x", "f.level:gt=1"]) {
      const response = await client.get(`${COMMENTS}?${q}`);
      assert.equal(response.status, 422, q);
      assert.equal(response.json().error.kind, "Invalid", q);
    }
    for (const q of ["f.body=one", "n.nothing=x", "f.rank:between=1"]) {
      assert.equal((await client.get(`${COMMENTS}?${q}`)).status, 400, q);
    }
  }));

  test(`an unknown or unfit filter field is a build error (${provider})`, () => world(provider, async (w) => {
    const C = w.Comment;
    const build = (filters: unknown[]) => {
      const site = new Admin(w.registry);
      site.register(C, { list: { filters: filters as never } });
      site.register(w.Author);
      site.check();
    };
    assert.throws(() => build(["nope"]), (e) => e instanceof AdminConfigError && /nope/.test(e.message));
    assert.throws(() => build([w.Author.comments]), (e) => e instanceof AdminConfigError && /comments/.test(e.message));
    assert.throws(() => build(["author.nope"]), (e) => e instanceof AdminConfigError && /author\.nope/.test(e.message));
  }));

  test(`a named filter has a default and an all choice (${provider})`, () => world(provider, async (w) => {
    const C = w.Comment;
    const namedFilters = [{
      name: "recent",
      label: "Age",
      default: "march",
      choices: [
        { name: "march", label: "Rank two or more", condition: C.rank.gte(2) },
        { name: "first", label: "Rank one", condition: C.rank.eq(1) },
      ],
    }];
    const { client } = siteOf(w, { namedFilters });
    assert.deepEqual(first(await client.get(COMMENTS)), ["three", "four"]);
    assert.deepEqual(first(await client.get(`${COMMENTS}?n.recent=first`)), ["one", "two"]);
    assert.deepEqual(first(await client.get(`${COMMENTS}?n.recent=all`)), ["one", "two", "three", "four"]);
    assert.equal((await client.get(`${COMMENTS}?n.recent=nope`)).status, 422);
    const resource = await resourceOf(client, "FltComment");
    assert.equal(resource.named_filters[0].default, "march");
    assert.deepEqual(resource.named_filters[0].choices[1], { name: "first", title: "Rank one" });
    assert.equal((await client.get("/admin/api/counts/FltComment")).json().count, 2);
  }));

  test(`a named filter with a parameter is a build error (${provider})`, () => world(provider, async (w) => {
    const C = w.Comment;
    const site = new Admin(w.registry);
    const namedFilters = [{ name: "n", label: "N", choices: [{ name: "c", label: "C", condition: C.rank.eq(param("r")) }] }];
    assert.throws(() => {
      site.register(C, { list: { namedFilters } });
      site.check();
    }, (e) => e instanceof AdminConfigError && /placeholder|literal/.test(e.message));
  }));

  test(`an aggregate over a relation is one statement (${provider})`, () => world(provider, async (w) => {
    const A = w.Author;
    const aggregates = [
      { name: "comment_count", label: "Comments", relation: A.comments, fn: "count" as const },
      { name: "top_rank", label: "Top rank", relation: A.comments, fn: "max" as const, field: A.comments.rank },
    ];
    const { client } = siteOf(w, {}, { columns: [A.name, "comment_count", "top_rank"], aggregates });
    const preamble = (await client.get("/admin/api/resources")).sql.length;
    const page = await client.get(AUTHORS);
    assert.equal(page.status, 200, JSON.stringify(page.json()));
    assert.equal(page.sql.length, preamble + 1, "no N+1");
    const rows = page.json().rows;
    assert.deepEqual(rows.map((r: unknown[]) => Number(r[1])), [2, 1, 0]);
    assert.deepEqual(rows.map((r: unknown[]) => r[2]), [3, 2, null]);
    assert.equal(page.json().columns[1].title, "Comments");
    const deeper = [{ name: "author_max", relation: A.comments, fn: "max" as const, field: A.comments.author.name }];
    assert.throws(() => siteOf(w, {}, { columns: [A.name, "author_max"], aggregates: deeper }), (e) => e instanceof AdminConfigError && /field of the relation/.test(e.message));
  }));

  test(`a computed expression column is in the query and sorts when the config allows (${provider})`, () => world(provider, async (w) => {
    const C = w.Comment;
    const computed = [{ name: "body_length", type: "int", nullable: false, sortable: true, expr: func.length(C.body) }];
    let { client } = siteOf(w, { columns: [C.body, "body_length"], computed });
    const page = await client.get(`${COMMENTS}?sort=body_length`);
    assert.deepEqual(first(page), ["one", "two", "four", "three"]);
    assert.deepEqual(page.json().rows.map((r: unknown[]) => r[1]), [3, 3, 4, 5]);
    assert.deepEqual(first(await client.get(`${COMMENTS}?sort=-body_length`)), ["three", "four", "one", "two"]);
    const plain = [{ name: "body_length", type: "int", expr: func.length(C.body) }];
    ({ client } = siteOf(w, { columns: [C.body, "body_length"], computed: plain }));
    assert.equal((await client.get(`${COMMENTS}?sort=body_length`)).status, 400);
  }));

  test(`a computed column with the wrong type or a name clash is a build error (${provider})`, () => world(provider, async (w) => {
    const C = w.Comment;
    const length = func.length(C.body);
    const build = (column: Record<string, unknown>) => {
      const site = new Admin(w.registry);
      site.register(C, { list: { computed: [column as never] } });
      site.register(w.Author);
      site.check();
    };
    assert.throws(() => build({ name: "n", type: "text", expr: length }), (e) => e instanceof AdminConfigError && /declared/.test(e.message));
    assert.throws(() => build({ name: "body", type: "int", expr: length }), (e) => e instanceof AdminConfigError && /name of a field/.test(e.message));
    assert.throws(() => build({ name: "n", type: "int" }), (e) => e instanceof AdminConfigError && /expr or function/.test(e.message));
  }));

  test(`a batch function column runs once for the page (${provider})`, () => world(provider, async (w) => {
    const C = w.Comment;
    const calls: unknown[] = [];
    const doubled = async (keys: readonly any[]) => {
      calls.push(keys);
      return keys.map((key) => key * 2);
    };
    let { client } = siteOf(w, { columns: [C.body, "doubled"], computed: [{ name: "doubled", type: "int", nullable: false, function: doubled }], pageSize: 3 });
    const page = await client.get(COMMENTS);
    assert.equal(page.status, 200, JSON.stringify(page.json()));
    assert.deepEqual(calls, [[1, 2, 3]], "one call for three rows");
    assert.deepEqual(page.json().rows.map((r: unknown[]) => r[1]), [2, 4, 6]);
    const wrong = async (keys: readonly unknown[]) => keys.map(() => "x");
    ({ client } = siteOf(w, { columns: [C.body, "doubled"], computed: [{ name: "doubled", type: "int", nullable: false, function: wrong }] }));
    assert.equal((await client.get(COMMENTS)).status, 500, "a value of the wrong type is a server error");
    assert.equal((await client.get(`${COMMENTS}?sort=doubled`)).status, 400);
  }));

  test(`a to-many tab lists the children of one record with the child list features (${provider})`, () => world(provider, async (w) => {
    const C = w.Comment;
    const { client } = siteOf(w, { search: [C.body], pageSize: 10 });
    const tab = "/admin/api/resources/FltAuthor/1/comments";
    const page = await client.get(tab);
    assert.equal(page.status, 200, JSON.stringify(page.json()));
    assert.deepEqual(first(page), ["one", "four"]);
    assert.deepEqual(first(await client.get(`${tab}?f.level=high`)), ["one"]);
    assert.deepEqual(first(await client.get(`${tab}?q=fou`)), ["four"]);
    assert.deepEqual(first(await client.get(`${tab}?sort=-id`)), ["four", "one"]);
    assert.equal((await client.get("/admin/api/counts/FltAuthor/1/comments")).json().count, 2);
    assert.equal((await client.get("/admin/api/resources/FltAuthor/99/comments")).status, 404);
    assert.equal((await client.get("/admin/api/resources/FltAuthor/1/nothing")).status, 404);
    assert.deepEqual((await resourceOf(client, "FltAuthor")).tabs, [{ name: "comments", title: "Comments", target: "FltComment" }]);
  }));

  test(`labels and empty texts reach the metadata (${provider})`, () => world(provider, async (w) => {
    const C = w.Comment;
    const { client } = siteOf(w, { columns: [C.body, C.author.name], labels: { body: "Text" }, empty: "none", emptyFields: { "author.name": "no author" } });
    const resource = await resourceOf(client, "FltComment");
    assert.equal(resource.columns[0].title, "Text");
    assert.equal(resource.columns[0].empty, "none");
    assert.equal(resource.columns[1].empty, "no author");
    const site = new Admin(w.registry);
    site.register(C, { list: { labels: { nope: "x" } } });
    site.register(w.Author);
    assert.throws(() => site.check(), (e) => e instanceof AdminConfigError && /nope/.test(e.message));
    const big = [{ name: "big", type: "int", expr: func.coalesce(C.rank, 2n ** 60n as unknown as number) }];
    assert.throws(() => siteOf(w, { columns: [C.body, "big"], computed: big }), (e) => e instanceof AdminConfigError && /safe integer/.test(e.message));
  }));
}
