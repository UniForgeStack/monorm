import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, define, IntegrityError, loads, NotLoaded, Registry } from "../src/index.js";
import { native } from "../src/native.js";

const queryDefaults = (JSON.parse(native().nativeArtifact()) as { capabilities?: string[] }).capabilities?.includes("query-defaults") ?? false;

async function open(dialect: "sqlite" | "postgres", defaults: boolean) {
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
    ], behavior: defaults ? { schema_contract: 1, query_defaults: [
      { model: "SelectedAuthor", filter: { t: "col", path: [], name: "visible" }, fields: ["name", "note"] },
      { model: "SelectedBook", fields: ["title"], related: [["author"]] },
    ] } : {} } as never, { registry });
  const Author = models["SelectedAuthor"] as any;
  const Book = models["SelectedBook"] as any;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables(); await db.createTables();
  return { db, Author, Book, a: Author.objects.using(db), b: Book.objects.using(db) };
}

for (const dialect of ["sqlite", "postgres"] as const) {
  test(`partial model shapes: ${dialect}`, async () => {
    const { db, Author, Book, a, b } = await open(dialect, false);
    try {
      const author = await a.insert({ name: "ann", bio: "large", note: null });
      await a.insert({ name: "bob", bio: "large" });
      await b.insert({ title: "one", authorId: author.pk });
      const partial = await a.load(Author.name, Author.note).orderBy(Author.id).first();
      assert.deepEqual(partial.toJSON(), { name: "ann", note: null });
      assert.equal(partial.pk, author.pk);
      assert.throws(() => partial.bio, NotLoaded);
      assert.throws(() => a.load(Author.name, Author.name), TypeError);
      await partial.update({ name: "changed" }); assert.deepEqual(partial.toJSON(), { name: "changed", note: null });
      await partial.refresh(Author.bio); assert.deepEqual(partial.toJSON(), { name: "changed", note: null, bio: "large" });
      const names: string[] = [];
      for await (const o of a.load(Author.name).iterate(1)) names.push(o.name);
      assert.deepEqual(names, ["changed", "bob"]);
      assert.equal((await partial.books.using(db).get()).title, "one");
      const full = await a.load().orderBy(Author.id).first();
      assert.equal(Object.getPrototypeOf(full), Author._meta.Row.prototype); assert.equal(full.bio, "large");
      const book = await b.load(Book.title).get(); assert.throws(() => book.author, NotLoaded);
      assert.equal((await b.load(Book.author).load(Book.title).get()).author.name, "changed");
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`schema query defaults: ${dialect}`, async (t) => {
    if (!queryDefaults) {
      t.skip("needs a native build with the query-defaults feature");
      return;
    }
    const { db, Author, Book, a, b } = await open(dialect, true);
    try {
      await assert.rejects(a.insert({ name: "missing" }), /bio/);
      const visible = await a.insert({ name: "visible", bio: "large", note: null });
      const hidden = await a.insert({ name: "hidden", bio: "large", visible: false });
      await b.insert({ title: "one", authorId: visible.pk });
      await b.insert({ title: "two", authorId: hidden.pk });
      const partial = await a.get();
      assert.deepEqual(partial.toJSON(), { name: "visible", note: null });
      assert.equal(partial.pk, visible.pk);
      for (const name of ["id", "bio", "visible"]) assert.throws(() => partial[name], NotLoaded);
      assert.equal(a.sql().includes('"bio"'), false);
      assert.equal(await a.count(), 1); assert.equal(await a.exists(), true);
      assert.equal(await a.withoutDefaults().count(), 2);
      assert.equal(await a.withoutDefaults().filter(Author.name.eq("visible")).count(), 1);
      assert.equal((await a.load().get()).bio, "large");
      const explicit = await a.load(Author.note, Author.name).get();
      await explicit.refresh(); assert.deepEqual(explicit.toJSON(), { note: null, name: "visible" });
      await explicit.refresh(Author.bio); assert.deepEqual(explicit.toJSON(), { name: "visible", note: null, bio: "large" });
      await partial.update({ name: "changed" }); assert.deepEqual(partial.toJSON(), { name: "changed", note: null });
      const joined = await b.orderBy(Book.id).all();
      assert.equal(joined.length, 2); assert.equal(joined[0].author.name, "changed"); assert.equal(joined[1].author, null);
      assert.deepEqual(joined[0].toJSON(), { title: "one" });
      assert.deepEqual(joined[0].author.toJSON(), { name: "changed", note: null });
      assert.throws(() => joined[0].authorId, NotLoaded);
      await joined[1].update({ authorId: visible.pk });
      assert.throws(() => joined[1].author, NotLoaded);
      await joined[1].update({ authorId: hidden.pk });
      const bypassed = await b.withoutDefaults().load(Book.author).orderBy(Book.id).all();
      assert.equal(bypassed[1].author.name, "hidden"); assert.equal(bypassed[1].author.visible, false);
      const partialReturn = await a.load(Author.name).update({ name: "again" }, { returning: true });
      assert.deepEqual(partialReturn[0].toJSON(), { name: "again" });
      const cleared = await b.withoutRelated().first(); assert.throws(() => cleared.author, NotLoaded);
      const prefetched = await a.load(Author.books).get(); assert.equal(prefetched.books.cached[0].title, "one");
      const unfiltered = await a.withoutDefaults().load(Author.books).filter(Author.id.eq(visible.pk)).get();
      assert.equal(unfiltered.books.cached[0].authorId, visible.pk);
      assert.equal((await a.inBulk()).get(visible.pk).name, "again");
      const written = await a.load().update({ visible: false }, { returning: true });
      assert.equal(written.length, 1); assert.equal(written[0].visible, false); assert.equal(await a.count(), 0);
      assert.equal(await a.withoutDefaults().updateMany([{ id: hidden.pk, name: "bulk" }]), 1);
      assert.equal(await a.updateMany([{ id: hidden.pk, name: "excluded" }]), 0);
      assert.equal(await a.delete(), 0);
      assert.equal(await a.withoutDefaults().filter(Author.id.eq(hidden.pk)).delete(), 1);
    } finally { await db.dropTables(); await db.close(); }
  });
}

const ordered = `
datasource db {
  provider = "sqlite"
}
model Topic {
  id Int @id @default(autoincrement())
  rank Int?
  name String
  notes Note[]
  @@map("order09_js_topics")
  @@query.order("-rank nulls last", "id")
}
model Note {
  id Int @id @default(autoincrement())
  topic_id Int
  body String
  topic Topic @relation(fields: [topic_id], references: [id], onDelete: Cascade)
  @@map("order09_js_notes")
  @@query.defaults(order: ["-body"])
}
`;

for (const dialect of ["sqlite", "postgres"] as const) {
  test(`schema default order: ${dialect}`, async (t) => {
    if (!queryDefaults) {
      t.skip("needs a native build with the query-defaults feature");
      return;
    }
    const registry = new Registry();
    const models = loads(ordered.replace('"sqlite"', dialect === "postgres" ? '"postgresql"' : '"sqlite"'), { registry }) as any;
    const { Topic, Note } = models;
    const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
    const db = await connect(url, { registry, default: false });
    await db.dropTables(); await db.createTables();
    const tq = Topic.objects.using(db), nq = Note.objects.using(db);
    const names = (rows: any[]) => rows.map((r) => r.name);
    try {
      for (const [name, rank] of [["a", 1], ["b", null], ["c", 3], ["d", 3]] as const) await tq.insert({ name, rank });
      assert.deepEqual(names(await tq.all()), ["c", "d", "a", "b"]);
      assert.equal((await tq.first()).name, "c");
      assert.equal((await tq.last()).name, "b");
      assert.equal((await tq.prepare().first()).name, "c");
      assert.deepEqual(names(await tq.orderBy(Topic.name.desc())), ["d", "c", "b", "a"]);
      assert.deepEqual(names(await tq.orderBy(Topic.rank.asc({ nulls: "first" }), Topic.id)), ["b", "a", "c", "d"]);
      assert.ok(!tq.withoutDefaults().sql().includes("ORDER BY"));
      assert.equal((await tq.withoutDefaults().first()).name, "a");
      assert.equal(await tq.count(), 4);
      const batches: string[][] = [];
      for await (const batch of tq.batches(2)) batches.push(names(batch));
      assert.deepEqual(batches, [["a", "b"], ["c", "d"]]);
      const page = await tq.paginate({ first: 3 });
      assert.deepEqual(names(page.items), ["c", "d", "a"]);
      assert.deepEqual(names((await tq.paginate({ first: 3, after: page.nextCursor })).items), ["b"]);
      assert.deepEqual(names((await tq.withoutDefaults().paginate({ first: 3 })).items), ["a", "b", "c"]);
      const c = await tq.get(Topic.name.eq("c"));
      for (const body of ["x", "z", "y"]) await nq.insert({ topicId: c.pk, body });
      assert.deepEqual((await nq.all()).map((x: any) => x.body), ["z", "y", "x"]);
      const loaded = await tq.load(Topic.notes).get(Topic.name.eq("c"));
      assert.deepEqual((await loaded.notes).map((x: any) => x.body), ["z", "y", "x"]);
      const own = await tq.load(Topic.notes.objects.orderBy(Note.body)).get(Topic.name.eq("c"));
      assert.deepEqual((await own.notes).map((x: any) => x.body), ["x", "y", "z"]);
      const sliced = await tq.load(Topic.notes.objects.limit(2)).get(Topic.name.eq("c"));
      assert.deepEqual((await sliced.notes).map((x: any) => x.body), ["z", "y"]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}

test("schema default order: one source per option", (t) => {
  if (!queryDefaults) {
    t.skip("needs a native build with the query-defaults feature");
    return;
  }
  const both = ordered.replace('@@query.defaults(order: ["-body"])', '@@query.defaults(order: ["-body"])\n  @@query.order("id")');
  assert.throws(() => loads(both, { registry: new Registry() }), /Note: order is set by both @@query\.defaults\(order:\) at .* and @@query\.order at/);
  assert.throws(() => loads(ordered.replace('"-body"', '"nope"'), { registry: new Registry() }), /order column "nope" is not a field/);
});


for (const dialect of ["sqlite", "postgres"] as const) {
  test(`load() of a path with columns unites the default selection: ${dialect}`, async (t) => {
    if (!queryDefaults) {
      t.skip("needs a native build with the query-defaults feature");
      return;
    }
    const { db, Author, Book, a, b } = await open(dialect, true);
    try {
      const ann = await a.insert({ name: "ann", bio: "large", note: "n" });
      await b.insertMany([{ title: "one", authorId: ann.pk, isbn: "i1" }]);
      for (const items of [[Book.author, Book.author.bio], [Book.author.bio, Book.author]]) {
        const book = await b.load(...items).get();
        assert.deepEqual(book.author.toJSON(), { name: "ann", note: "n", bio: "large" });
      }
      const mixed = await b.load(Book.author, Book.author.objects.load(Author.bio)).get();
      assert.deepEqual(mixed.author.toJSON(), { name: "ann", note: "n", bio: "large" });
      for (const items of [[Author.books, Author.books.isbn], [Author.books.isbn, Author.books]]) {
        const author = await a.load(...items).get();
        assert.deepEqual((await author.books).map((x: any) => x.toJSON()), [{ title: "one", isbn: "i1" }]);
      }
      assert.deepEqual((await b.load(Book.author.bio).get()).author.toJSON(), { bio: "large" });
      assert.deepEqual(((await (await a.load(Author.books.isbn).get()).books) as any[]).map((x) => x.toJSON()), [{ isbn: "i1" }]);
    } finally { await db.dropTables(); await db.close(); }
  });
}

const unsetSource = `
datasource db {
  provider = "sqlite"
}
model UAuthor {
  id Int @id
  name String
  active Boolean @default(true)
  books UBook[]
  profile UProfile?
  @@map("g2_js_authors")
  //FILTER
}
model UBook {
  id Int @id
  author_id Int?
  author UAuthor? @relation(fields: [author_id], references: [id])
  @@map("g2_js_books")
}
model UProfile {
  id Int @id
  author_id Int @unique
  author UAuthor @relation(fields: [author_id], references: [id])
  @@map("g2_js_profiles")
}
`;

async function openUnset(dialect: "sqlite" | "postgres", filtered: boolean) {
  const registry = new Registry();
  const source = unsetSource.replace("//FILTER", filtered ? '@@query.filter("active == true")' : "").replace('"sqlite"', dialect === "postgres" ? '"postgresql"' : '"sqlite"');
  const models = loads(source, { registry }) as any;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables(); await db.createTables();
  if (dialect === "sqlite") await db.execute("PRAGMA foreign_keys = OFF");
  else {
    await db.execute("ALTER TABLE g2_js_books DROP CONSTRAINT IF EXISTS g2_js_books_author_id_fkey");
    await db.execute("ALTER TABLE g2_js_profiles DROP CONSTRAINT IF EXISTS g2_js_profiles_author_id_fkey");
  }
  const { UAuthor: Author, UBook: Book, UProfile: Profile } = models;
  await Author.objects.using(db).withoutDefaults().insertMany([{ id: 1, name: "ann", active: true }, { id: 2, name: "bob", active: false }, { id: 3, name: "cy", active: true }]);
  // Book 1 has an author, 2 a dangling key, 3 a NULL key, 4 a hidden author.
  await Book.objects.using(db).insertMany([{ id: 1, authorId: 1 }, { id: 2, authorId: 99 }, { id: 3, authorId: null }, { id: 4, authorId: 2 }]);
  await Profile.objects.using(db).insertMany([{ id: 1, authorId: 1 }, { id: 2, authorId: 99 }, { id: 3, authorId: 2 }]);
  return { db, Author, Book, Profile };
}

const reference = (book: any) => { try { return book.author; } catch (e) { if (e instanceof NotLoaded) return "unset"; throw e; } };

for (const dialect of ["sqlite", "postgres"] as const) {
  test(`an unfiltered dangling key stays not loaded after a join, a prefetch and load(): ${dialect}`, async () => {
    const { db, Book } = await openUnset(dialect, false);
    try {
      const books = Book.objects.using(db).orderBy(Book.id);
      const joined = await books.load(Book.author).all();
      assert.deepEqual([1, 2].map((i) => reference(joined[i])), ["unset", null]);
      const prefetched = await books.load(Book.author.objects.asPrefetch()).all();
      assert.equal(reference(prefetched[0]).name, "ann");
      assert.deepEqual([1, 2].map((i) => reference(prefetched[i])), ["unset", null]);
      const book = await Book.objects.using(db).get(Book.id.eq(2));
      await book.load(Book.author.objects.asPrefetch());
      assert.equal(reference(book), "unset");
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a labelled prefetch and a has-one keep null for no row: ${dialect}`, async () => {
    const { db, Author, Book } = await openUnset(dialect, false);
    try {
      const books = await Book.objects.using(db).orderBy(Book.id).load(Book.author.objects.asPrefetch().label("writer")).all();
      assert.equal(books[1].writer, null);
      const ann = await Author.objects.using(db).orderBy(Author.id).load(Author.profile).get(Author.id.eq(3));
      assert.equal(ann.profile, null);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a filtered target is null and a bypassed prefetch is not loaded: ${dialect}`, async (t) => {
    if (!queryDefaults) { t.skip("needs a native build with the query-defaults feature"); return; }
    const { db, Book } = await openUnset(dialect, true);
    try {
      const prefetched = await Book.objects.using(db).orderBy(Book.id).load(Book.author.objects.asPrefetch()).all();
      assert.deepEqual([1, 2, 3].map((i) => reference(prefetched[i])), [null, null, null]);
      const bypassed = await Book.objects.using(db).withoutDefaults().orderBy(Book.id).load(Book.author.objects.asPrefetch()).all();
      assert.deepEqual([1, 2].map((i) => reference(bypassed[i])), ["unset", null]);
      assert.equal(reference(bypassed[3]).name, "bob");
    } finally { await db.dropTables(); await db.close(); }
  });
}


for (const dialect of ["sqlite", "postgres"] as const) {
  test(`adapter guards for required and nullable dangling keys: ${dialect}`, async () => {
    const { db, Book, Profile } = await openUnset(dialect, false);
    try {
      for (const Model of [Book, Profile]) {
        for (const path of [Model.author, Model.author.objects.asPrefetch()]) {
          const row = await Model.objects.using(db).load(path).get(Model.id.eq(2));
          assert.throws(() => row.author, NotLoaded);
        }
      }
      const required = await Profile.objects.using(db).get(Profile.id.eq(2));
      await assert.rejects(required.load(Profile.author), IntegrityError);
      const nullable = await Book.objects.using(db).get(Book.id.eq(2));
      await nullable.load(Book.author);
      assert.equal(nullable.author, null); // F2: current behavior pending an owner decision.
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a missing reference is null only when the engine filtered the join: ${dialect}`, async (t) => {
    if (!queryDefaults) { t.skip("needs a native build with the query-defaults feature"); return; }
    const { db, Book, Profile } = await openUnset(dialect, true);
    try {
      for (const Model of [Book, Profile]) {
        const rows = Model.objects.using(db).orderBy(Model.id);
        for (const path of [Model.author, Model.author.objects.asPrefetch()]) {
          const loaded = await rows.load(path).all();
          assert.deepEqual(loaded.slice(1).map((row: any) => row.author), Model === Book ? [null, null, null] : [null, null]);
        }
        const [first, dangling] = await rows.withoutDefaults().load(Model.author).all();
        assert.equal(first.author.name, "ann");
        assert.throws(() => dangling.author, NotLoaded);
      }
    } finally { await db.dropTables(); await db.close(); }
  });
}
