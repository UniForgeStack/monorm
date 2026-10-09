import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, exists, func, getDatabase, outer, param, type QueryEvent, type QueryInspection } from "../src/index.js";
import { Comment, Post, Tag, User } from "./blog/models.js";
import { Author, Book, sqliteRegistry } from "./sqlite/models.js";
import { DATABASE_URL, useDatabase } from "./helpers.js";

useDatabase();

const subqueries = (result: QueryInspection) => {
  const { correlatedCount, uncorrelatedCount } = result.statements[0]!;
  return { correlatedCount, uncorrelatedCount };
};

test("inspection is typed, repeatable and deeply immutable", () => {
  const query = Post.objects.load(Post.author);
  const result: QueryInspection = query.inspect();
  assert.equal(result.statementCount, 1);
  assert.equal(result.hasBatching, false);
  assert.equal(result.statements[0]!.joinsCount, 1);
  assert.deepEqual(subqueries(result), { correlatedCount: 0, uncorrelatedCount: 0 });
  assert.deepEqual(query.inspect(), result);
  assert.ok(Object.isFrozen(result) && Object.isFrozen(result.statements));
  assert.ok(Object.isFrozen(result.statements[0]));
  assert.throws(() => Object.assign(result, { statementCount: 2 }), TypeError);
});

test("implicit and explicit nested subqueries have their own enclosing dependencies", () => {
  assert.deepEqual(subqueries(User.objects.filter(User.posts.comments.body.eq("hi")).inspect()), { correlatedCount: 2, uncorrelatedCount: 0 });
  const comments = exists(Comment.objects.filter(Comment.postId.eq(outer(Post.id))));
  assert.deepEqual(subqueries(User.objects.filter(exists(Post.objects.filter(comments))).inspect()), { correlatedCount: 1, uncorrelatedCount: 1 });
  const average = Post.objects.filter(Post.authorId.eq(outer(Post.authorId))).select({ value: func.avg(Post.views) }).asScalar();
  assert.deepEqual(subqueries(Post.objects.filter(Post.views.gt(average)).inspect()), { correlatedCount: 1, uncorrelatedCount: 0 });
});

test("CTE and FROM dependencies are independent of placement", () => {
  const recent = Post.objects.filter(Post.views.gt(5)).cte("recent");
  assert.deepEqual(subqueries(Post.objects.from(recent).inspect()), { correlatedCount: 0, uncorrelatedCount: 0 });
  const totals = Post.objects.select({ authorId: Post.authorId, n: func.count() }).groupBy(Post.authorId).cte("totals");
  assert.deepEqual(subqueries(User.objects.filter(User.id.in(totals.select({ authorId: totals.c.authorId }))).inspect()), { correlatedCount: 0, uncorrelatedCount: 1 });
});

test("shared aggregates use one correlated LATERAL on PostgreSQL and scalar subqueries on SQLite", () => {
  const pg = User.objects.select({ n: func.count(User.posts), max: func.max(User.posts.views) }).inspect();
  assert.equal(pg.statements[0]!.joinsCount, 1);
  assert.deepEqual(subqueries(pg), { correlatedCount: 1, uncorrelatedCount: 0 });
  // A different registry inspects offline even with a PostgreSQL default pool.
  const sqlite = Author.objects.select({ n: func.count(Author.books), max: func.max(Author.books.pages) }).inspect();
  assert.equal(sqlite.statements[0]!.joinsCount, 0);
  assert.deepEqual(subqueries(sqlite), { correlatedCount: 2, uncorrelatedCount: 0 });
});

test("prepared inspection validates and binds parameters", () => {
  const query = Post.objects.filter(Post.views.gt(param("minimum"))).prepare();
  assert.deepEqual(query.inspect({ minimum: 5 }), Post.objects.filter(Post.views.gt(5)).inspect());
  assert.throws(() => Reflect.apply(query.inspect, query, []), TypeError);
});

test("empty parents skip planned prefetches; inspection sends no statements", async () => {
  const db = getDatabase();
  const query = User.objects.load(User.posts.comments);
  const events: QueryEvent[] = [];
  const remove = db.onQuery(e => events.push(e));
  try {
    const result = query.inspect();
    assert.equal(result.statementCount, 3);
    assert.equal(result.hasBatching, true);
    assert.deepEqual(result.statements.map(s => s.hasBatching), [false, true, true]);
    assert.deepEqual(events, []);
    assert.deepEqual(await query, []);
  } finally { remove(); }
  assert.equal(events.length, 1);
});

test("prefetch template counts differ from actual key batches", async () => {
  await User.objects.insertMany(Array.from({ length: 5 }, (_, i) => ({ email: `u${i}@x`, name: `U${i}` })));
  const db = await connect(DATABASE_URL, { default: false, disable: ["max_params=2"] });
  try {
    const query = User.objects.using(db).load(User.posts);
    assert.equal(query.inspect().statementCount, 2);
    const events: QueryEvent[] = [];
    const remove = db.onQuery(e => events.push(e));
    try { assert.equal((await query).length, 5); } finally { remove(); }
    assert.equal(events.length, 4);
  } finally { await db.close(); }
});

test("inserts, upserts, mutations and bulk returning writes inspect without execution", async () => {
  const db = getDatabase();
  const events: QueryEvent[] = [];
  const remove = db.onQuery(e => events.push(e));
  try {
    const insert = Tag.objects.insertMany(Array.from({ length: 5 }, (_, i) => ({ name: `t${i}` })), { batchSize: 2 }).returning();
    const planned = insert.inspect();
    assert.equal(planned.statementCount, 3);
    assert.ok(planned.hasBatching && planned.statements.every(s => s.hasBatching));
    assert.deepEqual(events, []);
    const tags = await insert;
    assert.equal(events.length, 3);
    assert.equal(await insert, tags); // one execution per statement
    events.length = 0;
    const update = Tag.objects.updateMany(tags.map(t => ({ id: t.id, name: `${t.name}!` })), { batchSize: 2, returning: true });
    assert.equal(update.inspect().statementCount, 3);
    assert.equal(update.inspect().hasBatching, true);
    assert.deepEqual(events, []);
    assert.equal((await update).length, 5);
    assert.equal(events.length, 3);
    events.length = 0;
    const scalar = Comment.objects.filter(Comment.postId.eq(outer(Post.id))).select({ n: func.count() }).asScalar();
    const mutation = Post.objects.update({ views: scalar });
    assert.deepEqual(subqueries(mutation.inspect()), { correlatedCount: 1, uncorrelatedCount: 0 });
    const deletion = Post.objects.filter(Post.comments.body.eq("hi")).delete({ returning: true });
    assert.deepEqual(subqueries(deletion.inspect()), { correlatedCount: 1, uncorrelatedCount: 0 });
    const upsert = Tag.objects.insert({ name: "t0!" }).onConflict(Tag.name, { update: false }).returning();
    assert.equal(upsert.inspect().statementCount, 1);
    assert.deepEqual(events, []);
    assert.equal(await mutation, 0);
    assert.deepEqual(await deletion, []);
    assert.equal(await upsert, null);
    assert.equal(Tag.objects.insertMany([]).inspect().statementCount, 0);
    assert.equal(Tag.objects.updateMany([]).inspect().statementCount, 0);
    assert.equal(Tag.objects.update({}).inspect().statementCount, 0);
  } finally { remove(); }
});

test("default and explicit connection capabilities affect write batch inspection", async () => {
  await getDatabase().close();
  const db = await connect(DATABASE_URL, { disable: ["max_params=2"] });
  try {
    const statement = Tag.objects.insertMany(Array.from({ length: 5 }, (_, i) => ({ name: `t${i}` })));
    assert.equal(statement.inspect().statementCount, 3);
    assert.equal(statement.inspect().hasBatching, true);
    assert.equal(Tag.objects.using(db).insertMany([{ name: "a" }, { name: "b" }, { name: "c" }]).inspect().statementCount, 2);
  } finally {
    await db.close();
    await connect(DATABASE_URL);
  }
});

test("SQLite inspection and actual batched writes share the same planner", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const events: QueryEvent[] = [];
    const remove = db.onQuery(e => events.push(e));
    try {
      const insert = Author.objects.using(db).insertMany(Array.from({ length: 5 }, (_, i) => ({ email: `a${i}`, name: `A${i}` })), { batchSize: 2 }).returning();
      assert.equal(insert.inspect().statementCount, 3);
      assert.deepEqual(events, []);
      const authors = await insert;
      assert.equal(events.length, 3);
      events.length = 0;
      const update = Author.objects.using(db).updateMany(authors.map(a => ({ id: a.id, name: `${a.name}!` })), { batchSize: 2, returning: true });
      assert.equal(update.inspect().statementCount, 3);
      assert.equal((await update).length, 5);
      assert.equal(events.length, 3);
      assert.equal(Book.objects.using(db).load(Book.author).inspect().statements[0]!.joinsCount, 1);
      const query = Author.objects.using(db).load(Author.books);
      assert.equal(query.inspect().statementCount, 2);
      assert.equal((await query).length, 5);
    } finally { remove(); }
  } finally { await db.close(); }
});
