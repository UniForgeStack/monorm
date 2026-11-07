/** Prepared writes held in Rust: a call writes what the unprepared write writes. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { allowWrites, connect, loads, param, QueryError, Registry, scope, UnfilteredWriteError, WriteProtected, type Database } from "../src/index.js";
import { Comment, Post, User } from "./blog/models.js";
import { hasExtension } from "./artifact-caps.js";
import { useDatabase } from "./helpers.js";

useDatabase();

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

async function users(): Promise<User[]> {
  return (await User.objects
    .insertMany(Array.from({ length: 3 }, (_, i) => ({ email: `u${i}@example.com`, name: `U${i}` })))
    .returning()) as User[];
}

async function table() {
  return (await Post.objects.orderBy(Post.id)).map((p) => [p.title, p.body, p.views]);
}

test("prepared insert returns the instance and matches the plain insert", async () => {
  const [alice] = await users();
  const add = Post.objects.insert({ author: alice!, title: param("t"), body: param("b"), views: param("v") }).prepare();
  const first = await add.run({ t: "a", b: "x", v: 1 });
  const second = await add.run({ t: "b", b: "y", v: 2 });
  const plain = await Post.objects.insert({ author: alice!, title: "c", body: "z", views: 3 });
  assert.deepEqual([first.title, second.title, plain.title], ["a", "b", "c"]);
  assert.notEqual(first.id, second.id);
  assert.deepEqual(await table(), [["a", "x", 1], ["b", "y", 2], ["c", "z", 3]]);
});

test("prepared insert inspect equals the plain inspect", async () => {
  const [alice] = await users();
  const add = Post.objects.insert({ author: alice!, title: param("t"), body: param("b") }).prepare();
  const plain = Post.objects.insert({ author: alice!, title: "a", body: "x" });
  assert.deepEqual(add.inspect({ t: "a", b: "x" }), plain.inspect());
});

test("a param write needs prepare", async () => {
  const [alice] = await users();
  const need = (e: Error) => /prepare/.test(e.message);
  await assert.rejects(Promise.resolve().then(() => Post.objects.insert({ author: alice!, title: param("t"), body: "" })), need);
  await assert.rejects(Post.objects.filter(Post.id.eq(param("i"))).update({ title: "x" }) as Any, need);
  await assert.rejects(Post.objects.filter(Post.id.eq(1n)).update({ title: param("t") }) as Any, need);
});

test("prepared insert many batches", async () => {
  const [alice] = await users();
  const add = Post.objects.insertMany(param("rows", { fields: [Post.authorId, Post.title, Post.body] })).prepare();
  for (const n of [0, 1, 31, 32, 33, 40000]) {
    await Post.objects.all().delete();
    const rows = Array.from({ length: n }, (_, i) => ({ authorId: alice!.id, title: `t${i}`, body: "" }));
    assert.equal(await add.run({ rows }), n);
    assert.equal(await Post.objects.count(), n);
  }
});

test("a large prepared batch keeps row order with returning and upsert", async () => {
  const [alice] = await users();
  const add = Post.objects.insertMany(param("rows", { fields: [Post.authorId, Post.title, Post.body] })).returning().prepare();
  const rows = Array.from({ length: 40 }, (_, i) => ({ authorId: alice!.id, title: `t${i}`, body: "" }));
  const got = (await add.run({ rows })) as Post[];
  assert.deepEqual(got.map((p) => p.title), rows.map((r) => r.title));
  assert.equal(new Set(got.map((p) => p.id)).size, 40);
  const mail = User.objects.insertMany(param("rows", { fields: [User.email, User.name] })).onConflict(User.email, { update: true }).prepare();
  const people = Array.from({ length: 40 }, (_, i) => ({ email: `m${i}@example.com`, name: "a" }));
  assert.equal(await mail.run({ rows: people }), 40);
  assert.equal(await mail.run({ rows: people.map((r) => ({ ...r, name: "b" })) }), 40);
  const named = await User.objects.filter(User.email.like("m%"));
  assert.deepEqual([...new Set(named.map((u) => u.name))], ["b"]);
});

test("prepared update and delete match the plain write", async () => {
  const [alice] = await users();
  const posts = (await Post.objects
    .insertMany(Array.from({ length: 3 }, (_, i) => ({ author: alice!, title: `t${i}`, body: "", views: i })))
    .returning()) as Post[];
  const ids = posts.map((p) => p.id);
  const edit = Post.objects.filter(Post.id.eq(param("id"))).update({ title: param("t") }).prepare();
  assert.deepEqual(edit.inspect({ id: ids[0]!, t: "x" }), Post.objects.filter(Post.id.eq(ids[0]!)).update({ title: "x" }).inspect());
  assert.equal(await edit.run({ id: ids[0]!, t: "x" }), 1);
  assert.equal(await edit.run({ id: -1n, t: "x" }), 0);
  const shown = Post.objects.filter(Post.id.eq(param("id"))).update({ views: param("v") }).returning(Post.id, Post.views).prepare();
  assert.deepEqual((await shown.run({ id: ids[1]!, v: 9 })).map((r: Any) => [r.id, r.views]), [[ids[1], 9]]);
  const drop = Post.objects.filter(Post.id.eq(param("id"))).delete().prepare();
  assert.equal(await drop.run({ id: ids[2]! }), 1);
  assert.equal(await drop.run({ id: ids[2]! }), 0);
  assert.deepEqual(await table(), [["x", "", 0], ["t1", "", 9]]);
});

test("none is null in a value and an error in a filter", async () => {
  const [alice] = await users();
  const add = Post.objects.insert({ author: alice!, title: "a", body: param("b") }).prepare();
  const post = await add.run({ b: "x" });
  const comments = Comment.objects.insert({ postId: post.id, body: "c", authorId: param("a") }).prepare();
  assert.equal((await comments.run({ a: null })).authorId, null);
  assert.equal((await comments.run({ a: alice!.id })).authorId, alice!.id);
  const edit = Post.objects.filter(Post.id.eq(param("id"))).update({ body: param("b") }).prepare();
  await assert.rejects(edit.run({ id: null as Any, b: "y" }), QueryError);
});

test("none is null in a set expression", async () => {
  const [alice] = await users();
  const post = await Post.objects.insert({ author: alice!, title: "a", body: "" });
  const comment = await Comment.objects.insert({ postId: post.id, body: "c", authorId: alice!.id });
  const shift = Comment.objects.filter(Comment.id.eq(param("id"))).update({ authorId: Comment.authorId.add(param("n", "int")) as Any }).prepare();
  assert.equal(await shift.run({ id: comment.id, n: 0 }), 1);
  assert.equal(await shift.run({ id: comment.id, n: null as Any }), 1);
  assert.equal((await Comment.objects.get(Comment.id.eq(comment.id)))!.authorId, null);
});

test("none in an update many filter param is an error", async () => {
  const [alice] = await users();
  const posts = (await Post.objects
    .insertMany(Array.from({ length: 40 }, () => ({ author: alice!, title: "a", body: "" })))
    .returning()) as Post[];
  const edit = Post.objects.filter(Post.views.eq(param("v"))).updateMany(param("rows", { fields: [Post.id, Post.title] })).prepare();
  const rows = posts.map((p) => ({ id: p.id, title: "b" }));
  assert.equal(await edit.run({ v: 0, rows: rows.slice(0, 1) }), 1);
  for (const batch of [rows.slice(0, 1), rows]) await assert.rejects(edit.run({ v: null as Any, rows: batch }), QueryError);
  assert.equal((await Post.objects.orderBy(Post.id)).filter((p) => p.title === "b").length, 1);
});

test("a row with a missing or an extra key fails before any SQL", async () => {
  const [alice] = await users();
  const add = Post.objects.insertMany(param("rows", { fields: [Post.authorId, Post.title, Post.body] })).prepare();
  const good = { authorId: alice!.id, title: "t", body: "" };
  await assert.rejects(add.run({ rows: [good, { authorId: alice!.id, title: "t" } as Any] }), /no body/);
  await assert.rejects(add.run({ rows: [good, { ...good, extra: 1 } as Any] }), /extra/);
  assert.equal(await Post.objects.count(), 0);
});

test("prepared update many matches the plain update many", async () => {
  const [alice] = await users();
  const posts = (await Post.objects
    .insertMany(Array.from({ length: 40 }, (_, i) => ({ author: alice!, title: `t${i}`, body: "" })))
    .returning()) as Post[];
  const edit = Post.objects.updateMany(param("rows", { fields: [Post.id, Post.title] })).prepare();
  for (const n of [0, 1, 31, 32, 40]) {
    const rows = posts.slice(0, n).map((p) => ({ id: p.id, title: `n${n}-${p.id}` }));
    assert.equal(await edit.run({ rows }), n);
  }
  assert.equal((await Post.objects.get(Post.id.eq(posts[39]!.id)))!.title, `n40-${posts[39]!.id}`);
  await assert.rejects(edit.run({ rows: [{ title: "x" } as Any] }), /no id/);
});

test("prepared upsert keeps its conflict fields", async () => {
  const [alice] = await users();
  const add = User.objects.insert({ email: param("e"), name: param("n") }).onConflict(User.email, { update: false }).prepare();
  assert.equal(await add.run({ e: alice!.email, n: "other" }), 0);
  assert.equal(await add.run({ e: "new@example.com", n: "N" }), 1);
  assert.equal((await User.objects.get(User.id.eq(alice!.id)))!.name, alice!.name);
  const upd = User.objects.insert({ email: param("e"), name: param("n") }).onConflict(User.email, { update: true }).prepare();
  await upd.run({ e: alice!.email, n: "renamed" });
  assert.equal((await User.objects.get(User.id.eq(alice!.id)))!.name, "renamed");
});

// -- schemas of their own ---------------------------------------------------------------------

const SCHEMA = `
model Item {
  shop Int
  id Int
  name String
  tags String[]
  mail String @db.Citext
  @@id([shop, id])
  @@map("pw_js_item")
}
model Note {
  id Int @id @default(autoincrement())
  shop Int
  name String
  views Int @default(0)
  @@query.filter("tenant", "shop == scope.shop")
  @@map("pw_js_note")
}
model Vault {
  id Int @id
  name String
  @@protected_write
  @@map("pw_js_vault")
}
`;

async function open(source: string): Promise<{ db: Database; models: Record<string, Any> }> {
  const registry = new Registry();
  const models = loads(`datasource db {\n  provider = "postgresql"\n}\n${source}`, { registry }) as Record<string, Any>;
  const db = await connect(process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test", { registry, default: false });
  await db.dropTables();
  await db.createTables();
  return { db, models };
}

async function withDb(source: string, body: (db: Database, models: Record<string, Any>) => Promise<void>): Promise<void> {
  const { db, models } = await open(source);
  try {
    await body(db, models);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

test("composite keys, array columns and writeSql fields in batches", async () => {
  await withDb(SCHEMA, async (db, models) => {
    const model = models["Item"];
    const items = model.objects.using(db);
    const add = items.insertMany(param("rows", { fields: [model.shop, model.id, model.name, model.tags, model.mail] })).prepare();
    for (const n of [1, 31, 32, 33, 70]) {
      await items.all().delete();
      const rows = Array.from({ length: n }, (_, i) => ({ shop: i % 3, id: i, name: `n${i}`, tags: [`a${i}`, "b"], mail: `M${i}@X.com` }));
      assert.equal(await add.run({ rows }), n);
      const got = (await items.orderBy(model.id)) as Any[];
      assert.deepEqual(got.map((r) => [r.shop, r.id, r.tags, r.mail]), rows.map((r) => [r.shop, r.id, r.tags, r.mail]));
    }
    const edit = items.updateMany(param("rows", { fields: [model.shop, model.id, model.name, model.tags] })).prepare();
    const rows = Array.from({ length: 70 }, (_, i) => ({ shop: i % 3, id: i, name: `x${i}`, tags: [`z${i}`] }));
    assert.equal(await edit.run({ rows }), 70);
    const got = (await items.orderBy(model.id)) as Any[];
    assert.deepEqual(got.map((r) => [r.name, r.tags]), rows.map((r) => [r.name, r.tags]));
    assert.equal(await edit.run({ rows: [{ ...rows[0]!, shop: 99 }] }), 0);
  });
});

test("scope binds per call in a prepared write", { skip: !hasExtension("monorm.query-defaults") }, async () => {
  await withDb(SCHEMA, async (db, models) => {
    const Note = models["Note"];
    const notes = Note.objects.using(db);
    await notes.withoutDefaults().insertMany([1, 1, 2].map((shop) => ({ shop, name: "n", views: 0 })));
    const bump = notes.filter(Note.name.eq(param("n"))).update({ views: param("v") }).prepare();
    await scope({ shop: 1 }, async () => {
      assert.equal(await bump.run({ n: "n", v: 5 }), 2);
    });
    assert.equal(await bump.run({ n: "n", v: 7, scope: { shop: 2 } }), 1);
    const got = ((await notes.withoutDefaults().all()) as Any[]).map((r) => [r.shop, r.views]).sort();
    assert.deepEqual(got, [[1, 5], [1, 5], [2, 7]]);
    const drop = notes.filter(Note.name.eq(param("n"))).delete().prepare();
    assert.equal(await drop.run({ n: "n", scope: { shop: 1 } }), 2);
    assert.equal(await notes.withoutDefaults().count(), 1);
  });
});

test("a scoped update many of 33 rows keeps the scope filter", { skip: !hasExtension("monorm.query-defaults") }, async () => {
  await withDb(SCHEMA, async (db, models) => {
    const Note = models["Note"];
    const notes = Note.objects.using(db);
    await notes.withoutDefaults().insertMany(Array.from({ length: 40 }, (_, i) => ({ shop: 1 + (i % 2), name: `n${i}`, views: 0 })));
    const ids = ((await notes.withoutDefaults().orderBy(Note.id).all()) as Any[]).map((r) => [r.id, r.shop]);
    const edit = notes.updateMany(param("rows", { fields: [Note.id, Note.views] })).prepare();
    const rows = ids.slice(0, 33).map(([id], i) => ({ id, views: 100 + i }));
    await scope({ shop: 1 }, async () => {
      assert.equal(await edit.run({ rows }), 17);
    });
    const got = new Map(((await notes.withoutDefaults().all()) as Any[]).map((r) => [r.id, [r.shop, r.views]]));
    ids.forEach(([id, shop], i) => {
      assert.deepEqual(got.get(id), [shop, i < 33 && shop === 1 ? 100 + i : 0]);
    });
  });
});

test("the write guard and protectedWrite run per call", async () => {
  await withDb(SCHEMA, async (db, models) => {
    const Vault = models["Vault"];
    const vaults = Vault.objects.using(db);
    const add = vaults.insert({ id: param("i"), name: param("n") }).prepare();
    await assert.rejects(add.run({ i: 1, n: "a" }), WriteProtected);
    await allowWrites([Vault], () => add.run({ i: 1, n: "a" }));
    await assert.rejects(add.run({ i: 2, n: "b" }), WriteProtected);
    assert.equal(await vaults.count(), 1);
    const rename = vaults.filter(Vault.id.eq(param("i"))).update({ name: param("n") }).prepare();
    await assert.rejects(rename.run({ i: 1, n: "c" }), WriteProtected);
    assert.equal(await allowWrites([Vault], () => rename.run({ i: 1, n: "c" })), 1);
    assert.throws(() => vaults.update({ name: "x" }).prepare(), UnfilteredWriteError);
    assert.equal((await vaults.get(Vault.id.eq(1))).name, "c");
  });
});

test("timestamp hooks run per call", { skip: !hasExtension("monorm.timestamps") }, async () => {
  const source = 'model Stamp {\n  id Int @id\n  views Int @default(0)\n  @@timestamps.updated_at("changed", mode: "application")\n  @@map("pw_js_stamp")\n}\n';
  await withDb(source, async (db, models) => {
    const Stamp = models["Stamp"];
    const stamps = Stamp.objects.using(db);
    const add = stamps.insert({ id: param("i") }).prepare();
    const first = await add.run({ i: 1 });
    const second = await add.run({ i: 2 });
    assert.ok(second.changed >= first.changed);
    const edit = stamps.filter(Stamp.id.eq(param("i"))).update({ views: param("v") }).prepare();
    const before = (await stamps.get(Stamp.id.eq(1))).changed;
    // The clock has millisecond steps: without a pause the update can stamp the same time.
    await new Promise((resolve) => setTimeout(resolve, 5));
    await edit.run({ i: 1, v: 3 });
    assert.ok((await stamps.get(Stamp.id.eq(1))).changed > before);
  });
});

test("a multi-table insert runs the same statements as the plain insert", { skip: !hasExtension("monorm.composition") }, async () => {
  const source =
    'model Person {\n  id Int @id @default(autoincrement())\n  name String\n  @@map("pw_js_people")\n}\n' +
    'model Staff {\n  salary Int\n  @@composition.model(parent: "Person", parentRef: "person", childRef: "staff")\n  @@map("pw_js_staff")\n}\n';
  await withDb(source, async (db, models) => {
    const staff = models["Staff"].objects.using(db);
    const seen = (): { sql: string[]; stop: () => void } => {
      const sql: string[] = [];
      const stop = db.onQuery((e) => { sql.push(e.sql); });
      return { sql, stop };
    };
    const plain = seen();
    await staff.insert({ name: "a", salary: 1 });
    plain.stop();
    const add = staff.insert({ name: param("n"), salary: param("s") }).prepare();
    const events = seen();
    const row = await add.run({ n: "b", s: 2 });
    events.stop();
    assert.deepEqual(events.sql, plain.sql);
    assert.deepEqual([row.name, row.salary], ["b", 2]);
    assert.deepEqual(((await staff.all()) as Any[]).map((r) => [r.name, r.salary]).sort(), [["a", 1], ["b", 2]]);
  });
});
