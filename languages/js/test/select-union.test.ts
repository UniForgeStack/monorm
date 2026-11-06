/** `select(...).union(other)` and `.unionAll(other)`: one statement, one `CompoundSelect`. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { CompoundSelect, DoesNotExist, MultipleObjectsReturned, QueryError, Registry, connect, func, getDatabase, loads, outer, param } from "../src/index.js";
import { Comment, Post, Profile, Tag, User } from "./blog/models.js";
import { otherDatabase, useDatabase } from "./helpers.js";

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
  const posts = (await Post.objects.insertMany([
    { author: alice, title: "a1", body: "", views: 80 },
    { author: alice, title: "a2", body: "", views: 40 },
    { author: bob, title: "b1", body: "", views: 5 },
  ]).returning()) as Post[];
  return { users: [alice, bob, carol] as const, posts };
}

const names = (low: bigint, high: bigint) =>
  User.objects.filter(User.id.gte(low), User.id.lte(high)).select({ id: User.id, label: User.name });
const titles = () => Post.objects.select({ id: Post.id, label: Post.title });
const pairs = (rows: readonly { id: bigint; label: string }[]) => rows.map((r) => [Number(r.id), r.label]);

test("union deduplicates", async () => {
  await seed();
  const rows = await names(1n, 2n).union(names(2n, 3n)).orderBy("id");
  assert.deepEqual(pairs(rows), [[1, "Alice"], [2, "Bob"], [3, "Carol"]]);
});

test("union all keeps duplicates", async () => {
  await seed();
  const rows = await names(1n, 2n).unionAll(names(2n, 3n)).orderBy("id");
  assert.deepEqual(pairs(rows), [[1, "Alice"], [2, "Bob"], [2, "Bob"], [3, "Carol"]]);
  assert.equal(await names(1n, 2n).unionAll(names(1n, 2n)).count(), 4);
});

test("null and empty branches", async () => {
  const { users, posts } = await seed();
  await Comment.objects.insertMany([
    { post: posts[0]!, body: "x", authorId: null }, { post: posts[0]!, body: "y", authorId: null }, { post: posts[0]!, body: "z", author: users[0] },
  ]);
  const authors = () => Comment.objects.select({ authorId: Comment.authorId });
  assert.equal((await authors().union(authors())).length, 2);
  assert.equal((await authors().unionAll(authors())).length, 6);
  const none = () => Comment.objects.filter(Comment.body.eq("nope")).select({ authorId: Comment.authorId });
  assert.equal((await none().unionAll(authors())).length, 3);
  assert.equal((await authors().unionAll(none())).length, 3);
  assert.deepEqual(await none().union(none()), []);
  assert.equal(await none().union(none()).count(), 0);
  assert.equal(await none().union(none()).exists(), false);
});

test("scalar cross model", async () => {
  await seed();
  const q = names(1n, 1n).unionAll(Post.objects.filter(Post.title.eq("b1")).select({ id: Post.id, label: Post.title }));
  assert.deepEqual(pairs(await q.orderBy("label")), [[1, "Alice"], [3, "b1"]]);
  assert.ok(names(1n, 1n).union(titles()) instanceof CompoundSelect);
});

test("shape validation", async () => {
  await seed();
  const bad = (other: unknown) => assert.throws(() => (names(1n, 2n) as Dynamic).union(other), QueryError);
  bad(User.objects.select({ id: User.id }));
  bad(User.objects.select({ id: User.id, other: User.name }));
  bad(User.objects.select({ label: User.name, id: User.id }));
  assert.throws(() => (names(1n, 2n) as Dynamic).union([1, 2]), TypeError);
  await assert.rejects(async () => await (User.objects.select({ v: User.name }) as Dynamic).union(User.objects.select({ v: User.id })), QueryError);
});

test("nullable result", async () => {
  const { users, posts } = await seed();
  await Comment.objects.insertMany([{ post: posts[0]!, body: "x", authorId: null }, { post: posts[0]!, body: "y", author: users[0] }]);
  const rows = await Comment.objects.select({ id: Comment.authorId }).unionAll(User.objects.select({ id: User.id })).orderBy("id");
  const ids = rows.map((r) => (r.id === null ? null : Number(r.id)));
  assert.deepEqual(ids.filter((i) => i !== null).sort(), [1, 1, 2, 3]);
  assert.equal(ids.filter((i) => i === null).length, 1);
});

test("enum and storage mismatch", async () => {
  await seed();
  await assert.rejects(async () => await (Profile.objects.select({ v: Profile.role }) as Dynamic).union(User.objects.select({ v: User.name })), QueryError);
  await assert.rejects(async () => await (Tag.objects.select({ v: Tag.priority }) as Dynamic).union(Post.objects.select({ v: Post.views })), QueryError);
});

test("branch slice and final slice", async () => {
  await seed();
  const left = names(1n, 3n).orderBy(User.id.desc()).slice(0, 2);
  const right = titles().orderBy(Post.id).slice(0, 1);
  assert.deepEqual(pairs(await left.unionAll(right).orderBy("id")), [[1, "a1"], [2, "Bob"], [3, "Carol"]]);
  assert.deepEqual((await left.unionAll(right).orderBy("id").slice(1, 3)).map((r) => Number(r.id)), [2, 3]);
  assert.deepEqual((await left.unionAll(right).orderBy("id").offset(2)).map((r) => Number(r.id)), [3]);
  assert.deepEqual(await left.unionAll(right).orderBy("id").limit(0), []);
  assert.equal(await left.unionAll(right).count(), 3);
  assert.throws(() => left.unionAll(right).slice(-1), RangeError);
  assert.throws(() => left.unionAll(right).limit(1).limit(2), QueryError);
});

test("nested compound grouping", async () => {
  await seed();
  const [a, b, c] = [names(1n, 2n), names(2n, 3n), names(3n, 3n)] as const;
  const ids = async (q: PromiseLike<readonly { id: bigint }[]>) => (await q).map((r) => Number(r.id)).sort();
  assert.deepEqual(await ids(a.unionAll(b).union(c)), [1, 2, 3]);
  assert.deepEqual(await ids(a.unionAll(b.union(c))), [1, 2, 2, 3]);
  assert.deepEqual(await ids(a.union(b).unionAll(c)), [1, 2, 3, 3]);
});

test("final order alias", async () => {
  await seed();
  const q = names(1n, 2n).union(Post.objects.filter(Post.title.eq("b1")).select({ id: Post.id, label: Post.title }));
  assert.deepEqual((await q.orderBy("-id", "label")).map((r) => Number(r.id)), [3, 2, 1]);
  assert.deepEqual((await q.orderBy("id")).map((r) => r.label), ["Alice", "Bob", "b1"]);
  for (const bad of ["nope", 'x"; DROP TABLE users; --']) {
    await assert.rejects(async () => await (q as Dynamic).orderBy(bad), QueryError);
  }
  assert.throws(() => (q as Dynamic).orderBy(""), TypeError);
  assert.throws(() => (q as Dynamic).orderBy(User.id), TypeError);
});

test("branch defaults and scope", async () => {
  await seed();
  const left = User.objects.filter(User.name.ne("Bob")).select({ id: User.id, label: User.name });
  const right = User.objects.exclude(User.name.ne("Bob")).select({ id: User.id, label: User.name });
  assert.deepEqual((await left.unionAll(right)).map((r) => r.label).sort(), ["Alice", "Bob", "Carol"]);
  assert.equal(await left.unionAll(right).count(), 3);
});

test("unsupported model shapes", async () => {
  await seed();
  assert.equal("union" in (User.objects as Dynamic), false);
  assert.throws(() => (names(1n, 2n) as Dynamic).union(User.objects.filter(User.id.eq(1n))), TypeError);
  assert.throws(() => (names(1n, 2n) as Dynamic).union(User.objects.select({ id: User, label: User.name })), QueryError);
  assert.throws(() => (User.objects.lock().select({ id: User.id, label: User.name }) as Dynamic).union(names(1n, 2n)), QueryError);
});

test("parameters across branches", async () => {
  await seed();
  const left = User.objects.filter(User.name.eq("Alice")).select({ id: User.id, label: User.name });
  const right = User.objects.filter(User.name.eq("Carol")).select({ id: User.id, label: User.name });
  const mid = Post.objects.filter(Post.title.eq("b1")).select({ id: Post.id, label: Post.title });
  const rows = await left.unionAll(mid).unionAll(right).orderBy("id").slice(1, 3);
  assert.deepEqual(rows.map((r) => [Number(r.id), r.label]), [[3, "b1"], [3, "Carol"]]);
  const paged = await left.unionAll(mid).unionAll(right).orderBy("id", "label").limit(3).offset(1);
  assert.deepEqual(paged.map((r) => [Number(r.id), r.label]), [[3, "b1"], [3, "Carol"]]);
  assert.deepEqual((await left.union(right)).map((r) => r.label).sort(), ["Alice", "Carol"]);
});

test("database mismatch", async () => {
  await seed();
  const db = getDatabase();
  const other = await otherDatabase();
  try {
    assert.throws(() => names(1n, 2n).using(other).union(names(2n, 3n).using(db)), /same database/);
    assert.equal((await names(1n, 2n).using(other).union(names(2n, 3n).using(other))).length, 3);
    await assert.rejects(async () => await names(1n, 2n).using(other).union(names(2n, 3n)), /same database/);
  } finally {
    await other.close();
  }
});

test("transaction read", async () => {
  await seed();
  await getDatabase().transaction(async () => {
    await User.objects.insert({ email: "t@example.com", name: "Tess" });
    const rows = await names(1n, 10n).union(titles().limit(1)).orderBy("-id");
    assert.equal(rows[0]!.label, "Tess");
  });
  assert.equal(await names(1n, 10n).union(titles()).exists(), true);
});

test("lazy cache and all", async () => {
  await seed();
  const seen: string[] = [];
  const off = getDatabase().onQuery((e) => { seen.push(e.sql); });
  try {
    const q = names(1n, 10n).union(titles()).orderBy("id");
    const q2 = q.limit(1);
    assert.notEqual(q, q2);
    assert.equal(seen.length, 0);
    const first = await q;
    assert.deepEqual(await q, first);
    assert.equal(seen.length, 1);
    await User.objects.insert({ email: "n@example.com", name: "Nina" });
    seen.length = 0;
    assert.equal((await q.all()).length, first.length + 1);
    assert.equal((await q).length, first.length);
  } finally {
    off();
  }
});

test("terminals after slice", async () => {
  await seed();
  const none = () => names(9n, 9n).union(names(9n, 9n));
  const q = names(1n, 3n).union(Post.objects.filter(Post.title.eq("b1")).select({ id: Post.id, label: Post.title })).orderBy("id");
  assert.equal(await q.slice(0, 2).count(), 2);
  assert.equal(await q.slice(0, 2).exists(), true);
  assert.equal(await q.slice(10).exists(), false);
  assert.equal((await q.first())?.label, "Alice");
  assert.equal(Number((await q.require().first())?.id), 1);
  await assert.rejects(async () => await q.get(), MultipleObjectsReturned);
  assert.equal((await names(1n, 1n).union(names(1n, 1n)).get())?.label, "Alice");
  assert.equal(await none().get(), null);
  await assert.rejects(async () => await none().require().first(), DoesNotExist);
  await assert.rejects(async () => await none().require(), DoesNotExist);
  assert.throws(() => q.slice(0, 2).first(), QueryError);
  const ids = () => User.objects.select({ id: User.id });
  assert.deepEqual((await ids().union(ids()).scalars()).map(Number).sort(), [1, 2, 3]);
  assert.equal(Number(await ids().union(ids()).orderBy("id").scalar()), 1);
  await assert.rejects(async () => await (q as Dynamic).scalar(), QueryError);
});

test("unsupported chaining", async () => {
  await seed();
  const q = names(1n, 2n).union(names(2n, 3n));
  for (const name of ["filter", "exclude", "select", "groupBy", "having", "join", "cte", "lock", "delete", "update", "prepare", "paginate", "batches", "distinct"]) {
    assert.equal(name in (q as Dynamic), false, name);
  }
  assert.throws(() => names(1n, 2n).require().union(names(2n, 3n)), QueryError);
  assert.throws(() => names(1n, 2n).union(names(2n, 3n).require()), QueryError);
  const totals = Post.objects.select({ authorId: Post.authorId, n: func.count() }).groupBy(Post.authorId).cte("totals");
  const joined = User.objects.join(totals, totals.c.authorId.eq(User.id)).select({ id: User.id, n: totals.c.n });
  assert.throws(() => (joined as Dynamic).union(User.objects.select({ id: User.id, n: func.count() }).groupBy(User.id)), /CTE/);
});

test("inspection and explain", async () => {
  await seed();
  const q = names(1n, 2n).unionAll(names(2n, 3n)).orderBy("-id").limit(2);
  const sql = q.sql().toUpperCase();
  assert.equal(sql.split("UNION").length - 1, 1);
  assert.ok(sql.includes("ALL"));
  const inspection = q.inspect();
  assert.equal(inspection.statementCount, 1);
  assert.equal(inspection.statements.length, 1);
  const plan = (await q.explain()).toLowerCase();
  assert.ok(plan.includes("union") || plan.includes("append"));
  assert.deepEqual((await q).map((r) => Number(r.id)), [3, 2]);
});

test("recursive cte regression", async () => {
  await seed();
  const totals = Post.objects.select({ authorId: Post.authorId, n: func.count() }).groupBy(Post.authorId).cte("totals");
  const rows = await User.objects.join(totals, totals.c.authorId.eq(User.id)).select({ name: User.name, n: totals.c.n }).orderBy(User.name);
  assert.deepEqual(rows.map((r) => [r.name, Number(r.n)]), [["Alice", 2], ["Bob", 1]]);
  assert.deepEqual((await User.objects.select({ id: User.id }).orderBy(User.id)).map((r) => Number(r.id)), [1, 2, 3]);
});

test("or is still filter combination", async () => {
  await seed();
  const high = Post.objects.filter(Post.views.gt(50));
  const low = Post.objects.filter(Post.views.lt(10));
  assert.deepEqual((await high.or(low)).map((p) => p.title).sort(), ["a1", "b1"]);
  const rows = await Post.objects.filter(Post.views.gt(50).or(Post.views.lt(10))).select({ title: Post.title });
  assert.deepEqual(rows.map((r) => r.title).sort(), ["a1", "b1"]);
});

test("string and text columns union", async () => {
  const { users: [alice], posts } = await seed();
  await Post.objects.filter(Post.title.eq("a1")).update({ body: "first body" });
  await Comment.objects.insert({ post: posts[0]!, body: "first body", author: alice });
  // Post.title is `varchar(200)` and Comment.body is `text`: one family.
  const q = Post.objects.select({ text: Post.title }).union(Comment.objects.select({ text: Comment.body }));
  assert.deepEqual((await q).map((r) => r.text).sort(), ["a1", "a2", "b1", "first body"]);
  assert.equal(await q.count(), 4);
});

test("unbound param and outer", async () => {
  await seed();
  const bound = Post.objects.filter(Post.id.eq(param("n"))).select({ id: Post.id, label: Post.title });
  await assert.rejects(async () => await (names(1n, 2n) as Dynamic).union(bound), /placeholder/);
  const inner = Post.objects.filter(Post.id.eq(outer(User.id))).select({ id: Post.id, label: Post.title });
  await assert.rejects(async () => await (names(1n, 2n) as Dynamic).union(inner), /enclosing|outer/);
});

test("sqlite select union", async () => {
  const registry = new Registry();
  const { Author, Book } = loads(SQLITE_SOURCE, { registry }) as Dynamic;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const [a, b] = await Author.objects.using(db).insertMany([{ email: "a@example.com", name: "Alice" }, { email: "b@example.com", name: "Bob" }]).returning();
    await Book.objects.using(db).insertMany([{ authorId: a.id, title: "one" }, { authorId: a.id, title: "Alice" }, { authorId: b.id, title: "three" }]);
    const people = Author.objects.using(db).select({ label: Author.name });
    const books = Book.objects.using(db).select({ label: Book.title });
    const labels = (rows: { label: string }[]) => rows.map((r) => r.label).sort();
    assert.deepEqual(labels(await people.union(books)), ["Alice", "Bob", "one", "three"]);
    assert.deepEqual(labels(await people.unionAll(books)), ["Alice", "Alice", "Bob", "one", "three"]);
    const left = people.orderBy(Author.name).limit(1);
    const right = books.orderBy(Book.title.desc()).limit(1);
    assert.deepEqual(labels(await left.unionAll(right)), ["Alice", "three"]);
    assert.deepEqual((await left.unionAll(right).orderBy("label").slice(1, 2)).map((r: { label: string }) => r.label), ["three"]);
    assert.equal(await left.unionAll(right).unionAll(people).count(), 4);
    assert.equal(await left.unionAll(right).orderBy("-label").limit(1).exists(), true);
    // A mismatch is an error, not a wrong result; a type mismatch names the branches.
    await assert.rejects(async () => await (people as Dynamic).union(Book.objects.using(db).select({ other: Book.title })), /same columns/);
    await assert.rejects(async () => await (people as Dynamic).union(Book.objects.using(db).select({ label: Book.id })), /branch 0 and .* in branch 1/);
    await assert.rejects(async () => await people.union(books).orderBy("missing" as never), QueryError);
    await db.transaction(async () => {
      await Author.objects.using(db).insert({ email: "t@example.com", name: "Tess" });
      assert.ok(labels(await people.union(books)).includes("Tess"));
    });
    const q = people.union(books).orderBy("label");
    const first = await q;
    assert.deepEqual(await q, first);
    assert.equal((await q.all()).length, first.length);
  } finally {
    await db.close();
  }
});
