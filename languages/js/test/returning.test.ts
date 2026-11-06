/** What a write returns: the default selection, or the columns named in returning(...). */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, define, loads, NotLoaded, of, Registry } from "../src/index.js";

const COMPOSITE = `
datasource db {
  provider = "postgresql"
}
model ReturnItem {
  shop  BigInt
  id    BigInt
  title String
  note  String?
  @@id([shop, id])
  @@map("ret_item_js")
}
`;

async function open(dialect: "sqlite" | "postgres", defaults: boolean, annotated = false) {
  const registry = new Registry();
  const field = (name: string, type = "string", flags = {}) => ({ name, column: name, type, ...flags });
  // Runtime define intentionally has no generated static model types.
  const models = define({ dialect, models: [
      { name: "SelectedAuthor", table: "selection03_js_authors", fields: [
        field("name"), field("id", "int", { primary_key: true, auto_increment: true }),
        field("visible", "bool", { default: true }), field("bio", "text"), field("note", "string", { nullable: true }),
      ], relations: [{ name: "books", kind: "many", target: "SelectedBook", from: "id", to: "author_id" }] },
      { name: "SelectedBook", table: "selection03_js_books", fields: [field("title"), field("author_id", "int"), field("id", "int", { primary_key: true, auto_increment: true }), field("isbn", "string", { nullable: true })],
        relations: [{ name: "author", kind: "one", target: "SelectedAuthor", from: "author_id", to: "id", foreign_key: true, on_delete: "cascade" }] },
    ], behavior: defaults ? { schema_contract: 5, read_defaults: [
      { source: "monorm.query-defaults", name: "query", model: "SelectedAuthor", filter: { t: "col", path: [], name: "visible" }, fields: ["name", "note"] },
      { source: "monorm.query-defaults", name: "query", model: "SelectedBook", fields: ["title"], load: [["author"]] },
      ...(annotated ? [{ source: "test.counts", name: "counts", model: "SelectedAuthor", annotate: { bookCount: { t: "func", name: "count", rel: ["books"] } } }] : []),
    ] } : {} } as never, { registry });
  const Author = models["SelectedAuthor"] as any;
  const Book = models["SelectedBook"] as any;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables(); await db.createTables();
  return { db, Author, Book, a: Author.objects.using(db), b: Book.objects.using(db) };
}


const loaded = (row: any) => row.toJSON();
const notLoaded = (row: any, name: string) => assert.throws(() => row[name], NotLoaded);

for (const dialect of ["sqlite", "postgres"] as const) {
  async function run(body: (c: { Author: any; Book: any; a: any; b: any; db: any }) => Promise<void>) {
    const { db, Author, Book, a, b } = await open(dialect, true);
    try { await body({ Author, Book, a, b, db }); } finally { await db.dropTables(); await db.close(); }
  }

  test(`each write returns the default selection without columns (${dialect})`, () => run(async ({ Author, a }) => {
    const one = await a.insert({ name: "one", bio: "long", note: "n" }).returning();
    assert.deepEqual(loaded(one), { name: "one", note: "n" });
    const many = await a.insertMany([{ name: "two", bio: "x" }, { name: "three", bio: "y" }]).returning();
    assert.deepEqual(many.map(loaded), [{ name: "two", note: null }, { name: "three", note: null }]);
    const updated = await a.filter(Author.name.eq("one")).update({ note: "m" }).returning();
    assert.deepEqual(updated.map(loaded), [{ name: "one", note: "m" }]);
    const manyUpdated = await a.updateMany([{ id: one.pk, note: "k" }]).returning();
    assert.deepEqual(manyUpdated.map(loaded), [{ name: "one", note: "k" }]);
    const deleted = await a.filter(Author.name.eq("three")).delete().returning();
    assert.deepEqual(deleted.map(loaded), [{ name: "three", note: null }]);
    assert.equal(await a.filter(Author.name.eq("two")).update({ note: "c" }), 1);
    assert.deepEqual(loaded(await a.insert({ name: "plain", bio: "b" })), { name: "plain", note: null });
  }));

  test(`each write returns the named columns and the key (${dialect})`, () => run(async ({ Author, a }) => {
    const one = await a.insert({ name: "one", bio: "long" }).returning(Author.bio);
    assert.deepEqual(loaded(one), { bio: "long" }); assert.equal(one.pk, 1);
    const many = await a.insertMany([{ name: "two", bio: "x" }, { name: "three", bio: "y" }]).returning(Author.bio);
    assert.deepEqual(many.map(loaded), [{ bio: "x" }, { bio: "y" }]);
    const updated = await a.filter(Author.name.eq("one")).update({ note: "m" }).returning(Author.note);
    assert.deepEqual(updated.map(loaded), [{ note: "m" }]);
    const manyUpdated = await a.updateMany([{ id: one.pk, note: "k" }]).returning(Author.note, Author.bio);
    assert.deepEqual(manyUpdated.map(loaded), [{ note: "k", bio: "long" }]);
    const deleted = await a.filter(Author.name.eq("two")).delete().returning(Author.bio);
    assert.deepEqual(deleted.map(loaded), [{ bio: "x" }]);
    for (const row of [one, ...many, ...updated, ...manyUpdated, ...deleted]) notLoaded(row, "name");
  }));

  test(`a field hidden by the defaults is not loaded even when supplied (${dialect})`, () => run(async ({ a }) => {
    const row = await a.insert({ name: "hidden", bio: "supplied", visible: false }).returning();
    for (const name of ["bio", "visible"]) notLoaded(row, name);
    assert.equal(row.name, "hidden");
  }));

  test(`without defaults restores the hidden fields (${dialect})`, () => run(async ({ Author, a }) => {
    const row = await a.withoutDefaults().insert({ name: "all", bio: "supplied", visible: false }).returning();
    assert.deepEqual([row.bio, row.visible, row.id], ["supplied", false, 1]);
    const rows = await a.withoutDefaults().insertMany([{ name: "b", bio: "x" }]).returning();
    assert.deepEqual([rows[0].bio, rows[0].visible], ["x", true]);
    const changed = await a.withoutDefaults().filter(Author.name.eq("all")).update({ note: "n" }).returning();
    assert.equal(changed[0].bio, "supplied");
    const gone = await a.withoutDefaults().filter(Author.name.eq("all")).delete().returning();
    assert.equal(gone[0].visible, false);
  }));

  test(`the write statement returns only the selected columns (${dialect})`, () => run(async ({ Author, a, db }) => {
    const sql: string[] = [];
    db.onQuery((e: { sql: string }) => { sql.push(e.sql); });
    const columns = async (write: () => Promise<unknown>) => {
      sql.length = 0;
      await write();
      const writes = sql.filter((s) => s.includes("RETURNING"));
      assert.equal(writes.length, 1);
      const tail = writes[0]!.split("RETURNING")[1]!;
      return ["name", "note", "bio", "visible"].filter((name) => tail.includes(`"${name}"`));
    };
    assert.deepEqual(await columns(() => a.insert({ name: "one", bio: "long" })), ["name", "note"]);
    assert.deepEqual(await columns(() => a.insert({ name: "two", bio: "long" }).returning(Author.bio)), ["bio"]);
    assert.deepEqual(await columns(() => a.withoutDefaults().insert({ name: "three", bio: "long" })), ["name", "note", "bio", "visible"]);
    assert.deepEqual(await columns(() => a.filter(Author.name.eq("one")).update({ note: "m" }).returning(Author.note)), ["note"]);
    assert.deepEqual(await columns(() => a.filter(Author.name.eq("one")).delete().returning()), ["name", "note"]);
  }));

  test(`an explicit column wins over the defaults (${dialect})`, () => run(async ({ Author, a }) => {
    const row = await a.insert({ name: "x", bio: "b", visible: false }).returning(Author.visible);
    assert.deepEqual(loaded(row), { visible: false });
    notLoaded(row, "note");
    const rows = await a.withoutDefaults().filter(Author.id.eq(row.pk)).update({ bio: "c" }).returning(Author.name);
    assert.deepEqual(loaded(rows[0]), { name: "x" });
  }));

  test(`insertMany returns the rows in input order (${dialect})`, () => run(async ({ Author, a }) => {
    const names = [7, 6, 5, 4, 3, 2, 1].map((i) => `n${i}`);
    const rows = await a.insertMany(names.map((name) => ({ name, bio: "b" }))).returning(Author.name);
    assert.deepEqual(rows.map((r: any) => r.name), names);
    const keys = rows.map((r: any) => r.pk);
    assert.deepEqual(keys, [...keys].sort((x: number, y: number) => x - y));
  }));

  test(`upsert returns the named columns and skipped rows stay out (${dialect})`, () => run(async ({ Author, a }) => {
    const first = await a.insert({ name: "u", bio: "1" }).returning(Author.bio);
    assert.deepEqual(loaded(first), { bio: "1" });
    const skipped = await a.insert({ id: first.pk, name: "u2", bio: "2" }).onConflict(Author.id, { update: false }).returning(Author.bio);
    assert.equal(skipped, null);
    const updated = await a.insert({ id: first.pk, name: "u3", bio: "3" }).onConflict(Author.id, { update: true }).returning(Author.bio);
    assert.deepEqual(loaded(updated), { bio: "3" }); assert.equal(updated.pk, first.pk);
    const rows = await a.insertMany([{ id: first.pk, name: "x", bio: "b" }, { name: "y", bio: "4" }])
      .onConflict([Author.id], { update: false }).returning(Author.bio);
    assert.deepEqual(rows.map(loaded), [{ bio: "4" }]);
  }));

  test(`updateMany returns the matched rows (${dialect})`, () => run(async ({ Author, a }) => {
    const rows = await a.insertMany([{ name: "a", bio: "b" }, { name: "b", bio: "b" }]).returning();
    const changed = await a.updateMany(rows.map((r: any) => ({ id: r.pk, note: `n${r.pk}` }))).returning(Author.note);
    assert.deepEqual(changed.map((r: any) => r.note).sort(), rows.map((r: any) => `n${r.pk}`).sort());
    assert.equal(await a.updateMany([{ id: rows[0].pk, note: "z" }]), 1);
  }));

  test(`load on a write raises and names returning (${dialect})`, () => run(async ({ Author, a }) => {
    const row = await a.insert({ name: "x", bio: "b" });
    const calls = [
      () => a.load(Author.bio).insert({ name: "y", bio: "b" }),
      () => a.load(Author.bio).insertMany([{ name: "y", bio: "b" }]),
      () => a.load(Author.bio).filter(Author.id.eq(row.pk)).update({ note: "n" }),
      () => a.load(Author.bio).filter(Author.id.eq(row.pk)).delete(),
      () => a.load(Author.bio).updateMany([{ id: row.pk, note: "n" }]),
      () => a.load().insert({ name: "y", bio: "b" }),
    ];
    for (const call of calls) await assert.rejects(async () => await call(), { name: "TypeError", message: /returning\(/ });
  }));

  test(`a relation in returning raises and names of load (${dialect})`, () => run(async ({ Author, Book, a, b }) => {
    const calls = [
      () => a.insert({ name: "y", bio: "b" }).returning(Author.books),
      () => b.insert({ title: "t", authorId: 1 }).returning(Book.author),
      () => a.insertMany([{ name: "y", bio: "b" }]).returning(Author.books),
      () => a.filter(Author.id.eq(1)).update({ note: "n" }).returning(Author.books),
      () => a.filter(Author.id.eq(1)).delete().returning(Author.books),
      () => a.updateMany([{ id: 1, note: "n" }]).returning(Author.books),
      () => a.insert({ name: "y", bio: "b" }).returning(Book.title),
      () => a.insert({ name: "y", bio: "b" }).returning(Author.books.title),
    ];
    for (const call of calls) assert.throws(call, { name: "TypeError", message: /of\(rows\)\.load\(/ });
  }));

  test(`a returned key batch loads a relation afterwards (${dialect})`, () => run(async ({ Author, Book, a, b }) => {
    const people = await a.insertMany([{ name: "p", bio: "b" }]).returning(Author.id);
    const books = await b.insertMany([{ title: "t", authorId: people[0].pk }]).returning(Book.authorId);
    await of(books).load(Book.author);
    assert.equal(books[0].author.name, "p");
  }));
}

test("a copy insert keeps the count and refuses returning", { skip: !process.env["ORM_TEST_DATABASE_URL"] }, async () => {
  const { db, Author, a } = await open("postgres", true);
  try {
    assert.equal(await a.insertMany([{ name: "c", bio: "b" }], { copy: true }), 1);
    const copy = a.insertMany([{ name: "d", bio: "b" }], { copy: true });
    assert.equal(typeof (copy as any).returning, "undefined");
    assert.equal(await copy, 1);
    assert.ok(Author);
  } finally { await db.dropTables(); await db.close(); }
});

test("a write with columns keeps composite key fields", async () => {
  const registry = new Registry();
  const models = loads(COMPOSITE, { registry }) as any;
  const db = await connect(process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test", { registry, default: false });
  const Item = models["ReturnItem"];
  const items = Item.objects.using(db);
  try {
    await db.dropTables(); await db.createTables();
    const row = await items.insert({ shop: 1, id: 2, title: "t", note: "n" }).returning(Item.title);
    assert.deepEqual(loaded(row), { title: "t" });
    assert.deepEqual(row.pk, { shop: 1n, id: 2n });
    const rows = await items.insertMany([{ shop: 1, id: 3, title: "u" }]).returning(Item.note);
    assert.deepEqual(loaded(rows[0]), { note: null }); assert.deepEqual(rows[0].pk, { shop: 1n, id: 3n });
    const changed = await items.filter(Item.id.eq(3)).update({ title: "v" }).returning(Item.title);
    assert.deepEqual(loaded(changed[0]), { title: "v" }); assert.deepEqual(changed[0].pk, { shop: 1n, id: 3n });
    const batched = await items.updateMany([{ shop: 1, id: 3, title: "w" }]).returning(Item.title);
    assert.deepEqual(loaded(batched[0]), { title: "w" }); assert.deepEqual(batched[0].pk, { shop: 1n, id: 3n });
    const gone = await items.filter(Item.id.eq(3)).delete().returning(Item.note);
    assert.deepEqual(loaded(gone[0]), { note: null }); assert.deepEqual(gone[0].pk, { shop: 1n, id: 3n });
  } finally { await db.dropTables(); await db.close(); }
});
