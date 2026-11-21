/** `@@history.track`: event tables, triggers, timeline, restore and purge; needs the `monorm.history` package. */

import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { allowWrites, connect, Decimal, loads, Migrations, QueryError, Registry, scope, skipTriggers, WriteProtected, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const SCHEMA = `
model Post {
  id Int @id @default(autoincrement())
  title String
  body String?
  views Int @default(0)
  @@timestamps.updated_at("changed")
  @@history.track
  @@map("hx_js_posts")
}
model Note {
  id Int @id @default(autoincrement())
  title String
  body String?
  @@history.track(events: [update], fields: [title], label: "title_changed")
  @@map("hx_js_notes")
}
model Both {
  id Int @id @default(autoincrement())
  name String
  @@history.track
  @@history.track(model: "BothAudit", events: [delete])
  @@map("hx_js_both")
}
model Typed {
  id Int @id @default(autoincrement())
  at DateTime
  extra Json?
  @@history.track
  @@map("hx_js_typed")
}
model Line {
  orderId Int @map("order_id")
  sku String
  qty Int
  @@id([orderId, sku])
  @@history.track
  @@map("hx_js_lines")
}
`;

const SCOPED = `
model Doc {
  id Int @id @default(autoincrement())
  title String
  @@history.track(scope: [user])
  @@map("hx_js_docs")
}
`;

const skip = !hasExtension("monorm.history", "monorm.timestamps");
const urlOf = (provider: string) => (provider === "sqlite" ? "sqlite://:memory:" : (process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test"));

async function withSchema(provider: string, schema: string, body: (db: Database, models: Record<string, Any>) => Promise<void>, datasource = ""): Promise<void> {
  const registry = new Registry();
  const models = loads(`datasource db {\n  provider = "${provider}"\n${datasource}}\n${schema}`, { registry }) as Record<string, Any>;
  const db = await connect(urlOf(provider), { registry, default: false });
  await db.dropTables();
  await db.createTables();
  try {
    await body(db, models);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

const timeline = async (row: Any, options: Any = {}) => (await row.history.timeline(row, options)) as { event: Any; diff: Any }[];
const labels = async (row: Any) => (await timeline(row)).map((e) => e.event.label);
const ids = (entries: { event: Any }[]) => entries.map((e) => e.event.id);
const anyOf = (m: Any, name: string) => m[name].objects;

for (const provider of ["sqlite", "postgresql"]) {
  const scenario = (what: string, body: (db: Database, models: Record<string, Any>) => Promise<void>) => {
    const bypass = provider === "postgresql" && !what.startsWith("a skip outside") ? '  bypass_roles = ["postgres"]\n' : "";
    return test(what, { skip }, () => withSchema(provider, SCHEMA, body, bypass));
  };

  scenario("the event model is named after the table and holds the row key", async (db, models) => {
    const meta = models["PostEvent"]._meta;
    assert.equal(meta.table, "hx_js_posts_events");
    for (const name of ["rowId", "label", "data", "createdAt", "xactId"]) assert.ok(meta.inputFields.has(name), name);
  });

  scenario("an insert an update and a delete write one event each", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a" });
    await Post.objects.using(db).filter(Post.id.eq(post.id)).update({ title: "b" });
    await Post.objects.using(db).filter(Post.id.eq(post.id)).delete();
    const entries = await timeline(post);
    assert.deepEqual(entries.map((e) => e.event.label), ["insert", "update", "delete"]);
    assert.deepEqual(entries.map((e) => e.event.data.title), ["a", "b", "b"]);
    assert.ok(entries.every((e) => e.diff === null));
  });

  scenario("the timeline with diff pairs each event with the previous one", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a", views: 1 });
    await Post.objects.using(db).filter(Post.id.eq(post.id)).update({ title: "b", views: 2 });
    const entries = await timeline(post, { diff: true });
    assert.equal(entries[0]!.diff, null);
    assert.deepEqual(entries[1]!.diff.title, { old: "a", new: "b" });
    assert.deepEqual(entries[1]!.diff.views, { old: 1, new: 2 });
  });

  scenario("an update that changes only updated at writes no event", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a" });
    await Post.objects.using(db).filter(Post.id.eq(post.id)).update({ changed: new Date("2030-01-01T00:00:00Z") });
    assert.deepEqual(await labels(post), ["insert"]);
  });

  scenario("events filter by label fields and when", async (db, models) => {
    const Note = models["Note"];
    const note = await Note.objects.using(db).insert({ title: "a", body: "x" });
    const rows = Note.objects.using(db).filter(Note.id.eq(note.id));
    await rows.update({ body: "y" });
    assert.deepEqual(await timeline(note), []);
    await rows.update({ title: "b" });
    const entries = await timeline(note);
    assert.deepEqual(entries.map((e) => e.event.label), ["title_changed"]);
    assert.deepEqual(entries[0]!.event.data, { title: "b" });
  });

  scenario("a composite key is one column per key field", async (db, models) => {
    const Line = models["Line"];
    const line = await Line.objects.using(db).insert({ orderId: 1, sku: "a", qty: 1 });
    await Line.objects.using(db).filter(Line.orderId.eq(1), Line.sku.eq("a")).update({ qty: 2 });
    const entries = await timeline(line, { diff: true });
    assert.deepEqual(entries.map((e) => e.event.label), ["insert", "update"]);
    assert.deepEqual(entries[1]!.diff, { qty: { old: 1, new: 2 } });
    assert.deepEqual(ids(await line.history.timeline([1, "a"])), ids(entries));
    assert.deepEqual(await line.history.timeline([1, "b"]), []);
  });

  scenario("a manual event and a backfill write events with the label", async (db, models) => {
    const Post = models["Post"];
    const [a, b] = await Post.objects.using(db).insertMany([{ title: "a" }, { title: "b" }]).returning();
    const manual = await a.history.event(a, "reviewed");
    assert.deepEqual([manual.label, manual.data.title], ["reviewed", "a"]);
    assert.equal(await a.history.backfill("snapshot"), 2);
    assert.deepEqual(await labels(a), ["insert", "reviewed", "snapshot"]);
    assert.deepEqual(await labels(b), ["insert", "snapshot"]);
    await assert.rejects(a.history.event(999, "reviewed"), /does not exist/);
  });

  scenario("at gives the last event at or before a time", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a" });
    await Post.objects.using(db).filter(Post.id.eq(post.id)).update({ title: "b" });
    const [first, second] = (await timeline(post)).map((e) => e.event);
    assert.equal((await post.history.at(post, new Date(Date.now() + 60_000))).id, second.id);
    assert.equal(await post.history.at(post, new Date(first.createdAt.getTime() - 86_400_000)), null);
  });

  scenario("restore updates a row and leaves one restore event", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a", body: "x" });
    await Post.objects.using(db).filter(Post.id.eq(post.id)).update({ title: "b", body: "y" });
    const first = (await timeline(post))[0]!.event;
    const restored = await post.history.restore(first);
    assert.deepEqual([restored.title, restored.body], ["a", "x"]);
    const entries = await timeline(post);
    assert.deepEqual(entries.map((e) => e.event.label), ["insert", "update", "restore"]);
    assert.equal(entries[2]!.event.data.title, "a");
  });

  scenario("a write after a restore in the same transaction gets its own label", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a" });
    await Post.objects.using(db).filter(Post.id.eq(post.id)).update({ title: "b" });
    const first = (await timeline(post))[0]!.event;
    await db.transaction(async () => {
      await post.history.restore(first);
      await Post.objects.using(db).filter(Post.id.eq(post.id)).update({ title: "c" });
    });
    assert.deepEqual(await labels(post), ["insert", "update", "restore", "update"]);
  });

  scenario("restore writes to the database that loaded the event", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a" });
    await Post.objects.using(db).filter(Post.id.eq(post.id)).update({ title: "b" });
    const first = (await timeline(post))[0]!.event;
    const restored = await post.history.using(undefined).restore(first); // no default database: the event knows its own
    assert.equal(restored.title, "a");
  });

  scenario("restore inserts a deleted row again", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a", views: 3 });
    await Post.objects.using(db).filter(Post.id.eq(post.id)).delete();
    const deleted = (await timeline(post)).at(-1)!.event;
    const restored = await post.history.restore(deleted);
    assert.deepEqual([restored.id, restored.title, restored.views], [post.id, "a", 3]);
    assert.deepEqual(await labels(post), ["insert", "delete", "restore"]);
  });

  scenario("restore gives dates and json back from the event data", async (db, models) => {
    const Typed = models["Typed"];
    const at = new Date("2030-01-02T03:04:05Z");
    const row = await Typed.objects.using(db).insert({ at, extra: { k: [1, "x"] } });
    await Typed.objects.using(db).filter(Typed.id.eq(row.id)).delete();
    const restored = await row.history.restore((await timeline(row)).at(-1)!.event);
    assert.equal(restored.at.getTime(), at.getTime());
    assert.deepEqual(restored.extra, { k: [1, "x"] });
    await Typed.objects.using(db).filter(Typed.id.eq(row.id)).update({ at: new Date(at.getTime() + 86400000) });
    const again = await row.history.restore((await timeline(row))[0]!.event);
    assert.equal(again.at.getTime(), at.getTime());
  });

  scenario("restore of a deleted row needs a tracker that covers every field", async (db, models) => {
    const Note = models["Note"];
    const note = await Note.objects.using(db).insert({ title: "a" });
    await Note.objects.using(db).filter(Note.id.eq(note.id)).update({ title: "b" });
    const event = (await timeline(note))[0]!.event;
    await Note.objects.using(db).filter(Note.id.eq(note.id)).delete();
    await assert.rejects(note.history.restore(event), /only some fields/);
    assert.equal(await Note.objects.using(db).filter(Note.id.eq(note.id)).count(), 0);
  });

  scenario("restore raises for a required field that the event lacks", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a" });
    await Post.objects.using(db).filter(Post.id.eq(post.id)).delete();
    const event = (await timeline(post)).at(-1)!.event;
    const Event = models["PostEvent"];
    await allowWrites([Event], () => Event.objects.using(db).filter(Event.id.eq(event.id)).update({ data: { id: post.id } }));
    const stripped = await Event.objects.using(db).filter(Event.id.eq(event.id)).first();
    await assert.rejects(post.history.restore(stripped), /required field/);
  });

  scenario("restore ignores event fields that the model no longer has", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a" });
    const event = (await timeline(post))[0]!.event;
    const Event = models["PostEvent"];
    await allowWrites([Event], () => Event.objects.using(db).filter(Event.id.eq(event.id)).update({ data: { ...event.data, dropped: 1 } }));
    const changed = await Event.objects.using(db).filter(Event.id.eq(event.id)).first();
    await Post.objects.using(db).filter(Post.id.eq(post.id)).update({ title: "b" });
    assert.equal((await post.history.restore(changed)).title, "a");
  });

  scenario("purge deletes the events before a time and only those", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a" });
    assert.equal(await post.history.purge({ before: new Date(Date.now() - 86_400_000) }), 0);
    assert.equal(await post.history.purge({ before: new Date(Date.now() + 86_400_000) }), 1);
    assert.deepEqual(await timeline(post), []);
  });

  scenario("the event model has no update or delete in the app", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a" });
    const event = (await timeline(post))[0]!.event;
    const Event = models["PostEvent"];
    await assert.rejects(Event.objects.using(db).filter(Event.id.eq(event.id)).delete(), WriteProtected);
    if (provider === "postgresql") {
      await assert.rejects(db.execute("UPDATE hx_js_posts_events SET label = 'forged'"), (e: Error) => e instanceof WriteProtected && /append_only/.test(e.message));
    }
  });

  scenario("events of one transaction share a transaction id on postgresql", async (db, models) => {
    const Post = models["Post"];
    const [a, b] = await db.transaction(async () => [await Post.objects.using(db).insert({ title: "a" }), await Post.objects.using(db).insert({ title: "b" })]);
    const events = [(await timeline(a))[0]!.event, (await timeline(b))[0]!.event];
    if (provider === "sqlite") {
      assert.deepEqual(events.map((e) => e.xactId), [null, null]);
    } else {
      assert.notEqual(events[0].xactId, null);
      assert.equal(events[0].xactId, events[1].xactId);
    }
  });

  scenario("a skip outside the bypass roles still writes events", async (db, models) => {
    const Post = models["Post"];
    const post = await Post.objects.using(db).insert({ title: "a" });
    const update = () => skipTriggers(["history"], () => Post.objects.using(db).filter(Post.id.eq(post.id)).update({ title: "b" }));
    if (provider === "sqlite") {
      await assert.rejects(update(), /sqlite has none/);
      return;
    }
    await update();
    assert.deepEqual(await labels(post), ["insert", "update"]);
  });

  scenario("a model with two trackers needs the tracker name", async (db, models) => {
    const Both = models["Both"];
    const both = await Both.objects.using(db).insert({ name: "a" });
    await Both.objects.using(db).filter(Both.id.eq(both.id)).delete();
    await assert.rejects(both.history.timeline(both), /several trackers/);
    assert.deepEqual((await both.history.timeline(both, { tracker: "BothEvent" })).map((e: Any) => e.event.label), ["insert", "delete"]);
    assert.deepEqual((await both.history.timeline(both, { tracker: "BothAudit" })).map((e: Any) => e.event.label), ["delete"]);
    await assert.rejects(both.history.timeline(both, { tracker: "Nope" }), /no tracker with the event model/);
    assert.equal(await both.history.purge({ before: new Date(Date.now() + 86_400_000) }), 3);
    void anyOf;
  });

  test("the scope values are stored on postgresql and null without a value", { skip }, () =>
    withSchema(provider, SCOPED, async (db, models) => {
      const Doc = models["Doc"];
      const a = await scope({ user: "ann" }, () => Doc.objects.using(db).insert({ title: "a" }));
      const b = await Doc.objects.using(db).insert({ title: "b" });
      const first = (await timeline(a))[0]!.event;
      const second = (await timeline(b))[0]!.event;
      assert.equal(second.user, null);
      assert.equal(first.user, provider === "sqlite" ? null : "ann");
    }));
}

test("a skip for a bypass role writes no event", { skip: skip || !process.env["ORM_TEST_DATABASE_URL"] }, () =>
  withSchema(
    "postgresql",
    SCHEMA,
    async (db, models) => {
      const Post = models["Post"];
      const post = await Post.objects.using(db).insert({ title: "a" });
      const rows = Post.objects.using(db).filter(Post.id.eq(post.id));
      await skipTriggers(["history"], () => rows.update({ title: "b" }));
      await rows.update({ title: "c" });
      assert.deepEqual(await labels(post), ["insert", "update"]);
    },
    '  bypass_roles = ["postgres"]\n',
  ));

test("a decimal keeps every digit in the event and in a restore on postgresql", { skip: skip || !process.env["ORM_TEST_DATABASE_URL"] }, () =>
  withSchema(
    "postgresql",
    'model Price {\n  id Int @id @default(autoincrement())\n  amount Decimal\n  @@history.track\n  @@map("hx_js_prices")\n}\n',
    async (db, models) => {
      const Price = models["Price"];
      const exact = "12345678901234567890.123456789";
      const price = await Price.objects.using(db).insert({ amount: new Decimal(exact) });
      await Price.objects.using(db).filter(Price.id.eq(price.id)).update({ amount: new Decimal("1") });
      const first = (await timeline(price))[0]!.event;
      assert.equal(first.data.amount, exact);
      const restored = await price.history.restore(first);
      assert.equal(String(restored.amount), exact);
    },
  ));

test("restore gives uuid, time and big integers back on postgresql", { skip: skip || !process.env["ORM_TEST_DATABASE_URL"] }, () =>
  withSchema(
    "postgresql",
    'model Wide {\n  id Int @id @default(autoincrement())\n  ref String @db.Uuid\n  at DateTime @db.Time\n  big BigInt\n  @@history.track\n  @@map("hx_js_wide")\n}\n',
    async (db, models) => {
      const Wide = models["Wide"];
      const ref = "0190a5c2-7b3e-7d2a-9c1f-1234567890ab";
      const row = await Wide.objects.using(db).insert({ ref, at: new Date("1970-01-01T13:14:15Z"), big: 1099511627776n });
      await Wide.objects.using(db).filter(Wide.id.eq(row.id)).delete();
      const restored = await row.history.restore((await timeline(row)).at(-1)!.event);
      assert.deepEqual([restored.ref, restored.at.getTime(), restored.big], [ref, row.at.getTime(), 1099511627776n]);
      const unsafe = await Wide.objects.using(db).insert({ ref, at: new Date("1970-01-01T01:00:00Z"), big: 9007199254740993n });
      await Wide.objects.using(db).filter(Wide.id.eq(unsafe.id)).delete();
      await assert.rejects(unsafe.history.restore((await timeline(unsafe)).at(-1)!.event), /above 2\^53/);
    },
  ));

test("when without the update event is an error", { skip }, () => {
  const source = `datasource db {\n  provider = "sqlite"\n}
model Thing {
  id Int @id @default(autoincrement())
  name String
  @@history.track(events: [insert], when: "new.name <> old.name")
  @@map("hx_js_things")
}
`;
  assert.throws(() => loads(source, { registry: new Registry() }), /when applies to update events/);
});

test("two trackers with the same trigger suffix are an error", { skip }, () => {
  const source = `datasource db {\n  provider = "sqlite"\n}
model Thing {
  id Int @id @default(autoincrement())
  name String
  @@history.track
  @@history.track(model: "Events")
  @@map("hx_js_things")
}
`;
  assert.throws(() => loads(source, { registry: new Registry() }), /already uses the trigger names/);
});

test("a composed model cannot be tracked", { skip }, () => {
  const source = `datasource db {\n  provider = "sqlite"\n}
model Base {
  id Int @id @default(autoincrement())
  name String
  @@map("hx_js_base")
}
model Child {
  extra String
  @@composition.model(parent: "Base", parentRef: "base", childRef: "child")
  @@history.track
  @@map("hx_js_child")
}
`;
  assert.throws(() => loads(source, { registry: new Registry() }), /composed model/);
});

test("two trackers with the same event model name are an error", { skip }, () => {
  const source = `datasource db {\n  provider = "sqlite"\n}
model Thing {
  id Int @id @default(autoincrement())
  name String
  @@history.track
  @@history.track
  @@map("hx_js_things")
}
`;
  assert.throws(() => loads(source, { registry: new Registry() }), /@@history.track/);
});

void QueryError;

const MIGRATION_V1 = 'datasource db {\n provider = "postgresql"\n}\nmodel Item {\n id Int @id @default(autoincrement())\n name String\n @@history.track\n @@map("hm_js_items")\n}\n';
const MIGRATION_V2 = MIGRATION_V1.replace("@@history.track", "@@history.track(events: [update], fields: [name])");
const MIGRATION_V3 = MIGRATION_V1.replace("@@history.track", '@@history.track\n @@history.track(model: "ItemAudit", events: [delete])');
const MIGRATION_V4 = MIGRATION_V1.replace(" @@history.track\n", "");

test("migrations create change add and drop the event table and triggers", { skip }, () => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-history-"));
  const registry = (source: string) => {
    const r = new Registry();
    loads(source, { registry: r });
    return r;
  };
  const plan = (source: string) => new Migrations(dir, registry(source)).plan();
  const make = (source: string) => new Migrations(dir, registry(source)).make();
  let up = plan(MIGRATION_V1).up.map((x) => x.sql);
  assert.ok(up.some((x) => x.startsWith('CREATE TABLE "hm_js_items_events"')));
  assert.ok(up.some((x) => x.startsWith('CREATE TRIGGER "h_events_ins" AFTER INSERT ON "hm_js_items"')));
  make(MIGRATION_V1);
  up = plan(MIGRATION_V2).up.map((x) => x.sql); // a tracker change replaces triggers; the event table stays
  assert.ok(up.some((x) => x.startsWith('DROP TRIGGER "h_events_ins"')));
  assert.ok(!up.some((x) => x.startsWith("DROP TABLE")));
  make(MIGRATION_V2);
  up = plan(MIGRATION_V3).up.map((x) => x.sql);
  assert.ok(up.some((x) => x.startsWith('CREATE TABLE "item_audit"')));
  make(MIGRATION_V3);
  const last = plan(MIGRATION_V4);
  assert.ok(last.up.some((x) => x.sql === 'DROP TABLE "hm_js_items_events"') && last.up.some((x) => x.sql === 'DROP TABLE "item_audit"'));
  assert.ok(last.down.some((x) => x.sql.startsWith('CREATE TABLE "hm_js_items_events"')));
});
