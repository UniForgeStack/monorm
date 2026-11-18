/**
 * Every built-in model extension in one schema: the schema loads, and reads, writes and history rows work together.
 * The loader picks the extensions that a schema uses, so a test with one or two of them cannot see a clash between others.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, loads, Registry, scope, ScopeMismatch, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const SCHEMA = `
model Doc {
  id Int @id @default(autoincrement())
  shop_id Int
  title String
  hidden Boolean @default(false)
  @@scope.by(shop: shop_id)
  @@soft_delete.deleted_at("deleted")
  @@timestamps.updated_at("changed")
  @@locking.version("version")
  @@history.track
  @@query.defaults(filter: hidden == false)
  @@map("cx_js_docs")
}
model Note {
  id Int @id @default(autoincrement())
  name String
  @@soft_delete.deleted_at("deleted")
  @@map("cx_js_notes")
}
model Tag {
  id Int @id @default(autoincrement())
  shop_id Int
  name String
  @@scope.by(shop: shop_id)
  @@history.track
  @@map("cx_js_tags")
}
`;

const skip = !hasExtension("monorm.scope", "monorm.soft-delete", "monorm.history", "monorm.timestamps", "monorm.locking", "monorm.query-defaults");
const urlOf = (provider: string) => (provider === "sqlite" ? "sqlite://:memory:" : (process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test"));

async function withWorld(provider: string, body: (db: Database, models: Record<string, Any>) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${SCHEMA}`, { registry }) as Record<string, Any>;
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

const titles = async (query: Any) => ((await query) as Any[]).map((r) => r.title).sort();
const timeline = async (row: Any) => (await row.history.timeline(row)) as { event: Any }[];

for (const provider of ["sqlite", "postgresql"]) {
  const scenario = (what: string, body: (db: Database, models: Record<string, Any>) => Promise<void>) => test(what, { skip }, () => withWorld(provider, body));

  scenario("one schema with every model extension loads", async (_db, models) => {
    const meta = models["Doc"]._meta;
    for (const name of ["deleted", "changed", "version"]) assert.ok(meta.fields.has(name) || meta.inputFields.has(name), name);
    assert.ok(models["DocEvent"] && models["TagEvent"]);
  });

  scenario("a read applies the scope the soft delete and the query default filter", async (db, models) => {
    const Doc = models["Doc"];
    await scope({ shop: 1 }, () => Doc.objects.using(db).insertMany([{ title: "a" }, { title: "b" }, { title: "hidden", hidden: true }]));
    await scope({ shop: 2 }, () => Doc.objects.using(db).insert({ title: "other shop" }));
    await scope({ shop: 1 }, async () => {
      assert.deepEqual(await titles(Doc.objects.using(db)), ["a", "b"]);
      await Doc.objects.using(db).filter(Doc.title.eq("b")).delete();
      assert.deepEqual(await titles(Doc.objects.using(db)), ["a"]);
      // `allWithDeleted` lifts the soft-delete filter only: the scope and the query default stay.
      assert.deepEqual(await titles(Doc.objects.using(db).allWithDeleted()), ["a", "b"]);
      // `withoutDefaults("query")` lifts the query default only: the scope and the soft-delete filter stay.
      assert.deepEqual(await titles(Doc.objects.using(db).withoutDefaults("query")), ["a", "hidden"]);
    });
    await scope({ shop: 2 }, async () => assert.deepEqual(await titles(Doc.objects.using(db)), ["other shop"]));
  });

  scenario("a write takes the scope stamps the row and bumps the version", async (db, models) => {
    const Doc = models["Doc"];
    await scope({ shop: 1 }, async () => {
      const doc: Any = await Doc.objects.using(db).insert({ title: "a" });
      assert.deepEqual([doc.shopId, doc.version, doc.deleted], [1, 0, null]);
      assert.ok(doc.changed !== null && doc.changed !== undefined);
      await Doc.objects.using(db).filter(Doc.id.eq(doc.id)).update({ title: "b" });
      const [again] = (await Doc.objects.using(db)) as Any[];
      assert.deepEqual([again.title, again.version], ["b", 1]);
      await assert.rejects(Doc.objects.using(db).insert({ shopId: 2, title: "x" }), (e: Any) => e instanceof ScopeMismatch);
    });
    await scope({ shop: 2 }, async () => assert.deepEqual(await titles(Doc.objects.using(db)), []));
  });

  scenario("updates and a soft delete write history rows of the scoped model", async (db, models) => {
    const Doc = models["Doc"];
    await scope({ shop: 1 }, async () => {
      const doc: Any = await Doc.objects.using(db).insert({ title: "a" });
      await Doc.objects.using(db).filter(Doc.id.eq(doc.id)).update({ title: "b" });
      await Doc.objects.using(db).filter(Doc.id.eq(doc.id)).delete();
      assert.deepEqual(await titles(Doc.objects.using(db)), []);
      const entries = await timeline(doc);
      assert.deepEqual(entries.map((e) => e.event.label), ["insert", "update", "update"]);
      assert.deepEqual(entries.map((e) => e.event.data.title), ["a", "b", "b"]);
      assert.notEqual(entries[entries.length - 1]!.event.data.deleted, null);
    });
  });

  scenario("models with one extension each keep only their own filter", async (db, models) => {
    const { Note, Tag } = models;
    await Note.objects.using(db).insert({ name: "kept" });
    const gone: Any = await Note.objects.using(db).insert({ name: "gone" });
    await Note.objects.using(db).filter(Note.id.eq(gone.id)).delete();
    assert.deepEqual(((await Note.objects.using(db)) as Any[]).map((n) => n.name), ["kept"]);
    await scope({ shop: 1 }, () => Tag.objects.using(db).insertMany([{ name: "x" }]));
    await scope({ shop: 2 }, async () => {
      const tag: Any = await Tag.objects.using(db).insert({ name: "y" });
      assert.deepEqual(((await Tag.objects.using(db)) as Any[]).map((t) => t.name), ["y"]);
      assert.deepEqual((await timeline(tag)).map((e) => e.event.label), ["insert"]);
    });
  });
}
