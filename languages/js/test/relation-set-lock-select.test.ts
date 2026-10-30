/** A relation query set in `load()`: `lock()` and `select({...}).label(...)`. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { LockNotAvailable, Registry, TransactionRequired, connect, func, getDatabase, loads } from "../src/index.js";
import { Post, User } from "./blog/models.js";
import { DATABASE_URL, useDatabase } from "./helpers.js";

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
  status   String @default("new")
  author   Author @relation(fields: [authorId], references: [id])
}`;

async function seed() {
  const [alice, bob, carol] = (await User.objects.insertMany([
    { email: "a@example.com", name: "Alice" }, { email: "b@example.com", name: "Bob" }, { email: "c@example.com", name: "Carol" },
  ]).returning()) as [User, User, User];
  await Post.objects.insertMany([
    { author: alice, title: "A1", body: "", views: 80 },
    { author: alice, title: "A2", body: "", views: 40 },
    { author: bob, title: "B1", body: "", views: 5 },
  ]);
  return { alice, bob, carol };
}

async function sqlOf(run: () => Promise<unknown>): Promise<string[]> {
  const seen: string[] = [];
  const off = getDatabase().onQuery((e) => { seen.push(e.sql); });
  try {
    await run();
  } finally {
    off();
  }
  return seen;
}

test("a locked relation set locks its IN query", async () => {
  await seed();
  const db = getDatabase();
  let users: User[] = [];
  const sqls = await sqlOf(() => db.transaction(async () => {
    users = await User.objects.orderBy(User.id).load(User.posts.objects.lock());
  }));
  const posts = sqls.filter((s) => s.includes('FROM "posts"') || s.includes("FROM posts"));
  assert.equal(posts.length, 1);
  assert.ok(posts[0]!.trimEnd().endsWith('FOR UPDATE OF "posts"'));
  assert.deepEqual(users.map((u) => (u.posts as Dynamic).cached.length), [2, 1, 0]);
});

test("a share lock with skipLocked", async () => {
  await seed();
  const db = getDatabase();
  const sqls = await sqlOf(() => db.transaction(async () => {
    await User.objects.load(User.posts.objects.lock({ exclusive: false, skipLocked: true }));
  }));
  assert.ok(sqls.some((s) => s.trimEnd().endsWith('FOR SHARE OF "posts" SKIP LOCKED')));
});

test("a locked relation set needs a transaction", async () => {
  const { alice } = await seed();
  await assert.rejects(async () => { await User.objects.load(User.posts.objects.lock()); }, TransactionRequired);
  await assert.rejects(async () => { await User.objects.load(User.posts.comments).load(User.posts.objects.lock()); }, TransactionRequired);
  await assert.rejects(async () => { await alice.load(User.posts.objects.lock()); }, TransactionRequired);
  await assert.rejects(async () => { await User.objects.of([alice]).load(User.posts.objects.lock()); }, TransactionRequired);
});

test("a locked relation set blocks a second lock", async () => {
  await seed();
  const db = getDatabase();
  await db.transaction(async () => {
    await User.objects.load(User.posts.objects.lock());
    const other = await connect(DATABASE_URL, { default: false });
    try {
      await assert.rejects(
        () => other.transaction(async () => Post.objects.using(other).lock({ nowait: true }).all()),
        LockNotAvailable,
      );
    } finally {
      await other.close();
    }
  });
});

test("a lock cannot follow a per-parent slice", async () => {
  await seed();
  await getDatabase().transaction(async () => {
    await assert.rejects(
      async () => { await User.objects.load(User.posts.objects.lock().limit(1)); },
      { name: "QueryError", message: /lock\(\) can't be used with a slice/ },
    );
  });
});

test("a selected relation set gives rows per parent", async () => {
  await seed();
  const users = (await User.objects.orderBy(User.id).load(
    User.posts.objects.select({ id: Post.id, title: func.lower(Post.title) }).orderBy(Post.id).label("titles"),
  )) as Dynamic[];
  assert.deepEqual(users.map((u) => u.titles.map((r: Dynamic) => r.title)), [["a1", "a2"], ["b1"], []]);
  assert.equal(typeof users[0].titles[0].id, "bigint");
});

test("a selected relation set filters, orders and slices per parent", async () => {
  await seed();
  const users = (await User.objects.orderBy(User.id).load(
    User.posts.objects.filter(Post.views.gt(10)).select({ title: Post.title }).orderBy(Post.views.desc()).limit(1).label("top"),
  )) as Dynamic[];
  assert.deepEqual(users.map((u) => u.top.map((r: Dynamic) => r.title)), [["A1"], [], []]);
  const second = (await User.objects.orderBy(User.id).load(
    User.posts.objects.select({ title: Post.title }).orderBy(Post.views.desc()).offset(1).limit(1).label("second"),
  )) as Dynamic[];
  assert.deepEqual(second.map((u) => u.second.map((r: Dynamic) => r.title)), [["A2"], [], []]);
});

test("a grouped relation select groups per parent", async () => {
  await seed();
  const users = (await User.objects.orderBy(User.id).load(
    User.posts.objects.select({ n: func.count(), views: func.sum(Post.views) }).groupBy(Post.published).label("stats"),
  )) as Dynamic[];
  assert.deepEqual(users.map((u) => u.stats.map((r: Dynamic) => [Number(r.n), Number(r.views)])), [[[2, 120]], [[1, 5]], []]);
});

test("a selected relation set loads beside another relation", async () => {
  await seed();
  const users = (await User.objects.orderBy(User.id).load(
    User.posts.objects.select({ title: Post.title }).orderBy(Post.id).label("titles"), User.profile,
  )) as Dynamic[];
  assert.deepEqual(users.map((u) => u.titles.length), [2, 1, 0]);
  assert.equal(users[0].profile, null);
});

test("a select in load() needs a label and a relation", async () => {
  assert.throws(() => User.objects.load(User.posts.objects.select({ id: Post.id }) as never), /needs \.label/);
  assert.throws(() => User.objects.load(Post.objects.select({ id: Post.id }) as never), /relation query set/);
  assert.throws(() => (Post.objects.select({ id: Post.id }) as Dynamic).label("x"), /relation query set/);
});

test("a selected relation set cannot lock or run alone", async () => {
  await seed();
  await getDatabase().transaction(async () => {
    await assert.rejects(
      async () => { await User.objects.load(User.posts.objects.lock().select({ id: Post.id }).label("ids")); },
      { name: "QueryError", message: /select\(\) can't lock rows/ },
    );
  });
  await assert.rejects(async () => { await (User.posts.objects.select({ id: Post.id }) as Dynamic); }, /inside load/);
});

test("an instance load takes a selected relation set", async () => {
  const { alice } = await seed();
  const fresh = (await User.objects.get(User.id.eq(alice.id))) as Dynamic;
  await fresh.load(User.posts.objects.select({ title: Post.title }).orderBy(Post.id).label("titles"));
  assert.deepEqual(fresh.titles.map((r: Dynamic) => r.title), ["A1", "A2"]);
});

test("sqlite selected relation set", async () => {
  const registry = new Registry();
  const { Author, Book } = loads(SQLITE_SOURCE, { registry }) as Dynamic;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const [a, b] = await Author.objects.using(db).insertMany([{ email: "a@example.com", name: "Alice" }, { email: "b@example.com", name: "Bob" }]).returning();
    await Book.objects.using(db).insertMany([
      { authorId: a.id, title: "one" }, { authorId: a.id, title: "two" }, { authorId: b.id, title: "three" },
    ]);
    const authors = await Author.objects.using(db).orderBy(Author.id).load(
      Author.books.objects.select({ title: Book.title }).orderBy(Book.id).label("titles"),
    );
    assert.deepEqual(authors.map((x: Dynamic) => x.titles.map((r: Dynamic) => r.title)), [["one", "two"], ["three"]]);
    await assert.rejects(async () => { await Author.objects.using(db).load(Author.books.objects.lock()); }, TransactionRequired);
    const grouped = await Author.objects.using(db).orderBy(Author.id).load(
      Author.books.objects.select({ n: func.count() }).groupBy(Book.status).label("stats"),
    );
    assert.deepEqual(grouped.map((x: Dynamic) => x.stats.map((r: Dynamic) => Number(r.n))), [[2], [1]]);
  } finally {
    await db.close();
  }
});

