import assert from "node:assert/strict";
import { test } from "node:test";

import { Admin, AdminConfigError } from "../src/admin.js";
import { connect, Decimal, loads, Registry } from "../src/index.js";
import { native } from "../src/native.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";

const SOURCE = `
datasource db {
  provider = "%(provider)s"
}

model AdmAuthor {
  id    BigInt   @id @default(autoincrement())
  name  String
  posts AdmPost[]
  @@map("adm_authors")
}

model AdmPost {
  id         BigInt    @id @default(autoincrement())
  title      String
  views      BigInt
  price      %(decimal)s
  rank       Int
  created_at DateTime
  author_id  BigInt?
  author     AdmAuthor? @relation(fields: [author_id], references: [id])
  @@map("adm_posts")
}
`;

const SU = { is_superuser: true };
const PROVIDERS = ["sqlite", "postgres"] as const;

function source(provider: string): string {
  return SOURCE.replace("%(provider)s", provider).replace("%(decimal)s", provider === "postgres" ? "Decimal" : "Float");
}

// Models are loose here: the columns are reached by name.
type Loose = any;

async function world(provider: (typeof PROVIDERS)[number], run: (w: { site: Admin; db: Awaited<ReturnType<typeof connect>>; Author: Loose; Post: Loose }) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const classes = loads(source(provider), { registry }) as Record<string, Loose>;
  const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : PG_URL, { registry, default: false });
  try {
    await db.dropTables();
    await db.createTables();
    const Author = classes["AdmAuthor"], Post = classes["AdmPost"];
    const [ada, bo] = await Author.objects.using(db).insertMany([{ name: "Ada" }, { name: "Bo" }]).returning();
    const at = new Date("2026-01-02T03:04:05Z");
    const price = (v: string) => (provider === "postgres" ? new Decimal(v) : Number(v));
    await Post.objects.using(db).insertMany([
      { title: "one", views: 9007199254740993n, price: price("1.50"), rank: 1, createdAt: at, authorId: ada.id },
      { title: "two", views: 2n, price: price("2.25"), rank: 1, createdAt: at, authorId: null },
      { title: "three", views: 3n, price: price("3"), rank: 2, createdAt: at, authorId: bo.id },
    ]);
    await run({ site: new Admin(registry), db, Author, Post });
  } finally {
    await db.dropTables();
    await db.close();
  }
}

for (const provider of PROVIDERS) {
  test(`a to-one column is one statement and keeps the row without an author (${provider})`, () => world(provider, async ({ site, db, Post }) => {
    site.register(Post, { list: { columns: [Post.title, Post.author.name], order: [Post.title] } });
    const page = await site.testClient(db, { user: SU }).get("/admin/api/resources/AdmPost");
    assert.equal(page.status, 200);
    assert.equal(page.queries, 1);
    const body = page.json();
    assert.deepEqual(body.columns.map((c: { label: string }) => c.label).slice(0, 2), ["title", "author.name"]);
    assert.deepEqual(body.rows.map((r: unknown[]) => r.slice(0, 2)), [["one", "Ada"], ["three", "Bo"], ["two", null]]);
    assert.equal(body.next, null);
  }));

  test(`keyset pages with the primary key as tie-breaker (${provider})`, () => world(provider, async ({ site, db, Post }) => {
    site.register(Post, { list: { columns: [Post.rank, Post.title], order: [Post.rank.desc()], pageSize: 2 } });
    const client = site.testClient(db, { user: SU });
    const first = (await client.get("/admin/api/resources/AdmPost")).json();
    assert.equal(first.rows.length, 2);
    assert.ok(first.next);
    const second = (await client.get(`/admin/api/resources/AdmPost?cursor=${first.next}`)).json();
    const seen = [...first.rows, ...second.rows].map((r: unknown[]) => r[1]);
    assert.deepEqual([...seen].sort(), ["one", "three", "two"]);
    assert.equal(second.next, null);
  }));

  test(`bigint and decimal are strings and dates carry an offset (${provider})`, () => world(provider, async ({ site, db, Post }) => {
    site.register(Post, { list: { columns: [Post.views, Post.price, Post.createdAt], order: [Post.views.desc()] } });
    const body = (await site.testClient(db, { user: SU }).get("/admin/api/resources/AdmPost")).json();
    const first = body.rows[0];
    assert.equal(first[0], "9007199254740993");
    assert.equal(Number(first[1]), 1.5);
    assert.equal(typeof first[1] === "string", provider === "postgres");
    assert.equal(Date.parse(first[2]), Date.parse("2026-01-02T03:04:05Z"));
    assert.ok(first[2].endsWith("+00:00"));
  }));

  test(`with no config every concrete model is a read-only resource (${provider})`, () => world(provider, async ({ site, db }) => {
    const client = site.testClient(db, { user: SU });
    const listing = (await client.get("/admin/api/resources")).json();
    assert.deepEqual(listing.resources.map((r: { name: string }) => r.name).sort(), ["AdmAuthor", "AdmPost"]);
    assert.ok(listing.resources.every((r: { permissions: string[] }) => r.permissions.length === 1 && r.permissions[0] === "read"));
    assert.equal((await client.get("/admin/api/resources/AdmAuthor")).status, 200);
  }));

  test(`the prefix is a config value (${provider})`, () => world(provider, async ({ site, db }) => {
    site.prefix = "/back/office";
    const client = site.testClient(db, { user: SU });
    assert.equal((await client.get("/back/office/resources/AdmAuthor")).status, 200);
    assert.equal((await client.get("/admin/api/resources/AdmAuthor")).status, 404);
  }));

  test(`a request needs a superuser (${provider})`, () => world(provider, async ({ site, db }) => {
    assert.equal((await site.testClient(db).get("/admin/api/resources")).status, 401);
    assert.equal((await site.testClient(db, { user: { is_superuser: false } }).get("/admin/api/resources")).status, 403);
    assert.equal((await site.testClient(db, { user: SU }).get("/admin/api/resources/Nope")).status, 404);
    assert.equal((await site.testClient(db, { user: SU }).get("/admin/api/resources/AdmPost?cursor=zz")).status, 400);
  }));

  test(`check raises one precise error for a list column through a to-many relation (${provider})`, () => world(provider, async ({ site, db, Author }) => {
    site.register(Author, { list: { columns: [Author.posts.title] } });
    assert.throws(() => site.check(), (e: Error) => e instanceof AdminConfigError && /AdmAuthor/.test(e.message));
    let fromCheck = "", fromClient = "";
    try { site.check(); } catch (e) { fromCheck = (e as Error).message; }
    try { site.testClient(db, { user: SU }); } catch (e) { fromClient = (e as Error).message; }
    assert.ok(fromCheck);
    assert.equal(fromCheck, fromClient);
  }));
}

test("the native site rejects an unknown field", () => {
  const registry = new Registry();
  loads(source("sqlite"), { registry });
  const config = '{"resources":[{"model":"AdmPost","list":{"columns":["nope"]}}]}';
  assert.throws(() => native().adminSite(registry.native(), config), /nope/);
});

test("a column of another model is refused", () => {
  const registry = new Registry();
  const classes = loads(source("sqlite"), { registry }) as Record<string, Loose>;
  const site = new Admin(registry);
  site.register(classes["AdmPost"], { list: { columns: [classes["AdmAuthor"].name] } });
  assert.throws(() => site.check(), (e: Error) => e instanceof AdminConfigError && /not to AdmPost/.test(e.message));
});

test("a model registers once", () => {
  const registry = new Registry();
  const classes = loads(source("sqlite"), { registry }) as Record<string, Loose>;
  const site = new Admin(registry);
  site.register(classes["AdmPost"]);
  assert.throws(() => site.register(classes["AdmPost"]), /already registered/);
});
