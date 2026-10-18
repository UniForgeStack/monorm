import assert from "node:assert/strict";
import { test } from "node:test";
import { AdapterWriteError, FieldAdapterError, Pending, allowWrites, connect, loads, registerFieldAdapter, Registry, type Row } from "../src/index.js";
import { checkRequires } from "../src/adapters.js";

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

async function open(extra?: Counting) {
  const registry = new Registry();
  const models = loads(source, { registry }) as Record<string, any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  const body = new Counting("body");
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

test("a unique update prepares once; a filtered update rejects", async () => {
  const { db, Doc, body, objects } = await open();
  try {
    await objects.insertMany([{ id: 1, title: "a" }, { id: 2, title: "a" }]);
    assert.equal(await objects.filter(Doc.id.eq(1)).update({ body: "new" }), 1);
    assert.deepEqual(body.calls, ["new"]);
    await assert.rejects(async () => objects.filter(Doc.title.eq("a")).update({ body: "shared" }), /one unique row/);
    assert.deepEqual(body.calls, ["new"]);
    const rows = await objects.filter(Doc.id.eq(2)).update({ body: "again" }, { returning: true }) as Row[];
    assert.deepEqual(rows.map(r => r["body"]), [["body", { tag: "body", value: "again" }]]);
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

test("a missing language capability fails the load, naming the extension", () => {
  assert.throws(() => checkRequires({ requires: { "file-storage": ["async-field-adapter.v1"], other: ["stream.v9"] } }), /extension other requires the capability stream\.v9/);
  assert.throws(() => checkRequires({ requires: { "file-storage": ["async-field-adapter.v1"] } }, new Set()), /file-storage requires the capability async-field-adapter\.v1/);
  checkRequires({ requires: { "file-storage": ["async-field-adapter.v1"] } });
  checkRequires({});
});
