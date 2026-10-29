/** `load()` takes a labeled expression of the row; the label becomes an attribute of each instance. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { QueryError, Registry, connect, func, loads } from "../src/index.js";
import { Comment, Post, User } from "./blog/models.js";
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

const nComments = () => func.count(Post.comments).label("n");

async function seed() {
  const [alice] = (await User.objects.insertMany([{ email: "a@example.com", name: "Alice" }]).returning()) as [User];
  const [p1] = (await Post.objects.insertMany([
    { author: alice, title: "p1", body: "", views: 10 },
    { author: alice, title: "p2", body: "", views: 20 },
  ]).returning()) as [Post, Post];
  await Comment.objects.insertMany([
    { post: p1, body: "x" },
    { post: p1, body: "y" },
  ]);
  return { alice, p1 };
}

test("query set load sets the label", async () => {
  await seed();
  const posts = await Post.objects.load(nComments()).orderBy(Post.title).all();
  assert.deepEqual(posts.map((p) => [p.title, Number(p.n)]), [["p1", 2], ["p2", 0]]);
});

test("a labeled load keeps the other columns and relations", async () => {
  await seed();
  const posts = await Post.objects.load(Post.title, Post.author, nComments()).orderBy(Post.title).all();
  assert.deepEqual(posts.map((p) => [p.title, Number(p.n), p.author.name]), [["p1", 2, "Alice"], ["p2", 0, "Alice"]]);
});

test("a relation query set nests the aggregate", async () => {
  await seed();
  const [alice] = await User.objects.load(User.posts.objects.load(nComments())).all();
  assert.deepEqual(alice!.posts.cached.map((p) => [p.title, Number(p.n)]).sort(), [["p1", 2], ["p2", 0]]);
});

test("a labeled relation query set nests the aggregate", async () => {
  await seed();
  const [alice] = (await User.objects.load(User.posts.objects.load(nComments()).label("counted")).all()) as Dynamic[];
  assert.deepEqual(alice.counted.map((p: Dynamic) => [p.title, Number(p.n)]).sort(), [["p1", 2], ["p2", 0]]);
});

test("instance load reads only the aggregate and reads a loaded label again", async () => {
  const { p1 } = await seed();
  const post = (await Post.objects.load(Post.title).get(Post.id.eq(p1.id)))!;
  const loaded = (await post.load(nComments())) as Dynamic;
  assert.equal(Number(loaded.n), 2);
  assert.equal(post.title, "p1");
  await Comment.objects.insert({ post: p1, body: "z" });
  await post.load(nComments());
  assert.equal(Number((post as Dynamic).n), 3);
});

test("instance load of a label does not read other columns", async () => {
  const { p1 } = await seed();
  const post = (await Post.objects.load(Post.title).get(Post.id.eq(p1.id)))!;
  await post.load(nComments());
  assert.equal(Object.hasOwn(post, "body"), false);
});

test("a cached relation set is not used when it has a label", async () => {
  await seed();
  const [alice] = await User.objects.load(User.posts).all();
  const posts = (await alice!.posts.load(nComments()).all()) as Dynamic[];
  assert.deepEqual(posts.map((p) => Number(p.n)).sort(), [0, 2]);
});

test("an aggregate over a filtered relation", async () => {
  await seed();
  const posts = await Post.objects.load(func.count(Post.comments, { filter: Post.comments.body.eq("x") }).label("n")).orderBy(Post.title).all();
  assert.deepEqual(posts.map((p) => Number(p.n)), [1, 0]);
});

test("load takes any labeled expression of the row", async () => {
  await seed();
  const posts = await Post.objects.load(Post.views.mul(2).label("doubleViews")).orderBy(Post.title).all();
  assert.deepEqual(posts.map((p) => [p.title, Number(p.doubleViews)]), [["p1", 20], ["p2", 40]]);
});

test("a window expression keeps one row per instance", async () => {
  await seed();
  const posts = await Post.objects.load(func.count().over({ partitionBy: Post.authorId }).label("authorPosts")).orderBy(Post.title).all();
  assert.deepEqual(posts.map((p) => Number(p.authorPosts)), [2, 2]);
});

test("load rejects an aggregate that merges rows", async () => {
  await seed();
  const merging = () => Post.objects.load(func.count().label("x") as Dynamic);
  assert.throws(() => merging().sql(), { name: "QueryError", message: /merges rows/ });
  await assert.rejects(() => merging().all(), QueryError);
});

test("a bound load takes a labeled expression", async () => {
  const { alice } = await seed();
  const posts = await Post.objects.filter(Post.authorId.eq(alice.id)).all();
  const got = await Post.objects.of(posts).load(Post.views.mul(2).label("doubleViews"));
  assert.deepEqual(got.map((p) => [p.title, Number(p.doubleViews)]).sort(), [["p1", 20], ["p2", 40]]);
  assert.throws(() => Post.objects.of(posts).load(Post.views.mul(2).label("title")), { name: "QueryError", message: /is a field, relation or member/ });
});

const totals = () => Post.objects.select({ authorId: Post.authorId, views: func.sum(Post.views) }).groupBy(Post.authorId).cte("totals");

async function seedViews() {
  const [a, b, c] = (await User.objects.insertMany([
    { email: "a@example.com", name: "Alice" }, { email: "b@example.com", name: "Bob" }, { email: "c@example.com", name: "Carol" },
  ]).returning()) as [User, User, User];
  await Post.objects.insertMany([
    { author: a, title: "a1", body: "", views: 100 }, { author: a, title: "a2", body: "", views: 50 },
    { author: b, title: "b1", body: "", views: 10 },
  ]);
  return { a, b, c };
}

test("join filters and annotates a bound set", async () => {
  const { a, b, c } = await seedViews();
  const t = totals();
  const users = await User.objects.of([a, b, c]).join(t, t.c.authorId.eq(User.id)).filter(t.c.views.gt(100)).load(t.c.views.label("totalViews"));
  assert.deepEqual(users.map((u) => [u.name, Number(u.totalViews)]), [["Alice", 150]]);
});

test("join annotates a query set", async () => {
  await seedViews();
  const t = totals();
  const users = await User.objects.join(t, t.c.authorId.eq(User.id)).load(t.c.views.label("totalViews")).orderBy(User.name).all();
  assert.deepEqual(users.map((u) => [u.name, Number(u.totalViews)]), [["Alice", 150], ["Bob", 10]]);
});

test("a join that matches an object twice raises a QueryError that names it", async () => {
  const { a, b } = await seedViews();
  const posts = Post.objects.select({ authorId: Post.authorId, title: Post.title }).cte("posts_of");
  await assert.rejects(
    async () => await User.objects.of([a, b]).join(posts, posts.c.authorId.eq(User.id)),
    { name: "QueryError", message: /join\(posts_of\) returned User .* more than once/ },
  );
});

test("an outer join gives null for an object without a match", async () => {
  const { a, c } = await seedViews();
  const t = totals();
  const users = await User.objects.of([a, c]).join(t, t.c.authorId.eq(User.id), { outer: true }).load(t.c.views.label("totalViews"));
  const byName = Object.fromEntries(users.map((u) => [u.name, u.totalViews === null ? null : Number(u.totalViews)]));
  assert.deepEqual(byName, { Alice: 150, Carol: null });
});

test("load rejects an expression without a label", () => {
  assert.throws(() => Post.objects.load(func.count(Post.comments) as Dynamic), { name: "TypeError", message: /needs a label/ });
});

test("label() rejects a name that is not an identifier", () => {
  assert.throws(() => func.count(Post.comments).label("_n"), TypeError);
  assert.throws(() => func.count(Post.comments).label("a b"), TypeError);
});

test("a label must not name a field, relation or member", () => {
  for (const name of ["title", "author", "pk"]) {
    assert.throws(() => Post.objects.load(func.count(Post.comments).label(name)), { name: "QueryError", message: /is a field, relation or member/ });
  }
});

test("a label must be unique among loads", () => {
  assert.throws(() => Post.objects.load(func.count(Post.comments).label("n"), func.count(Post.comments).label("n")), /twice/);
  assert.throws(() => Post.objects.load(nComments(), Post.comments.objects.label("n")), QueryError);
  assert.throws(() => Post.objects.load(Post.comments.objects.label("n")).load(nComments()), /twice/);
});

test("a relation query set checks its own labels", () => {
  assert.throws(() => User.objects.load(User.posts.objects.load(func.count(Post.comments).label("title"))), /is a field, relation or member/);
});

test("select ignores labeled loads", async () => {
  await seed();
  const rows = await Post.objects.load(nComments()).select({ t: Post.title }).orderBy(Post.title).all();
  assert.deepEqual(rows.map((r) => r.t), ["p1", "p2"]);
});

test("sqlite load sets the label", async () => {
  const registry = new Registry();
  const m = loads(SQLITE_SOURCE, { registry }) as Dynamic;
  const { Author, Book } = m;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const [a] = await Author.objects.using(db).insertMany([{ email: "a@example.com", name: "Alice" }, { email: "b@example.com", name: "Bob" }]).returning();
    await Book.objects.using(db).insertMany([{ authorId: a.id, title: "one" }, { authorId: a.id, title: "two" }]);
    const authors = await Author.objects.using(db).load(func.count(Author.books).label("n")).orderBy(Author.name).all();
    assert.deepEqual(authors.map((x: Dynamic) => [x.name, Number(x.n)]), [["Alice", 2], ["Bob", 0]]);
    const [alice] = await Author.objects.using(db).filter(Author.id.eq(a.id)).all();
    await alice.load(func.count(Author.books).label("m"));
    assert.equal(Number(alice.m), 2);
    const nested = await Author.objects.using(db).load(Author.books.objects.load(func.count(Book.author).label("k"))).all();
    assert.deepEqual(nested.flatMap((x: Dynamic) => x.books.cached.map((b: Dynamic) => Number(b.k))).sort(), [1, 1]);
  } finally {
    await db.close();
  }
});
