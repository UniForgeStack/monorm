/** `Select` count, exists, prepare, explain, CTEs, lazy one-row reads, `at()`, `last()` and paging. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { DoesNotExist, MultipleObjectsReturned, QueryError, Registry, connect, func, getDatabase, loads, param } from "../src/index.js";
import { Post, User } from "./blog/models.js";
import { useDatabase } from "./helpers.js";

useDatabase();

type Dynamic = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const SQLITE_SOURCE = `datasource db { provider = "sqlite" }
model Author {
  id    BigInt @id @default(autoincrement())
  email String @unique
  name  String
  books Book[]
}
model Book {
  id       BigInt @id @default(autoincrement())
  authorId BigInt
  title    String
  author   Author @relation(fields: [authorId], references: [id])
}`;

async function seed() {
  const [alice, bob, carol] = (await User.objects.insertMany([
    { email: "a@example.com", name: "Alice" }, { email: "b@example.com", name: "Bob" }, { email: "c@example.com", name: "Carol" },
  ]).returning()) as [User, User, User];
  await Post.objects.insertMany([
    { author: alice, title: "a1", body: "", views: 80 },
    { author: alice, title: "a2", body: "", views: 40 },
    { author: bob, title: "b1", body: "", views: 5 },
  ]);
  return { alice, bob, carol };
}

const perAuthor = () => Post.objects.select({ authorId: Post.authorId, n: func.count() }).groupBy(Post.authorId);

async function sqlOf(run: () => Promise<unknown>): Promise<string[]> {
  const seen: string[] = [];
  const off = getDatabase().onQuery((e) => { seen.push(e.sql.toLowerCase()); });
  try {
    await run();
  } finally {
    off();
  }
  return seen;
}

test("count counts the rows of a plain select", async () => {
  await seed();
  assert.equal(await Post.objects.select({ t: Post.title }).count(), 3);
  assert.equal(await Post.objects.filter(Post.views.gt(10)).select({ t: Post.title }).count(), 2);
});

test("count counts groups, not rows", async () => {
  await seed();
  assert.equal(await perAuthor().count(), 2);
  assert.equal(await perAuthor().having(func.count().gt(1)).count(), 1);
});

test("count follows distinct and slices", async () => {
  await seed();
  assert.equal(await Post.objects.select({ a: Post.authorId }).distinct().count(), 2);
  assert.equal(await Post.objects.select({ t: Post.title }).orderBy(Post.title).slice(0, 2).count(), 2);
});

test("count of an aggregate without groups is one", async () => {
  await seed();
  assert.equal(await Post.objects.select({ s: func.sum(Post.views) }).count(), 1);
});

test("exists follows the groups", async () => {
  await seed();
  assert.equal(await perAuthor().exists(), true);
  assert.equal(await perAuthor().having(func.count().gt(5)).exists(), false);
  assert.equal(await Post.objects.filter(Post.views.gt(1000)).select({ t: Post.title }).exists(), false);
});

test("count and exists SQL shape", async () => {
  await seed();
  const events = await sqlOf(async () => {
    await perAuthor().having(func.count().gt(1)).count();
    await perAuthor().exists();
    await Post.objects.select({ t: Post.title }).count();
  });
  const [groupedCount, groupedExists, plainCount] = events as [string, string, string];
  assert.ok(groupedCount.startsWith("select count(*) from (select") && groupedCount.includes("group by") && groupedCount.includes("having"), groupedCount);
  assert.ok(groupedExists.startsWith("select exists(select") && groupedExists.includes("group by"), groupedExists);
  assert.ok(plainCount.startsWith("select count(*) from (select"), plainCount);
});

test("explain returns a plan", async () => {
  await seed();
  const plan = await perAuthor().explain();
  assert.match(plan, /Aggregate|Scan/);
});

test("prepare runs with values", async () => {
  await seed();
  const q = Post.objects.filter(Post.views.gt(param("min"))).select({ t: Post.title, v: Post.views }).orderBy(Post.title).prepare();
  assert.deepEqual((await q.all({ min: 10 })).map((r) => [r.t, r.v]), [["a1", 80], ["a2", 40]]);
  assert.deepEqual((await q.all({ min: 50 })).map((r) => r.t), ["a1"]);
  assert.equal((await q.first({ min: 10 }))!.t, "a1");
  assert.equal(await q.first({ min: 1000 }), null);
  assert.equal(await q.count({ min: 10 }), 2);
  assert.equal(await q.exists({ min: 1000 }), false);
  assert.deepEqual([...q.params], ["min"]);
});

test("prepare scalars and scalar", async () => {
  await seed();
  const q = Post.objects.filter(Post.views.gt(param("min"))).select({ v: Post.views }).orderBy(Post.views).prepare();
  assert.deepEqual(await q.scalars({ min: 10 }), [40, 80]);
  assert.equal(await q.scalar({ min: 10 }), 40);
  assert.equal(await q.scalar({ min: 1000 }), null);
});

test("prepare counts groups", async () => {
  await seed();
  const q = Post.objects.filter(Post.views.gt(param("min"))).select({ a: Post.authorId, n: func.count() }).groupBy(Post.authorId).prepare();
  assert.equal(await q.count({ min: 0 }), 2);
  assert.equal(await q.count({ min: 50 }), 1);
});

test("prepare require throws on no row", async () => {
  await seed();
  const q = Post.objects.filter(Post.views.gt(param("min"))).select({ t: Post.title }).require().prepare();
  await assert.rejects(() => q.first({ min: 1000 }), DoesNotExist);
  await assert.rejects(() => q.all({ min: 1000 }), DoesNotExist);
});

test("prepared one-row queries of a select", async () => {
  await seed();
  const byTitle = Post.objects.select({ t: Post.title, v: Post.views }).filter(Post.title.eq(param("t")));
  assert.deepEqual(await byTitle.first().prepare().run({ t: "a1" }), { t: "a1", v: 80 });
  assert.equal(await byTitle.first().prepare().run({ t: "zz" }), null);
  assert.deepEqual(await byTitle.get().prepare().run({ t: "b1" }), { t: "b1", v: 5 });
  const many = Post.objects.select({ t: Post.title }).filter(Post.views.gt(param("min"))).get().prepare();
  await assert.rejects(() => many.run({ min: 1 }), MultipleObjectsReturned);
});

test("first and get are lazy", async () => {
  await seed();
  const first = Post.objects.select({ t: Post.title }).orderBy(Post.title).first();
  assert.deepEqual(await first, { t: "a1" });
  assert.deepEqual(await first, { t: "a1" });
  assert.equal(await Post.objects.select({ t: Post.title }).get(Post.title.eq("zz")), null);
  await assert.rejects(async () => await Post.objects.select({ t: Post.title }).get(Post.views.gt(1)), MultipleObjectsReturned);
  await assert.rejects(async () => await Post.objects.select({ t: Post.title }).require().get(Post.title.eq("zz")), DoesNotExist);
  assert.equal(first.sql().toLowerCase().split("limit").length - 1, 1);
});

test("at reads the row at a position", async () => {
  await seed();
  const q = Post.objects.select({ t: Post.title }).orderBy(Post.title);
  assert.deepEqual(await q.at(0), { t: "a1" });
  assert.deepEqual(await q.at(2), { t: "b1" });
  assert.equal(await q.at(3), null);
  assert.deepEqual(await q.at(-1), { t: "b1" });
  assert.throws(() => q.at(-2), RangeError);
});

test("last reverses the order", async () => {
  await seed();
  assert.deepEqual(await Post.objects.select({ t: Post.title }).orderBy(Post.title).last(), { t: "b1" });
  assert.deepEqual(await Post.objects.select({ t: Post.title }).orderBy(Post.title.desc()).last(), { t: "a1" });
  assert.equal(Number((await perAuthor().orderBy(Post.authorId).last())!.n), 1);
});

test("last and at need an order on grouped and distinct selects", () => {
  for (const q of [
    perAuthor(),
    Post.objects.select({ a: Post.authorId }).distinct(),
    Post.objects.select({ s: func.sum(Post.views) }),
  ] as Dynamic[]) {
    assert.throws(() => q.last(), { name: "TypeError", message: /orderBy/ });
    assert.throws(() => q.at(0), { name: "TypeError", message: /orderBy/ });
    assert.throws(() => q.at(param("n")), { name: "TypeError", message: /orderBy/ });
  }
});

test("last refuses an order past the distinct(on) columns", () => {
  const q = Post.objects.select({ a: Post.authorId, t: Post.title }).distinct(Post.authorId) as Dynamic;
  assert.throws(() => q.orderBy(Post.authorId, Post.views.desc()).last(), { name: "TypeError", message: /distinct/ });
  assert.throws(() => q.orderBy(Post.authorId, Post.views.desc()).at(-1), { name: "TypeError", message: /distinct/ });
  assert.doesNotThrow(() => q.orderBy(Post.authorId).last());
});

test("at takes a param", async () => {
  await seed();
  const q = Post.objects.select({ t: Post.title }).orderBy(Post.title).at(param("n")).prepare();
  assert.deepEqual(await q.run({ n: 1 }), { t: "a2" });
});

test("join and from a CTE in a select", async () => {
  await seed();
  const totals = perAuthor().cte("totals");
  const rows = await User.objects
    .join(totals, totals.c.authorId.eq(User.id))
    .select({ name: User.name, n: totals.c.n })
    .orderBy(totals.c.n.desc(), User.name);
  assert.deepEqual(rows.map((r) => [r.name, Number(r.n)]), [["Alice", 2], ["Bob", 1]]);
  const grouped = await User.objects
    .join(totals, totals.c.authorId.eq(User.id))
    .select({ n: totals.c.n, users: func.count() })
    .groupBy(totals.c.n)
    .having(totals.c.n.gt(0))
    .orderBy(totals.c.n);
  assert.deepEqual(grouped.map((r) => [Number(r.n), Number(r.users)]), [[1, 1], [2, 1]]);
  const ranked = Post.objects.select({ post: Post, rank: func.rowNumber().over({ orderBy: [Post.views.desc()] }) }).cte("ranked");
  const top = (Post.objects.from(ranked as Dynamic) as Dynamic).select({ t: Post.title, rank: ranked.c.rank }).filter(ranked.c.rank.lte(2)).orderBy(ranked.c.rank);
  assert.deepEqual((await top).map((r: Dynamic) => [r.t, Number(r.rank)]), [["a1", 1], ["a2", 2]]);
  assert.equal(await top.count(), 2);
});

test("join after select orders and groups by the CTE's columns", async () => {
  await seed();
  const totals = perAuthor().cte("totals");
  const rows = await Post.objects.select({ id: Post.id }).join(totals, totals.c.authorId.eq(Post.authorId)).orderBy(totals.c.n.desc(), Post.id);
  assert.deepEqual(rows.map((r) => Number(r.id)), [1, 2, 3]);
  const grouped = Post.objects.select({ n: func.count() }).join(totals, totals.c.authorId.eq(Post.authorId)).groupBy(totals.c.n).orderBy(totals.c.n);
  assert.deepEqual((await grouped).map((r) => Number(r.n)), [1, 2]);
});

test("paginate a grouped select", async () => {
  await seed();
  const q = perAuthor().orderBy(Post.authorId);
  const page = await q.paginate({ first: 1 });
  assert.deepEqual(page.items.map((r) => Number(r.n)), [2]);
  assert.ok(page.hasNext && !page.hasPrevious);
  const next = await q.paginate({ first: 1, after: page.nextCursor });
  assert.deepEqual(next.items.map((r) => Number(r.n)), [1]);
  assert.ok(!next.hasNext && next.hasPrevious);
  const back = await q.paginate({ last: 1, before: next.previousCursor });
  assert.deepEqual(back.items.map((r) => Number(r.n)), [2]);
});

test("paginate walks a plain select with the key in the list", async () => {
  await seed();
  const q = Post.objects.select({ id: Post.id, t: Post.title }).orderBy(Post.title);
  const seen: string[] = [];
  let cursor: string | null = null;
  for (;;) {
    const page = await q.paginate({ first: 2, after: cursor });
    seen.push(...page.items.map((r) => r.t));
    if (!page.hasNext) break;
    cursor = page.nextCursor;
  }
  assert.deepEqual(seen, ["a1", "a2", "b1"]);
});

test("paginate a distinct-on select", async () => {
  await seed();
  const q = Post.objects.select({ a: Post.authorId, t: Post.title }).distinct(Post.authorId).orderBy(Post.authorId, Post.title);
  const page = await q.paginate({ first: 1 });
  assert.deepEqual(page.items.map((r) => r.t), ["a1"]);
  const next = await q.paginate({ first: 1, after: page.nextCursor });
  assert.deepEqual(next.items.map((r) => r.t), ["b1"]);
  assert.ok(!next.hasNext);
});

test("paginate rejects what it cannot page", async () => {
  await seed();
  await assert.rejects(() => Post.objects.select({ a: Post.authorId }).distinct().paginate({ first: 1 }), { name: "QueryError", message: /distinct/ });
  await assert.rejects(() => Post.objects.select({ t: Post.title }).orderBy(Post.title).paginate({ first: 1 }), { name: "QueryError", message: /select/ });
  await assert.rejects(() => Post.objects.select({ id: Post.id }).orderBy(Post.id).slice(0, 2).paginate({ first: 1 }), { name: "QueryError", message: /sliced/ });
});

test("paginate SQL shape", async () => {
  await seed();
  const q = perAuthor().orderBy(Post.authorId);
  const page = await q.paginate({ first: 1 });
  const [sql] = await sqlOf(() => q.paginate({ first: 1, after: page.nextCursor }));
  assert.ok(sql!.includes("group by") && !sql!.includes("having"), sql);
  assert.ok(sql!.split("order by")[1]!.includes("author_id") && sql!.includes("limit") && sql!.includes("where"), sql);
});

test("batches and iterate read every row", async () => {
  await seed();
  const q = Post.objects.select({ id: Post.id, t: Post.title }).orderBy(Post.title);
  const batches: string[][] = [];
  for await (const b of q.batches(2)) batches.push(b.map((r) => r.t));
  assert.deepEqual(batches, [["a1", "a2"], ["b1"]]);
  const all: string[] = [];
  for await (const r of q.iterate(2)) all.push(r.t);
  assert.deepEqual(all, ["a1", "a2", "b1"]);
  await assert.rejects(async () => { for await (const _ of q.batches(0)) void _; }, RangeError);
  const counts: number[] = [];
  for await (const r of perAuthor().orderBy(Post.authorId).iterate(1)) counts.push(Number(r.n));
  assert.deepEqual(counts.sort(), [1, 2]);
});

test("a select has no lock", () => {
  assert.equal("lock" in Post.objects.select({ id: Post.id }), false);
});

test("a bound join count and exists apply the one-row-per-object rule", async () => {
  const { alice, bob } = await seed();
  const posts = Post.objects.select({ authorId: Post.authorId, title: Post.title }).cte("posts_of");
  const bound = User.objects.of([alice, bob]).join(posts, posts.c.authorId.eq(User.id));
  await assert.rejects(() => bound.count(), { name: "QueryError", message: /join\(posts_of\) returned User .* more than once/ });
  await assert.rejects(() => bound.exists(), QueryError);
  const totals = perAuthor().cte("totals");
  const once = User.objects.of([alice, bob]).join(totals, totals.c.authorId.eq(User.id));
  assert.equal(await once.count(), 2);
  assert.equal(await once.exists(), true);
});

test("sqlite select reads", async () => {
  const registry = new Registry();
  const m = loads(SQLITE_SOURCE, { registry }) as Dynamic;
  const { Author, Book } = m;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const [a, b] = await Author.objects.using(db).insertMany([{ email: "a@example.com", name: "Alice" }, { email: "b@example.com", name: "Bob" }]).returning();
    await Book.objects.using(db).insertMany([
      { authorId: a.id, title: "one" }, { authorId: a.id, title: "two" }, { authorId: b.id, title: "three" },
    ]);
    const grouped = Book.objects.using(db).select({ authorId: Book.authorId, n: func.count() }).groupBy(Book.authorId);
    assert.equal(await grouped.count(), 2);
    assert.equal(await grouped.having(func.count().gt(1)).exists(), true);
    assert.equal(await grouped.having(func.count().gt(5)).exists(), false);
    const ordered = grouped.orderBy(Book.authorId);
    assert.deepEqual((await ordered.paginate({ first: 1 })).items.map((r: Dynamic) => Number(r.n)), [2]);
    assert.equal(Number((await ordered.last()).n), 1);
    const q = Book.objects.using(db).filter(Book.id.gt(param("min"))).select({ t: Book.title }).orderBy(Book.id).prepare();
    assert.deepEqual(await q.scalars({ min: 1 }), ["two", "three"]);
    assert.equal(await q.count({ min: 0 }), 3);
    const titles: string[] = [];
    for await (const r of Book.objects.using(db).select({ id: Book.id, t: Book.title }).orderBy(Book.id).iterate(2)) titles.push(r.t);
    assert.deepEqual(titles, ["one", "two", "three"]);
    const books = Book.objects.using(db).select({ id: Book.id, authorId: Book.authorId, title: Book.title }).cte("books_of");
    const bound = Author.objects.using(db).of([a, b]).join(books, books.c.authorId.eq(Author.id));
    await assert.rejects(() => bound.count(), /more than once/);
  } finally {
    await db.close();
  }
});
