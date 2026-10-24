/** Inferred targets prefer a plain unique index to partial indexes. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, QueryError, Registry, type QueryEvent } from "../src/index.js";

const urls: Record<string, string | undefined> = { sqlite: "sqlite://:memory:", postgresql: process.env["ORM_TEST_DATABASE_URL"] };

async function setup(provider: string) {
  const registry = new Registry();
  const models = loads(`datasource db { provider = "${provider}" }
    model Mixed {
      id BigInt @id @default(autoincrement())
      slug String
      active Boolean @default(true)
      @@index([slug], unique: true, where: raw("active"), map: "mixed_active")
      @@index([slug], unique: true, map: "mixed_plain")
      @@map("oci_mixed_inference")
    }
    model Partial {
      id BigInt @id @default(autoincrement())
      slug String
      active Boolean @default(true)
      shown Boolean @default(true)
      @@index([slug], unique: true, where: raw("active"), map: "partial_active")
      @@index([slug], unique: true, where: raw("shown"), map: "partial_shown")
      @@map("oci_partial_inference")
    }
    model Single {
      id BigInt @id @default(autoincrement())
      slug String
      active Boolean @default(true)
      @@index([slug], unique: true, where: raw("active"), map: "single_active")
      @@map("oci_single_inference")
    }`, { registry }) as Record<string, any>;
  const db = await connect(urls[provider]!, { registry, default: false });
  await db.createTables();
  const events: QueryEvent[] = [];
  db.onQuery((e) => events.push(e));
  const close = async () => { await db.dropTables(); await db.close(); };
  return { db, models, events, close };
}

for (const provider of Object.keys(urls)) {
  const skip = !urls[provider];

  test(`inference plain plus partial insert (${provider})`, { skip }, async () => {
    const { db, models, events, close } = await setup(provider);
    try {
      const Mixed = models["Mixed"];
      const rows = Mixed.objects.using(db);
      const first = await rows.insert({ slug: "a", active: false });
      assert.equal(await rows.insert({ slug: "a" }).onConflict(Mixed.slug, { update: false }), 0);
      const updated = await rows.insert({ slug: "a", active: true }).onConflict(Mixed.slug, { update: true }).returning();
      assert.equal(updated.id, first.id);
      assert.equal(updated.active, true);
      assert.equal(await rows.insertMany([{ slug: "a" }, { slug: "b" }]).onConflict(Mixed.slug, { update: false }), 1);
      const sql = events.filter((e) => e.sql.includes("ON CONFLICT"));
      assert.equal(sql.length, 3);
      assert.ok(sql.every((e) => e.sql.includes('ON CONFLICT ("slug") DO ')));
      assert.equal(await rows.count(), 2);
    } finally { await close(); }
  });

  test(`inference plain plus partial getOrInsert (${provider})`, { skip }, async () => {
    const { db, models, events, close } = await setup(provider);
    try {
      const rows = models["Mixed"].objects.using(db);
      const [row, created] = await rows.getOrInsert({ slug: "a" }, { defaults: { active: false } });
      const [again, createdAgain] = await rows.getOrInsert({ slug: "a" });
      assert.deepEqual([created, createdAgain, again.id, row.active], [true, false, row.id, false]);
      const sql = events.find((e) => e.sql.includes("ON CONFLICT"))!.sql;
      assert.ok(sql.includes('ON CONFLICT ("slug") DO NOTHING'), sql);
      assert.equal(await rows.count(), 1);
    } finally { await close(); }
  });

  test(`inference two partial indexes raise (${provider})`, { skip }, async () => {
    const { db, models, events, close } = await setup(provider);
    try {
      const Partial = models["Partial"];
      const rows = Partial.objects.using(db);
      const check = (e: unknown) => e instanceof QueryError && /matches 2 unique indexes of Partial: partial_active, partial_shown/.test(e.message);
      await assert.rejects(rows.insert({ slug: "a" }).onConflict(Partial.slug, { update: false }), check);
      await assert.rejects(rows.getOrInsert({ slug: "a" }), check);
      assert.ok(!events.some((e) => e.sql.includes("INSERT")));
      assert.equal(await rows.count(), 0);
    } finally { await close(); }
  });

  test(`inference single partial keeps predicate (${provider})`, { skip }, async () => {
    const { db, models, events, close } = await setup(provider);
    try {
      const Single = models["Single"];
      const rows = Single.objects.using(db);
      await rows.insertMany([{ slug: "a", active: false }, { slug: "a", active: false }]);
      await rows.insert({ slug: "a" }).onConflict(Single.slug, { update: false });
      assert.equal(await rows.insert({ slug: "a" }).onConflict(Single.slug, { update: false }), 0);
      const [row, created] = await rows.getOrInsert({ slug: "b" });
      const [again, createdAgain] = await rows.getOrInsert({ slug: "b" });
      assert.deepEqual([created, createdAgain, again.id], [true, false, row.id]);
      const sql = events.filter((e) => e.sql.includes("ON CONFLICT"));
      assert.equal(sql.length, 3);
      assert.ok(sql.every((e) => e.sql.includes('ON CONFLICT ("slug") WHERE active DO NOTHING')));
      assert.equal(await rows.count(), 4);
    } finally { await close(); }
  });
}
