/** `onConflict(columns, { index })`: the unique schema indexes name the predicate of the conflict target. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { Column, connect, IndexRef, loads, QueryError, Registry, type Database, type QueryEvent } from "../src/index.js";

const source = `
model Note {
  id    BigInt  @id @default(autoincrement())
  slug  String
  body  String  @default("")
  live  Boolean @default(true)
  shown Boolean @default(true)
  @@index([slug], unique: true, where: raw("live"), map: "notes_slug_live")
  @@index([slug], unique: true, where: raw("shown"), map: "notes_slug_shown")
  @@index([body], unique: true, map: "notes_body")
  @@index([body, slug], map: "notes_body_slug")
  @@map("oci_note")
}

model Solo {
  id     BigInt  @id @default(autoincrement())
  slug   String
  body   String  @default("")
  active Boolean @default(true)
  @@index([slug], unique: true, where: raw("active"), map: "solo_slug_active")
  @@map("oci_solo")
}
`;

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const urls: Record<string, string | undefined> = { sqlite: "sqlite://:memory:", postgresql: process.env["ORM_TEST_DATABASE_URL"] };

async function setup(provider: string): Promise<{ db: Database; Note: Any; Solo: Any; events: QueryEvent[] }> {
  const registry = new Registry();
  const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${source}`, { registry }) as Record<string, Any>;
  const db = await connect(urls[provider]!, { registry, default: false, maxConnections: 1 });
  await db.dropTables();
  await db.createTables();
  const events: QueryEvent[] = [];
  db.onQuery((e) => events.push(e));
  return { db, Note: models["Note"], Solo: models["Solo"], events };
}

for (const provider of Object.keys(urls)) {
  const skip = !urls[provider];

  test(`Model.indexes offers the unique indexes (${provider})`, { skip }, async () => {
    const { db, Note, Solo } = await setup(provider);
    try {
      assert.deepEqual(Object.keys(Note.indexes), ["notes_slug_live", "notes_slug_shown", "notes_body"]);
      assert.ok(Solo.indexes.solo_slug_active instanceof IndexRef);
      assert.equal(String(Solo.indexes.solo_slug_active), "Solo.indexes.solo_slug_active");
      assert.equal(Note.indexes.notes_body_slug, undefined);
    } finally {
      await db.close();
    }
  });

  test(`onConflict index picks a partial index (${provider})`, { skip }, async () => {
    const { db, Note, events } = await setup(provider);
    try {
      const notes = Note.objects.using(db);
      const first = await notes.insert({ slug: "a", body: "x" });
      const index = Note.indexes.notes_slug_live;
      events.length = 0;
      assert.equal(await notes.insert({ slug: "a", body: "it's" }).onConflict(Note.slug, { index, update: false }).returning(), null);
      // The target carries the schema predicate; the user value stays a parameter.
      const sql = events.find((e) => e.sql.includes("ON CONFLICT"))!.sql;
      assert.ok(sql.includes('ON CONFLICT ("slug") WHERE live DO NOTHING') && !sql.includes("it's"), sql);
      const again = await notes.insert({ slug: "a", body: "y" }).onConflict(Note.slug, { index, update: true, updateValues: { body: "z" } }).returning();
      assert.equal(again.id, first.id);
      assert.equal(again.body, "z");
      // Rows outside the index predicate are not unique.
      await notes.insertMany([0, 1].map((i) => ({ slug: "a", body: `b${i}`, live: false, shown: false })));
      assert.equal(await notes.count(), 3);
    } finally {
      await db.close();
    }
  });

  test(`onConflict index takes a boolean column and a list of one (${provider})`, { skip }, async () => {
    // The predicate is SQL text: a parameter would stop matching the index from the
    // sixth run of the prepared statement, when Postgres plans it generically.
    const { db, Solo } = await setup(provider);
    try {
      const solos = Solo.objects.using(db);
      for (let i = 0; i < 8; i++) {
        await solos.insert({ slug: "a", body: `b${i}` }).onConflict(Solo.slug, { index: Solo.indexes.solo_slug_active, update: true, updateFields: [Solo.body] });
      }
      assert.deepEqual((await solos).map((s: Any) => s.body), ["b7"]);
      const rows = Array.from({ length: 8 }, (_, i) => ({ slug: `s${i % 2}`, body: "x" }));
      await solos.insertMany(rows, { batchSize: 1 }).onConflict(Solo.slug, { index: [Solo.indexes.solo_slug_active], update: false });
      assert.equal(await solos.count(), 3);
    } finally {
      await db.close();
    }
  });

  test(`onConflict index errors name the problem (${provider})`, { skip }, async () => {
    const { db, Note, Solo } = await setup(provider);
    try {
      const notes = Note.objects.using(db);
      await assert.rejects(notes.insert({ slug: "a" }).onConflict(Note.slug, { index: Solo.indexes.solo_slug_active, update: false }), /takes Note.indexes/);
      await assert.rejects(notes.insert({ slug: "a" }).onConflict(Note.slug, { index: "notes_slug_live", update: false } as never), /takes Note.indexes/);
      // A forged reference reaches the engine, which checks the schema.
      const forged = (name: string) => new IndexRef("Note", name);
      await assert.rejects(notes.insert({ slug: "a", body: "b" }).onConflict(Note.body, { index: forged("notes_body_slug"), update: false }), (e) => e instanceof QueryError && /not unique/.test(e.message));
      await assert.rejects(notes.insert({ slug: "a" }).onConflict(Note.slug, { index: Note.indexes.notes_body, update: false }), (e) => e instanceof QueryError && /exactly the conflict columns/.test(e.message));
      await assert.rejects(notes.insert({ slug: "a" }).onConflict(Note.slug, { index: forged("nope"), update: false }), (e) => e instanceof QueryError && /not an index of Note/.test(e.message));
      await assert.rejects(notes.insert({ slug: "a" }).onConflict(Note.slug, { update: false }), (e) => e instanceof QueryError && /notes_slug_live, notes_slug_shown/.test(e.message));
    } finally {
      await db.close();
    }
  });
}

test("onConflict index list sends the AND of the predicates (postgresql)", { skip: !urls["postgresql"] }, async () => {
  const { db, Note, events } = await setup("postgresql");
  try {
    const notes = Note.objects.using(db);
    const both = [Note.indexes.notes_slug_live, Note.indexes.notes_slug_shown];
    await notes.insertMany([
      { slug: "a", body: "a", shown: false },
      { slug: "b", body: "b", live: false },
    ]);
    // A row live and shown conflicts with a row that is live (but hidden) and with one shown (but not live).
    events.length = 0;
    assert.equal(await notes.insertMany([{ slug: "a", body: "c" }, { slug: "b", body: "d" }]).onConflict(Note.slug, { index: both, update: false }), 0);
    const sql = events.find((e) => e.sql.includes("ON CONFLICT"))!.sql;
    assert.ok(sql.includes('ON CONFLICT ("slug") WHERE (live) AND (shown) DO NOTHING') && !sql.includes("ON CONSTRAINT"), sql);
    const rows = await notes
      .insertMany([{ slug: "a", body: "e" }, { slug: "b", body: "f" }])
      .onConflict(Note.slug, { index: both, update: true, updateFields: [Note.body] })
      .returning();
    assert.deepEqual(rows.map((r: Any) => r.body).sort(), ["e", "f"]);
    assert.deepEqual((await notes).map((n: Any) => n.body).sort(), ["e", "f"]);
  } finally {
    await db.dropTables();
    await db.close();
  }
});

test("onConflict index list is an error on sqlite", async () => {
  const { db, Note } = await setup("sqlite");
  try {
    const notes = Note.objects.using(db);
    await assert.rejects(
      notes.insert({ slug: "a" }).onConflict(Note.slug, { index: [Note.indexes.notes_slug_live, Note.indexes.notes_slug_shown], update: false }),
      (e) => e instanceof QueryError && /SQLite takes one index/.test(e.message),
    );
  } finally {
    await db.close();
  }
});

test("a field named indexes keeps its column", () => {
  const shadow = `datasource db {\n  provider = "sqlite"\n}\nmodel Shadow {\n  id      Int    @id\n  indexes String\n  @@index([indexes], unique: true, map: "shadow_indexes")\n}`;
  const { Shadow } = loads(shadow, { registry: new Registry() }) as Record<string, Any>;
  assert.ok(Shadow.indexes instanceof Column);
});
