import assert from "node:assert/strict";
import { test } from "node:test";
import { AdapterWriteError, FieldAdapterError, Pending, allowWrites, connect, loads, registerFieldAdapter, Registry, type Row } from "../src/index.js";
import type { ModelMeta } from "../src/model.js";

interface DocModel { readonly _meta: ModelMeta }

const source = `datasource db { provider = "sqlite" }
model Doc {
  id Int @id
  title String
  body Json?
  extra Json?
  views Int @default(0)
}
model Locked {
  id Int @id
  body Json?
  @@protected_write
}`;

class Counting {
  calls: unknown[] = [];
  constructor(readonly tag: string, readonly failOn?: unknown) {}
  validate(value: unknown) {
    if (value === null || typeof value === "object") return value;
    if (typeof value !== "string") throw new TypeError(`${this.tag}: unsupported value ${String(value)}`);
    return new Pending(value, { placeholder: true });
  }
  async prepare(value: unknown) {
    this.calls.push(value);
    if (value === this.failOn) throw new Error("prepare failed");
    return { tag: this.tag, value };
  }
  decode(stored: unknown) { return stored === null ? null : [this.tag, stored]; }
}

async function open(extra?: Counting, body: Counting = new Counting("body")) {
  const registry = new Registry();
  const models = loads(source, { registry }) as Record<string, any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  registerFieldAdapter(models.Doc._meta, "body", body);
  if (extra) registerFieldAdapter(models.Doc._meta, "extra", extra);
  const locked = new Counting("locked");
  registerFieldAdapter(models.Locked._meta, "body", locked);
  return { db, Doc: models.Doc, Locked: models.Locked, locked, body, objects: models.Doc.objects.using(db) as any };
}

test("insert prepares once and decodes the loaded value", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    const row = await objects.insert({ id: 1, title: "a", body: "file" }) as Row;
    assert.deepEqual(body.calls, ["file"]);
    assert.deepEqual(row["body"], ["body", { tag: "body", value: "file" }]);
    assert.deepEqual(((await objects.filter(Doc.id.eq(1)).get()) as Row)["body"], ["body", { tag: "body", value: "file" }]);
  } finally { await db.close(); }
});

test("a value that needs no prepare writes without a call", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    await objects.insert({ id: 1, title: "a", body: { stored: 1 } });
    await objects.insert({ id: 2, title: "b", body: null });
    assert.deepEqual(body.calls, []);
    assert.deepEqual(((await objects.filter(Doc.id.eq(1)).get()) as Row)["body"], ["body", { stored: 1 }]);
    assert.equal(((await objects.filter(Doc.id.eq(2)).get()) as Row)["body"], null);
  } finally { await db.close(); }
});

test("validation and the statement check run before any prepare", async () => {
  const { db, body, objects } = await open();
  try {
    assert.throws(() => objects.insert({ id: 1, title: "a", body: 3 }), /unsupported value 3/);
    await assert.rejects(async () => objects.insert({ id: 1, body: "file" }), /title is required/);
    assert.deepEqual(body.calls, []);
  } finally { await db.close(); }
});

test("write protection is checked before prepare", async () => {
  const { db, Locked, locked: adapter } = await open();
  try {
    const locked = Locked.objects.using(db);
    await assert.rejects(async () => locked.insert({ id: 1, body: "file" }), /write-protected/);
    assert.equal(await locked.count(), 0);
    assert.deepEqual(adapter.calls, []);
    await allowWrites([Locked], async () => { await locked.insert({ id: 1, body: "file" }); });
    assert.equal(await locked.count(), 1);
    assert.deepEqual(adapter.calls, ["file"]);
  } finally { await db.close(); }
});

test("onConflict and an expression are rejected before any prepare", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    assert.throws(() => objects.insert({ id: 1, title: "a", body: "file" }).onConflict(Doc.id, { update: true }), FieldAdapterError);
    assert.throws(() => objects.filter(Doc.id.eq(1)).update({ body: Doc.views.add(1) }), /takes no expression/);
    assert.deepEqual(body.calls, []);
  } finally { await db.close(); }
});

test("a unique update prepares once", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    await objects.insertMany([{ id: 1, title: "a" }, { id: 2, title: "a" }]);
    assert.equal(await objects.filter(Doc.id.eq(1)).update({ body: "new" }), 1);
    assert.deepEqual(body.calls, ["new"]);
    const rows = await objects.filter(Doc.id.eq(2)).update({ body: "again" }, { returning: true }) as Row[];
    assert.deepEqual(rows.map(r => r["body"]), [["body", { tag: "body", value: "again" }]]);
  } finally { await db.close(); }
});

test("a filtered update prepares once and writes the same value to every row", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    await objects.insertMany([{ id: 1, title: "a" }, { id: 2, title: "a" }, { id: 3, title: "b" }]);
    assert.equal(await objects.filter(Doc.title.eq("a")).update({ body: "shared" }), 2);
    assert.deepEqual(body.calls, ["shared"]);
    const shared = ["body", { tag: "body", value: "shared" }];
    assert.deepEqual((await objects.orderBy(Doc.id).fetch() as Row[]).map(r => r["body"]), [shared, shared, null]);
    const rows = await objects.filter(Doc.title.eq("b")).update({ body: "other" }, { returning: true }) as Row[];
    assert.deepEqual(rows.map(r => r["id"]), [3]);
    assert.deepEqual(body.calls, ["shared", "other"]);
  } finally { await db.close(); }
});

test("an instance update runs the pipeline", async () => {
  const { db, body, objects } = await open();
  try {
    const row = await objects.insert({ id: 1, title: "a" }) as any;
    await row.update({ body: "later" });
    assert.deepEqual(body.calls, ["later"]);
    assert.deepEqual(row.body, ["body", { tag: "body", value: "later" }]);
  } finally { await db.close(); }
});

test("two adapted fields run in one pipeline", async () => {
  const extra = new Counting("extra");
  const { db, body, objects } = await open(extra);
  try {
    const row = await objects.insert({ id: 1, title: "a", extra: "two", body: "one" }) as Row;
    assert.deepEqual([body.calls, extra.calls], [["one"], ["two"]]);
    assert.deepEqual(row["extra"], ["extra", { tag: "extra", value: "two" }]);
  } finally { await db.close(); }
});

test("a failed second prepare lists the first stored value", async () => {
  const { db, objects } = await open(new Counting("extra", "two"));
  try {
    await assert.rejects(async () => objects.insert({ id: 1, title: "a", body: "one", extra: "two" }), (error: unknown) => {
      assert.ok(error instanceof AdapterWriteError);
      assert.deepEqual(error.stored, { body: { tag: "body", value: "one" } });
      return true;
    });
    assert.equal(await objects.count(), 0);
  } finally { await db.close(); }
});

test("a failed SQL statement retries without a new prepare", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    await objects.insert({ id: 1, title: "taken" });
    let failure: AdapterWriteError | undefined;
    await objects.insert({ id: 1, title: "again", body: "file" }).then(undefined, (e: unknown) => { failure = e as AdapterWriteError; });
    assert.ok(failure instanceof AdapterWriteError);
    assert.deepEqual(body.calls, ["file"]);
    assert.deepEqual(failure.stored, { body: { tag: "body", value: "file" } });
    await objects.filter(Doc.id.eq(1)).delete();
    const row = await failure.operation.execute() as Row;
    assert.deepEqual(body.calls, ["file"]);
    assert.deepEqual(row["body"], ["body", { tag: "body", value: "file" }]);
  } finally { await db.close(); }
});

test("batch writes store checked values without a prepare call", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    await objects.insertMany([{ id: 1, title: "a", body: { stored: 1 } }, { id: 2, title: "b", body: null }]);
    assert.deepEqual(((await objects.filter(Doc.id.eq(1)).get()) as Row)["body"], ["body", { stored: 1 }]);
    assert.equal(((await objects.filter(Doc.id.eq(2)).get()) as Row)["body"], null);
    await objects.updateMany([{ id: 1, body: null }, { id: 2, body: { stored: 2 } }]);
    assert.equal(((await objects.filter(Doc.id.eq(1)).get()) as Row)["body"], null);
    assert.deepEqual(((await objects.filter(Doc.id.eq(2)).get()) as Row)["body"], ["body", { stored: 2 }]);
    assert.deepEqual(body.calls, []);
  } finally { await db.close(); }
});

const stored = (tag: string, value: unknown) => [tag, { tag, value }];

test("insertMany prepares each pending value and keeps the others", async () => {
  const { db, body, objects } = await open();
  try {
    const rows = [
      { id: 1, title: "a", body: "one" },
      { id: 2, title: "b", body: { stored: 2 } },
      { id: 3, title: "c", body: "three" },
      { id: 4, title: "d" },
    ];
    assert.equal(await objects.insertMany(rows), 4);
    assert.deepEqual([...body.calls].sort(), ["one", "three"]);
    const got = Object.fromEntries((await objects.fetch() as Row[]).map(r => [r["id"], r["body"]]));
    assert.deepEqual(got, { 1: stored("body", "one"), 2: ["body", { stored: 2 }], 3: stored("body", "three"), 4: null });
    const news = await objects.insertMany([{ id: 5, title: "e", body: "five" }]).returning() as Row[];
    assert.deepEqual(news.map(r => [r["id"], r["body"]]), [[5, stored("body", "five")]]);
    assert.equal(rows[0]!.body, "one");
  } finally { await db.close(); }
});

test("updateMany prepares each pending value", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    await objects.insertMany([{ id: 1, title: "a" }, { id: 2, title: "b" }, { id: 3, title: "c" }]);
    assert.equal(await objects.updateMany([{ id: 1, body: "one" }, { id: 2, body: { stored: 2 } }]), 2);
    assert.deepEqual(body.calls, ["one"]);
    const done = await objects.updateMany([{ id: 2, body: "two" }, { id: 3, body: "three" }], { returning: true }) as Row[];
    assert.deepEqual([...body.calls].sort(), ["one", "three", "two"]);
    assert.deepEqual(Object.fromEntries(done.map(r => [r["id"], r["body"]])), { 2: stored("body", "two"), 3: stored("body", "three") });
    assert.deepEqual(((await objects.filter(Doc.id.eq(1)).get()) as Row)["body"], stored("body", "one"));
  } finally { await db.close(); }
});

class Parallel extends Counting {
  running = 0;
  peak = 0;
  override async prepare(value: unknown) {
    this.running++;
    this.peak = Math.max(this.peak, this.running);
    await new Promise(resolve => setTimeout(resolve, 10));
    this.running--;
    return super.prepare(value);
  }
}

test("batch prepare calls run in parallel up to the limit", async () => {
  const body = new Parallel("body");
  const registry = new Registry();
  const models = loads(source, { registry }) as Record<string, any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    registerFieldAdapter(models.Doc._meta, "body", body);
    const objects = models.Doc.objects.using(db) as any;
    const ids = Array.from({ length: 40 }, (_, n) => n + 1);
    await objects.insertMany(ids.map(id => ({ id, title: "t", body: `f${id}` })));
    assert.equal(body.calls.length, 40);
    assert.equal(body.peak, 16);
    body.peak = 0;
    await objects.updateMany(ids.map(id => ({ id, body: `g${id}` })));
    assert.equal(body.peak, 16);
  } finally { await db.close(); }
});

test("a failed batch prepare lists the finished values and writes nothing", async () => {
  const { db, Doc, objects } = await open();
  try {
    const failing = new Counting("body", "bad");
    registerFieldAdapter((Doc as DocModel)._meta, "extra", failing);
    const rows = [{ id: 1, title: "a", extra: "good" }, { id: 2, title: "b", extra: "bad" }];
    await assert.rejects(async () => objects.insertMany(rows), (error: unknown) => {
      assert.ok(error instanceof AdapterWriteError);
      assert.deepEqual(error.stored, { "0.extra": { tag: "body", value: "good" } });
      assert.match(String((error.cause as Error).message), /prepare failed/);
      return true;
    });
    assert.equal(await objects.count(), 0);
    await objects.insertMany([{ id: 1, title: "a" }, { id: 2, title: "b" }]);
    await assert.rejects(async () => objects.updateMany([{ id: 1, extra: "bad" }, { id: 2, extra: "good" }]), AdapterWriteError);
    assert.deepEqual((await objects.orderBy(Doc.id).fetch() as Row[]).map(r => r["extra"]), [null, null]);
  } finally { await db.close(); }
});

test("a failed batch insert retries without a new prepare", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    await objects.insert({ id: 2, title: "taken" });
    let failure: AdapterWriteError | undefined;
    await objects.insertMany([{ id: 1, title: "a", body: "one" }, { id: 2, title: "b", body: "two" }]).then(undefined, (e: unknown) => { failure = e as AdapterWriteError; });
    assert.ok(failure instanceof AdapterWriteError);
    assert.deepEqual([...body.calls].sort(), ["one", "two"]);
    assert.deepEqual(Object.keys(failure.stored).sort(), ["0.body", "1.body"]);
    await objects.filter(Doc.id.eq(2)).delete();
    assert.equal(await failure.operation.execute(), 2);
    assert.deepEqual([...body.calls].sort(), ["one", "two"]);
    assert.deepEqual(((await objects.filter(Doc.id.eq(2)).get()) as Row)["body"], stored("body", "two"));
  } finally { await db.close(); }
});

/** A source that one `prepare` call reads empty after the first. */
class OneShot {
  constructor(public data: string) {}
  read(): string { const data = this.data; this.data = ""; return data; }
}

class Streams extends Counting {
  override validate(value: unknown) {
    return value instanceof OneShot ? new Pending(value, { placeholder: true }) : super.validate(value);
  }
  override async prepare(value: unknown): Promise<any> {
    if (!(value instanceof OneShot)) return super.prepare(value);
    this.calls.push(value);
    return { tag: this.tag, size: value.read().length };
  }
}

test("one object in several rows prepares once", async () => {
  const streams = new Streams("body");
  const { db, objects } = await open(undefined, streams);
  try {
    const shared = new OneShot("hello");
    await objects.insertMany([1, 2, 3].map(id => ({ id, title: "t", body: shared })));
    assert.equal(streams.calls.length, 1);
    assert.deepEqual(((await objects.all()) as Row[]).map(r => r["body"]), Array(3).fill(["body", { tag: "body", size: 5 }]));
    await objects.updateMany([1, 2].map(id => ({ id, body: new OneShot("hi") })));
    assert.equal(streams.calls.length, 3);
  } finally { await db.close(); }
});

test("updateMany validates every row before any prepare", async () => {
  const { db, body, objects } = await open();
  try {
    await assert.rejects(async () => objects.updateMany([{ id: 1, title: "ok", body: "aa" }, { id: 2, title: 5, body: "bb" }]));
    assert.deepEqual(body.calls, []);
  } finally { await db.close(); }
});

test("insertMany with a bad batchSize fails before any prepare", async () => {
  const { db, body, objects } = await open();
  try {
    await assert.rejects(async () => objects.insertMany([{ id: 1, title: "a", body: "one" }], { batchSize: 0 }), /batchSize/);
    assert.deepEqual(body.calls, []);
  } finally { await db.close(); }
});

test("a failed batch update retries without a new prepare", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    await objects.insert({ id: 1, title: "a" });
    await db.execute("DROP TABLE Doc");
    let failure: AdapterWriteError | undefined;
    await objects.updateMany([{ id: 1, body: "one" }, { id: 2, body: "two" }]).then(undefined, (e: unknown) => { failure = e as AdapterWriteError; });
    assert.ok(failure instanceof AdapterWriteError);
    assert.deepEqual([...body.calls].sort(), ["one", "two"]);
    assert.deepEqual(Object.keys(failure.stored).sort(), ["0.body", "1.body"]);
    await db.createTables();
    await objects.insert({ id: 1, title: "a" });
    assert.equal(await failure.operation.execute(), 1);
    assert.deepEqual([...body.calls].sort(), ["one", "two"]);
    assert.deepEqual(((await objects.filter(Doc.id.eq(1)).get()) as Row)["body"], stored("body", "one"));
  } finally { await db.close(); }
});

test("batch validation and protection run before any prepare", async () => {
  const { db, Locked, locked, body, objects } = await open();
  try {
    assert.throws(() => objects.insertMany([{ id: 1, title: "a", body: "ok" }, { id: 2, title: "b", body: 3 }]), /unsupported value 3/);
    assert.throws(() => objects.updateMany([{ id: 1, body: "ok" }, { id: 2, body: 3 }]), /unsupported value 3/);
    await assert.rejects(async () => objects.insertMany([{ id: 1, body: "ok" }]), /title is required/);
    const protectedRows = Locked.objects.using(db);
    await assert.rejects(async () => protectedRows.insertMany([{ id: 1, body: "file" }]), /write-protected/);
    await assert.rejects(async () => protectedRows.updateMany([{ id: 1, body: "file" }]), /write-protected/);
    assert.deepEqual(body.calls, []);
    assert.deepEqual(locked.calls, []);
    await allowWrites([Locked], async () => { assert.equal(await protectedRows.insertMany([{ id: 1, body: "file" }]), 1); });
    assert.deepEqual(locked.calls, ["file"]);
  } finally { await db.close(); }
});

test("a pending batch value is rejected in a conflict write and in copy", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    assert.throws(() => objects.insertMany([{ id: 1, title: "a", body: "file" }]).onConflict(Doc.id, { update: true }), FieldAdapterError);
    assert.throws(() => objects.insertMany([{ id: 1, title: "a", body: "file" }], { copy: true }), /copy/);
    assert.deepEqual(body.calls, []);
  } finally { await db.close(); }
});

test("inspect on an adapted batch shows the placeholder statement and calls no prepare", async () => {
  const { db, body, objects } = await open();
  try {
    assert.equal(objects.insertMany([{ id: 1, title: "a", body: "file" }]).inspect().statements.length, 1);
    assert.equal(objects.updateMany([{ id: 1, body: "file" }]).inspect().statements.length, 1);
    assert.deepEqual(body.calls, []);
  } finally { await db.close(); }
});

const m2mSource = `datasource db { provider = "sqlite" }
model Post {
  id Int @id
  tags Tag[] @relation(through: PostTag, through_fields: [post, tag])
}
model Tag {
  id Int @id
  name String
  icon Json?
  posts Post[] @relation(through: PostTag, through_fields: [tag, post])
}
model PostTag {
  id Int @id @default(autoincrement())
  post_id Int
  tag_id Int
  post Post @relation(fields: [post_id], references: [id])
  tag Tag @relation(fields: [tag_id], references: [id])
}`;

test("a related insert on a many-to-many prepares before the transaction", async () => {
  const registry = new Registry();
  const models = loads(m2mSource, { registry }) as Record<string, any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const inside: boolean[] = [];
    const icon = new Counting("icon");
    const prepare = icon.prepare.bind(icon);
    icon.prepare = async (value: unknown) => { inside.push(db.tx() !== null); return prepare(value); };
    registerFieldAdapter(models.Tag._meta, "icon", icon);
    const post = await models.Post.objects.using(db).insert({ id: 1 });
    const tag = await post.tags.insert({ id: 1, name: "a", icon: "file" });
    assert.deepEqual(inside, [false]);
    assert.deepEqual(tag.icon, stored("icon", "file"));
    assert.deepEqual((await post.tags.fetch()).map((t: Row) => t["id"]), [1]);
    await assert.rejects(async () => post.tags.insert({ id: 1, name: "again", icon: "file" }), AdapterWriteError);
    assert.deepEqual(icon.calls, ["file", "file"]);
    assert.equal(await models.Post.objects.using(db).count(), 1);
    assert.deepEqual((await post.tags.fetch()).map((t: Row) => t["id"]), [1]);
  } finally { await db.close(); }
});

test("through defaults prepare once, before the transaction, for every join row", async () => {
  const registry = new Registry();
  const models = loads(m2mSource.replace("  tag_id Int\n", "  tag_id Int\n  file Json?\n"), { registry }) as Record<string, any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const inside: boolean[] = [];
    const file = new Streams("file");
    const prepare = file.prepare.bind(file);
    file.prepare = async (value: unknown) => { inside.push(db.tx() !== null); return prepare(value); };
    registerFieldAdapter(models.PostTag._meta, "file", file);
    await models.Tag.objects.using(db).insertMany([1, 2, 3, 4].map(id => ({ id, name: String(id) })));
    const post = await models.Post.objects.using(db).insert({ id: 1 });
    await post.tags.set([1, 2, 3], { throughDefaults: { file: new OneShot("hello") } });
    assert.equal(file.calls.length, 1);
    assert.deepEqual(inside, [false]);
    const links = (await models.PostTag.objects.using(db).all()) as Row[];
    assert.deepEqual(links.map(l => l["file"]), Array(3).fill(["file", { tag: "file", size: 5 }]));
    await post.tags.add(4, { throughDefaults: { file: new OneShot("hi") } });
    assert.deepEqual(inside, [false, false]);
  } finally { await db.close(); }
});

test("a filtered update and an upsert take null or a stored value", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    await objects.insert({ id: 1, title: "a", body: { stored: 1 } });
    await objects.insert({ id: 2, title: "b", body: { stored: 2 } });
    assert.equal(await objects.filter(Doc.id.gt(0)).update({ body: null }), 2);
    assert.equal(((await objects.filter(Doc.id.eq(1)).get()) as Row)["body"], null);
    await objects.filter(Doc.id.gt(0)).update({ body: { stored: 9 } });
    assert.deepEqual(((await objects.filter(Doc.id.eq(2)).get()) as Row)["body"], ["body", { stored: 9 }]);
    await objects.insert({ id: 1, title: "again", body: null }).onConflict(Doc.id, { update: true });
    assert.equal(((await objects.filter(Doc.id.eq(1)).get()) as Row)["title"], "again");
    const [row, created] = await objects.getOrInsert({ id: 3 }, { defaults: { title: "c", body: { stored: 5 } } });
    assert.ok(created);
    assert.deepEqual(row["body"], ["body", { stored: 5 }]);
    const [nullRow, nullCreated] = await objects.getOrInsert({ id: 4 }, { defaults: { title: "d", body: null } });
    assert.ok(nullCreated);
    assert.equal(nullRow["body"], null);
    assert.deepEqual(body.calls, []);
  } finally { await db.close(); }
});

test("a method that collides with a model member is rejected", async () => {
  const { db, Doc } = await open();
  try {
    for (const name of ["title", "refresh", "update", "toString"]) {
      assert.throws(() => registerFieldAdapter((Doc as DocModel)._meta, "extra", new Counting("x"), { [name]() { return 1; } }), /collides with a model member/);
    }
  } finally { await db.close(); }
});

test("adapter methods are installed on the model", async () => {
  const { db, Doc, objects } = await open();
  try {
    registerFieldAdapter((Doc as DocModel)._meta, "extra", new Counting("x"), { extraOpen() { return "ok"; } });
    const row = await objects.insert({ id: 1, title: "a" }) as Row & { extraOpen(): string };
    assert.equal(row.extraOpen(), "ok");
  } finally { await db.close(); }
});

test("inspect on an adapted write shows the placeholder statement and calls no prepare", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    const insert = objects.insert({ id: 1, title: "a", body: "file" });
    assert.equal(insert.inspect().statements.length, 1);
    const update = objects.filter(Doc.id.eq(1)).update({ body: "file" });
    assert.equal(update.inspect().statements.length, 1);
    assert.deepEqual(body.calls, []);
    assert.ok(insert.operation && update.operation);
  } finally { await db.close(); }
});
