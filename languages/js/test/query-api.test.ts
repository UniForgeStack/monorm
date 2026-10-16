/** Query-set API: query-set classes, prefetch on loaded instances, columns through to-one
 * `load()` paths, OR of query sets, single-row labels, column paths and model metadata. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { ManyRelatedSet, NotLoaded, QueryError, QuerySet, RelatedSet, Registry, connect, getDatabase, loads, column, describe, prefetch, useQuerySet, type ModelClass, type ModelSpec } from "../src/index.js";
import { Comment, Post, PostTag, Profile, Tag, User, type PostSpec, type TagSpec } from "./blog/models.js";
import { useDatabase } from "./helpers.js";

useDatabase();

class PostQueries extends QuerySet<PostSpec> {
  published(): this {
    return this.filter(Post.published.eq(true)) as this;
  }

  popular(views = 10): this {
    return this.filter(Post.views.gte(views)) as this;
  }
}

class TagQueries extends QuerySet<TagSpec> {
  named(prefix: string): this {
    return this.filter(Tag.name.startsWith(prefix)) as this;
  }
}

const posts = () => Post.objects as unknown as PostQueries;

async function seed() {
  const alice = await User.objects.insert({ email: "alice@example.com", name: "Alice" });
  const bob = await User.objects.insert({ email: "bob@example.com", name: "Bob" });
  const rows = await Post.objects.insertMany([
    { author: alice, title: "draft", body: "...", views: 50 },
    { author: alice, title: "hit", body: "...", published: true, views: 90 },
    { author: alice, title: "quiet", body: "...", published: true, views: 1 },
    { author: bob, title: "bob", body: "...", published: true, views: 70 },
  ]).returning();
  return { alice, bob, posts: rows };
}

// -- query-set classes ------------------------------------------------------------------------

test("query-set classes", async (t) => {
  useQuerySet(Post, () => PostQueries);
  useQuerySet(Tag, TagQueries);
  t.after(() => {
    useQuerySet(Post, QuerySet);
    useQuerySet(Tag, QuerySet);
  });

  await t.test("custom methods chain with builder methods", async () => {
    const { alice } = await seed();
    const qs = (posts().published().filter(Post.authorId.eq(alice.id)) as unknown as PostQueries).popular(10);
    assert.ok(qs instanceof PostQueries);
    assert.deepEqual((await qs).map((p) => p.title), ["hit"]);
    assert.deepEqual((await (posts().orderBy("views") as unknown as PostQueries).popular(60).published()).map((p) => p.title), ["bob", "hit"]);
    assert.equal(await posts().published().count(), 3);
    assert.equal(Post.objects, Post.objects);
  });

  await t.test("relation sets have the custom methods", async () => {
    const { alice, posts: rows } = await seed();
    const set = alice.posts as unknown as PostQueries;
    assert.ok(alice.posts instanceof RelatedSet && alice.posts instanceof PostQueries);
    assert.deepEqual((await (set.published().orderBy("views") as unknown as PostQueries)).map((p) => p.title), ["quiet", "hit"]);
    assert.deepEqual((await (alice.posts.filter(Post.views.gt(5)) as unknown as PostQueries).published()).map((p) => p.title), ["hit"]);
    const tags = await Tag.objects.insertMany([{ name: "python" }, { name: "rust" }]).returning();
    await rows[1]!.tags.add(...tags);
    assert.ok(rows[1]!.tags instanceof ManyRelatedSet);
    assert.deepEqual((await (rows[1]!.tags as unknown as TagQueries).named("py")).map((t) => t.name), ["python"]);
  });

  await t.test("a relation query set uses them", async () => {
    await seed();
    const users = await User.objects.orderBy("name").load((User.posts.objects as unknown as PostQueries).published() as never);
    assert.deepEqual(users.map((u) => (u.posts as unknown as { cached: { title: string }[] }).cached.map((p) => p.title)), [["hit", "quiet"], ["bob"]]);
  });
});

test("a query-set class is checked", () => {
  assert.throws(() => useQuerySet(Post, Object as never), /extends QuerySet/);
  useQuerySet(Post, (() => Map) as never);
  try {
    assert.throws(() => Post.objects, /extends QuerySet/);
  } finally {
    useQuerySet(Post, QuerySet);
  }
  assert.equal(Post.objects.constructor, QuerySet);
});

// -- prefetch on loaded instances -------------------------------------------------------------

test("prefetch onto loaded instances", async () => {
  const { posts: rows } = await seed();
  await Comment.objects.insertMany([{ post: rows[1]!, body: "a" }, { post: rows[1]!, body: "b" }]);
  const users = await User.objects.orderBy("id");
  await prefetch(users, User.posts.comments, User.posts.objects.filter(Post.published.eq(true)).slice(0, 1).label("top"));
  type Loaded = { posts: { cached: { title: string; comments: { cached: { body: string }[] } }[] }; top: { title: string }[] };
  const loaded = users as unknown as Loaded[];
  assert.deepEqual(loaded[0]!.posts.cached.map((p) => p.title), ["draft", "hit", "quiet"]);
  assert.deepEqual(loaded[0]!.posts.cached[1]!.comments.cached.map((c) => c.body), ["a", "b"]);
  assert.deepEqual(loaded.map((u) => u.top.map((p) => p.title)), [["hit"], ["bob"]]);
});

test("prefetch reads keys from the instances, to-one and many-to-many", async () => {
  const { posts: rows } = await seed();
  await Comment.objects.insertMany([{ post: rows[0]!, body: "a" }, { post: rows[3]!, body: "b" }]);
  const comments = await Comment.objects.orderBy("id");
  await Comment.objects.delete(); // the instances' own rows are not read again
  await prefetch(comments, Comment.post.author);
  const loaded = comments as unknown as { post: { title: string; author: { name: string } } }[];
  assert.deepEqual(loaded.map((c) => [c.post.title, c.post.author.name]), [["draft", "Alice"], ["bob", "Bob"]]);
  const tags = await Tag.objects.insertMany([{ name: "a" }, { name: "b" }]).returning();
  await rows[0]!.tags.add(...tags);
  const posts = await Post.objects.orderBy("id");
  await prefetch(posts, Post.tags);
  assert.deepEqual((posts[0]!.tags as unknown as { cached: { name: string }[] }).cached.map((t) => t.name), ["a", "b"]);
});

test("prefetch checks its input", async () => {
  const { alice, posts: rows } = await seed();
  await prefetch([], User.posts);
  await assert.rejects(prefetch([alice, rows[0]!], User.posts), /one model/);
  await assert.rejects(prefetch([alice], Post.author as never), /does not start at User|User has no relation/);
});

async function prefetchDatabases() {
  const registry = new Registry();
  const models = loads(`datasource db { provider = "sqlite" }
model Author {
  id Int @id @default(autoincrement())
  name String
  books Book[]
}
model Book {
  id Int @id @default(autoincrement())
  title String
  authorId Int
  author Author @relation(fields: [authorId], references: [id])
}
`, { registry });
  const Author = models["Author"] as any, Book = models["Book"] as any;
  const d1 = await connect("sqlite://:memory:", { registry, default: false });
  const d2 = await connect("sqlite://:memory:", { registry, default: false });
  const close = async () => { await d1.close(); await d2.close(); };
  try {
    for (const [db, name] of [[d1, "d1"], [d2, "d2"]] as const) {
      await db.createTables();
      const authors = await Author.objects.using(db).insertMany([{ name }, { name: "empty" }]).returning();
      await Book.objects.using(db).insert({ title: `B-${name}`, authorId: authors[0].id });
    }
    const a1 = await Author.objects.using(d1).orderBy("id").first();
    const [x, y] = await Author.objects.using(d2).orderBy("id");
    return { Author, d2, objs: [a1, y, x], close };
  } catch (error) {
    await close();
    throw error;
  }
}

test("prefetch refuses instances from two databases", async () => {
  const { Author, objs, close } = await prefetchDatabases();
  try {
    await assert.rejects(prefetch(objs, Author.books), {
      name: "QueryError", message: "prefetch() takes instances from one database; pass using= to read another",
    });
    for (const obj of objs) assert.throws(() => obj.books.cached, NotLoaded);
  } finally {
    await close();
  }
});

test("prefetch with using reads that database", async () => {
  const { Author, d2, objs, close } = await prefetchDatabases();
  try {
    await prefetch(objs, Author.books, { using: d2 });
    assert.deepEqual(objs.map((obj) => obj.books.cached.map((b: any) => b.title)), [["B-d2"], [], ["B-d2"]]);
  } finally {
    await close();
  }
});

test("prefetch accepts instances from one database", async () => {
  const { Author, objs, close } = await prefetchDatabases();
  try {
    await prefetch(objs.slice(1), Author.books);
    assert.deepEqual(objs.slice(1).map((obj) => obj.books.cached.map((b: any) => b.title)), [[], ["B-d2"]]);
    const { alice, bob } = await seed();
    await prefetch([alice, bob], User.posts);
    const titles = (obj: User) => (obj.posts as unknown as { cached: Post[] }).cached.map((p) => p.title);
    assert.deepEqual(titles(alice), ["draft", "hit", "quiet"]);
    const explicit = await User.objects.using(getDatabase()).require().get(User.id.eq(alice.id));
    await prefetch([bob, explicit], User.posts);
    assert.deepEqual(titles(explicit), ["draft", "hit", "quiet"]);
  } finally {
    await close();
  }
});

// -- model metadata for factories -------------------------------------------------------------

test("describe gives fields, relations and unique keys", () => {
  const post = describe(Post);
  assert.equal(post.name, "Post");
  assert.deepEqual(post.primaryKey, ["id"]);
  const fields = new Map(post.fields.map((f) => [f.name, f]));
  assert.deepEqual(fields.get("title"), {
    name: "title", column: "title", type: "string", nullable: false, array: false, enum: null, maxLength: 200,
    primaryKey: false, unique: false, default: null, insert: true,
  });
  assert.equal(fields.get("authorId")!.column, "author_id");
  assert.equal(fields.get("id")!.default, "database");
  const relations = new Map(post.relations.map((r) => [r.name, r]));
  assert.equal(relations.get("author")!.kind, "belongsTo");
  assert.equal(relations.get("author")!.target, User);
  assert.deepEqual(relations.get("author")!.from, ["authorId"]);
  assert.deepEqual(relations.get("author")!.to, ["id"]);
  assert.equal(relations.get("tags")!.through, PostTag);
  assert.deepEqual(describe(PostTag).unique, [["id"], ["postId", "tagId"]]);
  assert.deepEqual(describe(User).unique, [["id"], ["email"]]);
  const profile = new Map(describe(Profile).fields.map((f) => [f.name, f]));
  assert.equal(profile.get("role")!.enum, "Role");
  assert.ok(profile.get("links")!.array);
});

let made = 0;
/** A factory built only on describe() and objects.insert(). */
async function make(model: ModelClass<ModelSpec>, values: Record<string, unknown> = {}): Promise<Record<string, unknown>> {
  const info = describe(model);
  made += 1;
  for (const rel of info.relations) {
    if (rel.kind === "belongsTo" && !rel.nullable && !(rel.name in values) && !rel.from.some((f) => f in values)) {
      values[rel.name] = await make(rel.target);
    }
  }
  const linked = new Set(info.relations.filter((r) => r.name in values).flatMap((r) => r.from));
  for (const f of info.fields) {
    if (f.insert && f.default === null && !f.nullable && !(f.name in values) && !linked.has(f.name)) {
      values[f.name] = f.type === "string" || f.type === "text" ? `${f.name}-${made}` : f.type === "bool" ? false : made;
    }
  }
  return (await model.objects.insert(values as never)) as Record<string, unknown>;
}

test("a factory needs only describe and insert", async () => {
  const post = await make(Post);
  assert.match(post["title"] as string, /^title-/);
  assert.equal(post["views"], 0);
  const author = await User.objects.require().get(User.id.eq(post["authorId"] as bigint));
  assert.match(author.email, /^email-/);
  const link = await make(PostTag, { post });
  assert.equal(link["postId"], post["id"]);
});

// -- columns through to-one load() paths --------------------------------------------------------------

test("load() through a to-one path", async () => {
  const { posts: rows } = await seed();
  const qs = Comment.objects.load(Comment.body, Comment.post.title);
  assert.ok(qs.sql().includes('"title"') && !qs.sql().includes('"views"'));
  await Comment.objects.insert({ post: rows[1]!, body: "x" });
  const [c] = (await qs) as unknown as { body: string; post: { title: string; views: number } }[];
  assert.equal(c!.body, "x");
  assert.equal(c!.post.title, "hit");
  assert.throws(() => c!.post.views, NotLoaded);
  // Q2.5.4: only a column of the model itself makes it partial
  const [d] = await Comment.objects.load(Comment.post.author.name).filter(Comment.body.eq("x"));
  assert.equal(d!.post.author.name, "Alice");
  assert.equal(d!.body, "x");
  assert.throws(() => (d!.post.author as unknown as { email: string }).email, NotLoaded);
  const [e] = await Comment.objects.load(Comment.id, Comment.post.author.name).filter(Comment.body.eq("x"));
  assert.throws(() => (e as unknown as { body: string }).body, NotLoaded);
  const [f] = await Comment.objects.load(Comment.post.author.name, Comment.post.author).filter(Comment.body.eq("x"));
  assert.equal(f!.post.author.email, "alice@example.com");
});

test("load() a column through a to-many path", async () => {
  await seed();
  const users = await User.objects.load(User.posts.title).orderBy(User.id);
  assert.deepEqual(users[0]!.posts.cached.map((p) => p.title).sort(), ["draft", "hit", "quiet"]);
  assert.throws(() => (users[0]!.posts.cached[0] as unknown as { views: number }).views, NotLoaded);
  assert.equal(users[0]!.name, "Alice");
});

// -- or(), label one, column() ---------------------------------------------------------------

test("or() of query sets", async () => {
  await seed();
  const popular = Post.objects.orderBy("id").filter(Post.views.gte(80));
  const drafts = Post.objects.filter(Post.published.eq(false));
  assert.deepEqual((await popular.or(drafts)).map((p) => p.title), ["draft", "hit"]);
  assert.equal((await popular.or(Post.objects)).length, 4);
  assert.throws(() => popular.filter(Post.views.lt(100)).or(drafts), /one filter/);
  assert.throws(() => drafts.or(popular), /sets order/);
  assert.throws(() => popular.slice(0, 1).or(drafts), QueryError);
});

test("or() of a relation set ignores the loaded rows", async () => {
  await seed();
  const users = await User.objects.orderBy("id").load(User.posts.objects.filter(Post.published.eq(false)));
  const qs = users[0]!.posts.or(Post.objects.filter(Post.published.eq(true)));
  assert.ok(!(qs instanceof RelatedSet));
  assert.deepEqual((await qs).map((p) => p.title).sort(), ["bob", "draft", "hit", "quiet"]);
  assert.deepEqual((await Post.objects.filter(Post.published.eq(true)).or(users[0]!.posts)).map((p) => p.title).sort(), ["bob", "draft", "hit", "quiet"]);
});

test("or() of a many-to-many set ignores the loaded rows", async () => {
  const { posts: rows } = await seed();
  const tags = await Tag.objects.insertMany([{ name: "a" }, { name: "b" }, { name: "c" }]).returning();
  await rows[1]!.tags.add(...tags);
  const [post] = await Post.objects.filter(Post.id.eq(rows[1]!.id)).load(Post.tags.objects.filter(Tag.name.eq("a")));
  const qs = post!.tags.or(Tag.objects.filter(Tag.name.eq("c")));
  assert.ok(!(qs instanceof ManyRelatedSet));
  assert.deepEqual((await qs).map((t) => t.name).sort(), ["a", "b", "c"]);
});

test("or() of two filtered relation sets names the relation", async () => {
  const { alice } = await seed();
  assert.throws(() => alice.posts.filter(Post.views.gt(1)).or(alice.posts.filter(Post.views.lt(100))), /posts.*filter\(a\.or\(b\)\)/);
});

test("or() keeps the custom query-set class", async (t) => {
  useQuerySet(Post, () => PostQueries);
  t.after(() => useQuerySet(Post, QuerySet));
  await seed();
  const users = await User.objects.orderBy("id").load(User.posts.objects.filter(Post.published.eq(false)));
  const qs = users[0]!.posts.or(Post.objects.filter(Post.published.eq(true))) as unknown as PostQueries;
  assert.ok(qs instanceof PostQueries && !(qs instanceof RelatedSet));
  assert.deepEqual((await qs.popular(60)).map((p) => p.title).sort(), ["bob", "hit"]);
});

test("a custom fetch() override runs on a relation set", async (t) => {
  let count = 0;
  class Hooked<M extends ModelSpec> extends QuerySet<M> {
    override async fetch(): Promise<M["row"][]> {
      count++;
      return super.fetch();
    }
  }
  useQuerySet(Post, Hooked as never);
  t.after(() => useQuerySet(Post, QuerySet));
  const { alice } = await seed();
  await Post.objects.all();
  assert.equal(count, 1);
  count = 0;
  await alice.posts.all();
  assert.equal(count, 1);
  await alice.posts.filter(Post.views.gt(0)).all();
  assert.equal(count, 2);
});

test("a custom getOrInsert() override runs on a relation set", async (t) => {
  let count = 0;
  class Hooked<M extends ModelSpec> extends QuerySet<M> {
    override getOrInsert(): ReturnType<QuerySet<M>["getOrInsert"]> {
      count++;
      return Promise.resolve([undefined as never, false]);
    }
  }
  useQuerySet(Post, Hooked as never);
  t.after(() => useQuerySet(Post, QuerySet));
  const { alice } = await seed();
  await alice.posts.getOrInsert({ title: "fresh", body: "..." });
  assert.equal(count, 1);
});

test("label one stores a row or null", async () => {
  await seed();
  await User.objects.insert({ email: "carol@example.com", name: "Carol" });
  const users = await User.objects.orderBy("id").load(User.posts.objects.orderBy("-views").label("best", { one: true }));
  assert.deepEqual(users.map((u) => u.best?.title ?? null), ["hit", "bob", null]);
  assert.throws(() => User.objects.load(User.posts.objects.slice(0, 2).label("x", { one: true })), /slice/);
  await assert.rejects(async () => { await Post.objects.load(Post.author.objects.label("writer", { one: true })); }, (e: Error) => /to-many/.test(e.message) && !/to_attr/.test(e.message));
});

test("column() from a dotted path", async () => {
  await seed();
  assert.equal(String(column(Comment, "post.author.name")), String(Comment.post.author.name));
  const names = await User.objects.filter((column(User, "posts.title") as typeof User.posts.title).startsWith("hi")).select({ name: User.name }).scalars();
  assert.deepEqual(names, ["Alice"]);
  assert.throws(() => column(Post, "nope.title"), /Post has no relation "nope"/);
  assert.throws(() => column(Post, "author.nope"), /User has no field "nope"/);
});

// -- mutation regressions --------------------------------------------------------------------

for (const key of ["db", "loads", "withoutDefaults"] as const) {
  test(`or() rejects right-side ${key}`, () => {
    const left = Post.objects.filter(Post.published.eq(true));
    // Isolate loads: public load() also sets derived state with independent guards.
    class LoadOnlyQueries extends QuerySet<PostSpec> {
      withLoads(): this { return this.clone({ loads: [Post.comments] }); }
    }
    const right = key === "db" ? Post.objects.using(getDatabase())
      : key === "loads" ? new LoadOnlyQueries(Post._meta).withLoads()
      : Post.objects.withoutDefaults();
    assert.throws(() => left.or(right), { name: "QueryError", message: new RegExp(`sets ${key}`) });
  });
}

for (const side of ["left", "right"] as const) {
  test(`or() rejects an offset without a limit on the ${side}`, () => {
    const plain = Post.objects.filter(Post.published.eq(true));
    const sliced = plain.slice(1);
    assert.throws(() => side === "left" ? sliced.or(plain) : plain.or(sliced), {
      name: "QueryError", message: /sliced query set/,
    });
  });
}

test("or() rejects a different model", () => {
  assert.throws(() => Post.objects.or(User.objects as never), {
    name: "TypeError", message: /combines query sets of Post/,
  });
});

test("or() with an unfiltered left returns all rows", async () => {
  await seed();
  const qs = Post.objects.orderBy("title").or(Post.objects.filter(Post.published.eq(false)));
  assert.deepEqual((await qs).map((p) => p.title), ["bob", "draft", "hit", "quiet"]);
});

class InvalidRelatedQueries extends RelatedSet<PostSpec> {}
class InvalidManyRelatedQueries extends ManyRelatedSet<PostSpec> {}
for (const Invalid of [InvalidRelatedQueries, InvalidManyRelatedQueries]) {
  test(`query-set classes reject ${Invalid.name}`, (t) => {
    t.after(() => useQuerySet(Post, QuerySet));
    assert.throws(() => useQuerySet(Post, Invalid), { name: "TypeError", message: /extends QuerySet/ });
  });
}

test("prefetch rejects a column of the instances' own model", async () => {
  const { alice } = await seed();
  await assert.rejects(prefetch([alice], User.name), {
    name: "TypeError", message: /own model has nothing to load/,
  });
});

test("column() rejects an unknown own field", () => {
  assert.throws(() => column(Post, "nope"), { name: "TypeError", message: /Post has no field "nope"/ });
});

test("describe marks a single primary key unique", () => {
  assert.equal(describe(Post).fields.find((f) => f.name === "id")!.unique, true);
  assert.equal(describe(Post).fields.find((f) => f.name === "body")!.unique, false);
});

test("describe reports a client default", () => {
  const registry = new Registry();
  const Item = loads(`model Item {
  id Int @id
  token String @client_default(uuid())
}`, { registry })["Item"]!;
  const token = describe(Item).fields.find((f) => f.name === "token")!;
  assert.equal(token.default, "client");
  assert.equal(token.insert, true);
});

test("describe keeps an autoincrement field insertable", () => {
  const id = describe(Post).fields.find((f) => f.name === "id")!;
  assert.equal(id.default, "database");
  assert.equal(id.insert, true);
});

test("describe marks has-many relations nullable", () => {
  const posts = describe(User).relations.find((r) => r.name === "posts")!;
  assert.equal(posts.kind, "hasMany");
  assert.equal(posts.nullable, true);
  assert.equal(describe(Post).relations.find((r) => r.name === "author")!.nullable, false);
});

test("describe marks an optional belongs-to nullable", () => {
  assert.equal(describe(Comment).relations.find((r) => r.name === "author")!.nullable, true);
  assert.equal(describe(Comment).relations.find((r) => r.name === "post")!.nullable, false);
});
