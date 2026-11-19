import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, define, func, IntegrityError, loads, NotLoaded, of, param, Registry } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

const queryDefaults = hasExtension("monorm.query-defaults");

async function open(dialect: "sqlite" | "postgres", defaults: boolean, annotated = false, shelf = false) {
  const registry = new Registry();
  const field = (name: string, type = "string", flags = {}) => ({ name, column: name, type, ...flags });
  // Runtime define intentionally has no generated static model types.
  const models = define({ dialect, models: [
      { name: "SelectedAuthor", table: "selection03_js_authors", fields: [
        field("name"), field("id", "int", { primary_key: true, auto_increment: true }),
        field("visible", "bool", { default: true }), field("bio", "text"), field("note", "string", { nullable: true }),
      ], relations: [{ name: "books", kind: "many", target: "SelectedBook", from: "id", to: "author_id" }] },
      { name: "SelectedBook", table: "selection03_js_books", fields: [field("title"), field("author_id", "int"), field("id", "int", { primary_key: true, auto_increment: true }), field("isbn", "string", { nullable: true }), ...(shelf ? [field("shelf_id", "int", { nullable: true })] : [])],
        relations: [{ name: "author", kind: "one", target: "SelectedAuthor", from: "author_id", to: "id", foreign_key: true, on_delete: "cascade" }] },
      ...(shelf ? [{ name: "SelectedShelf", table: "selection03_js_shelves", fields: [field("id", "int", { primary_key: true, auto_increment: true })],
        relations: [{ name: "books", kind: "many", target: "SelectedBook", from: "id", to: "shelf_id" }] }] : []),
    ], behavior: defaults ? { schema_contract: 5, read_defaults: [
      { source: "monorm.query-defaults", name: "query", model: "SelectedAuthor", filter: { t: "col", path: [], name: "visible" }, fields: ["name", "note"] },
      { source: "monorm.query-defaults", name: "query", model: "SelectedBook", fields: ["title"], load: [["author"]] },
      ...(annotated ? [{ source: "test.counts", name: "counts", model: "SelectedAuthor", annotate: { bookCount: { t: "func", name: "count", rel: ["books"] } } }] : []),
    ] } : {} } as never, { registry });
  const Author = models["SelectedAuthor"] as any;
  const Book = models["SelectedBook"] as any;
  const Shelf = models["SelectedShelf"] as any;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables(); await db.createTables();
  return { db, Author, Book, Shelf, a: Author.objects.using(db), b: Book.objects.using(db) };
}

test("currentFilter lists filters with their bypass state", async () => {
  const { db, Author, Book } = await open("sqlite", true);
  try {
    const visible = { t: "col", path: [], name: "visible" };
    assert.deepEqual(Author.objects.currentFilter(), [{ source: "monorm.query-defaults", name: "query", expr: visible, active: true }]);
    assert.equal(Author.objects.withoutDefaults("query").currentFilter()[0].active, false);
    assert.equal(Author.objects.withoutDefaults().currentFilter()[0].active, false);
    assert.deepEqual(Book.objects.currentFilter(), []);
    assert.throws(() => Author.objects.withoutDefaults("nope").currentFilter(), /unknown default "nope"; available defaults: \["query"\]/);
  } finally { await db.dropTables(); await db.close(); }
});

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
      // Loaded fields are plain stores: the prototype has no NotLoaded accessor for them.
      assert.equal(Object.getOwnPropertyDescriptor(Object.getPrototypeOf(partial), "name"), undefined);
      assert.deepEqual(Object.getOwnPropertyDescriptor(partial, "name"), { value: "ann", writable: true, enumerable: true, configurable: true });
      partial.bio = "set"; assert.equal(partial.bio, "set"); delete partial.bio;
      assert.throws(() => partial.bio, NotLoaded);
      assert.throws(() => a.load(Author.name, Author.name), TypeError);
      await partial.update({ name: "changed" }); assert.deepEqual(partial.toJSON(), { name: "changed", note: null });
      await partial.load(Author.bio, { reload: true }); assert.deepEqual(partial.toJSON(), { name: "changed", note: null, bio: "large" });
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

  test(`an update of a hidden key drops the cached relation: ${dialect}`, async () => {
    const { db, Author, Book, a, b } = await open(dialect, false);
    try {
      await a.insertMany([{ name: "ann", bio: "x" }, { name: "bob", bio: "x" }]);
      const authors = await a.orderBy(Author.id);
      await b.insert({ title: "one", authorId: authors[0].pk });
      const book = await b.load(Book.title, Book.author).get();
      assert.equal(book.author.name, "ann");
      assert.throws(() => book.authorId, NotLoaded);
      await book.update({ authorId: authors[1].pk });
      assert.throws(() => book.author, NotLoaded);
      assert.equal((await book.load(Book.author)).author.name, "bob");
    } finally { await db.dropTables(); await db.close(); }
  });

  // case: selection_defaults::schema_query_defaults
  test(`schema query defaults: ${dialect}`, async (t) => {
    if (!queryDefaults) {
      t.skip("needs the monorm.query-defaults package");
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
      assert.equal(await a.withoutDefaults("query").count(), 2);
      assert.equal(await a.withoutDefaults("query").withoutDefaults("query").count(), 2);
      assert.equal(await a.withoutDefaults("query").withoutDefaults().count(), 2);
      await assert.rejects(a.withoutDefaults("nope").count(), /unknown default "nope"; available defaults: \["query"\]/);
      assert.equal((await a.load().get()).bio, "large");
      let explicit = await a.load(Author.note, Author.name).get();
      assert.deepEqual(explicit.toJSON(), { name: "visible", note: null });
      await explicit.load({ reload: true }); assert.deepEqual(explicit.toJSON(), { id: visible.pk, name: "visible", note: null, bio: "large", visible: true });
      explicit = await a.load(Author.note, Author.name).get();
      await explicit.load(Author.note, Author.name, { reload: true }); assert.deepEqual(explicit.toJSON(), { note: null, name: "visible" });
      await explicit.load(Author.bio, { reload: true }); assert.deepEqual(explicit.toJSON(), { name: "visible", note: null, bio: "large" });
      await partial.update({ name: "changed" }); assert.deepEqual(partial.toJSON(), { name: "changed", note: null });
      const joined = await b.orderBy(Book.id).all();
      assert.equal(joined.length, 2); assert.equal(joined[0].author.name, "changed"); assert.equal(joined[1].author, null);
      assert.deepEqual(joined[0].toJSON(), { title: "one" });
      assert.deepEqual(joined[0].author.toJSON(), { name: "changed", note: null });
      assert.throws(() => joined[0].authorId, NotLoaded);
      await joined[1].update({ authorId: visible.pk });
      assert.throws(() => joined[1].author, NotLoaded);
      await joined[1].update({ authorId: hidden.pk });
      assert.equal(await b.filter(Book.author.name.eq("hidden")).updateMany([{ id: joined[1].pk, title: "t" }]), 0);
      const bypassed = await b.withoutDefaults().load(Book.author).orderBy(Book.id).all();
      assert.equal(bypassed[0].author.name, "changed"); assert.equal(bypassed[0].author.visible, true);
      assert.equal(bypassed[1].author.name, "hidden"); assert.equal(bypassed[1].author.visible, false);
      for (const named of [false, true]) {
        for (const filtered of [false, true]) {
          const books = await b.withoutDefaults().orderBy(Book.id).all();
          let bound = named ? of(books).withoutDefaults("query") : of(books).withoutDefaults();
          if (filtered) bound = bound.filter(Book.id.gt(0));
          await bound.load(Book.author);
          assert.deepEqual(books.map((book: any) => book.author.name), ["changed", "hidden"]);
        }
      }
      const hiddenBooks = await b.withoutDefaults().orderBy(Book.id).all();
      await of(hiddenBooks).load(Book.author);
      assert.equal(hiddenBooks[0].author.name, "changed"); assert.equal(hiddenBooks[1].author, null);
      const partialReturn = await a.all().update({ name: "again" }).returning(Author.name);
      assert.deepEqual(partialReturn[0].toJSON(), { name: "again" });
      const defaultReturn = await a.filter(Author.id.eq(visible.pk)).update({ note: "n" }).returning();
      assert.deepEqual(defaultReturn[0].toJSON(), { name: "again", note: "n" });
      const cleared = await b.withoutRelated().first(); assert.throws(() => cleared.author, NotLoaded);
      const prefetched = await a.load(Author.books).get(); assert.equal(prefetched.books.cached[0].title, "one");
      const unfiltered = await a.withoutDefaults().load(Author.books).filter(Author.id.eq(visible.pk)).get();
      assert.equal(unfiltered.books.cached[0].authorId, visible.pk);
      assert.equal((await a.inBulk()).get(visible.pk).name, "again");
      assert.equal(await a.filter(Author.books.title.eq("one")).count(), 1);
      const written = await a.all().update({ visible: false }).returning(Author.visible);
      assert.equal(written.length, 1); assert.equal(written[0].visible, false); assert.equal(await a.count(), 0);
      assert.equal(await a.withoutDefaults().updateMany([{ id: hidden.pk, name: "bulk" }]), 1);
      assert.equal(await a.updateMany([{ id: hidden.pk, name: "excluded" }]), 0);
      assert.equal(await a.all().delete(), 0);
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
  // case: selection_defaults::schema_default_order
  test(`schema default order: ${dialect}`, async (t) => {
    if (!queryDefaults) {
      t.skip("needs the monorm.query-defaults package");
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
      assert.equal((await tq.at(1)).name, "d");
      assert.equal((await tq.at(-1)).name, "b");
      assert.deepEqual(names(await tq.orderBy(Topic.name.desc())), ["d", "c", "b", "a"]);
      assert.deepEqual(names(await tq.orderBy(Topic.rank.asc({ nulls: "first" }), Topic.id)), ["b", "a", "c", "d"]);
      assert.ok(!tq.withoutDefaults().sql().includes("ORDER BY"));
      assert.ok(!tq.filter(Topic.id.gt(0)).withoutDefaults().sql().includes("ORDER BY"));
      assert.equal((await tq.withoutDefaults().first()).name, "a");
      assert.equal(await tq.count(), 4); assert.equal(await tq.exists(), true);
      const batches: string[][] = [];
      for await (const batch of tq.batches(2)) batches.push(names(batch));
      assert.deepEqual(batches, [["a", "b"], ["c", "d"]]);
      const page = await tq.paginate({ first: 3 });
      assert.deepEqual(names(page.items), ["c", "d", "a"]);
      assert.deepEqual(names((await tq.paginate({ first: 3, after: page.nextCursor })).items), ["b"]);
      assert.deepEqual(names((await tq.withoutDefaults().paginate({ first: 3 })).items), ["a", "b", "c"]);
      assert.equal((await tq.from(tq.cte("all_topics")).first()).name, "a");
      // Postgres rejects UPDATE ... ORDER BY, so these also show that writes take no default order.
      assert.equal(await tq.filter(Topic.name.eq("none")).update({ name: "x" }), 0);
      assert.equal(await tq.filter(Topic.name.eq("none")).delete(), 0);
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

for (const dialect of ["sqlite", "postgres"] as const) {
  // case: selection_defaults::default_annotation_keeps_model_instances
  test(`a default annotation stays an instance read: ${dialect}`, async (t) => {
    if (!queryDefaults) {
      t.skip("needs the monorm.query-defaults package");
      return;
    }
    const { db, Author, a, b } = await open(dialect, true, true);
    try {
      const ann = await a.insert({ name: "ann", bio: "large", note: null });
      await a.insert({ name: "bob", bio: "large" });
      await b.insert({ title: "one", authorId: ann.pk });
      await b.insert({ title: "two", authorId: ann.pk });
      const rows = await a.orderBy(Author.name).all();
      assert.deepEqual(rows.map((r: any) => Number(r.bookCount)), [2, 0]);
      // A TypeScript row has no class: the instance methods tell a model row from a projection row.
      assert.deepEqual(rows.map((r: any) => typeof r.update), ["function", "function"]);
      assert.equal(rows[0].pk, ann.pk);
      assert.deepEqual(rows[0].toJSON(), { name: "ann", note: null });
      assert.throws(() => rows[0].bio, NotLoaded);
      const prefetched = await a.load(Author.books).orderBy(Author.name).first();
      assert.equal(Number(prefetched.bookCount), 2);
      assert.deepEqual((await a.withoutDefaults().orderBy(Author.name).all()).map((r: any) => r.name), ["ann", "bob"]);
      assert.equal((await a.withoutDefaults().orderBy(Author.name).first()).bookCount, undefined);
      const explicit = await a.load(func.count(Author.books).label("bookCount")).orderBy(Author.name).first();
      assert.equal(Number(explicit.bookCount), 2);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a default annotation stays out of projections: ${dialect}`, async (t) => {
    if (!queryDefaults) {
      t.skip("needs the monorm.query-defaults package");
      return;
    }
    const { db, Author, a, b } = await open(dialect, true, true);
    try {
      const ann = await a.insert({ name: "ann", bio: "large", note: null });
      await b.insert({ title: "one", authorId: ann.pk });
      await b.insert({ title: "two", authorId: ann.pk });
      const grouped = await a.select({ id: Author.id, n: func.count() }).groupBy(Author.id).all();
      assert.deepEqual(grouped.map((r: any) => [r.id, Number(r.n)]), [[ann.pk, 1]]);
      assert.equal(await a.count(), 1);
      const distinct = await a.select({ name: Author.name }).distinct().all();
      assert.deepEqual(distinct.map((r: any) => r.name), ["ann"]);
      assert.equal(a.select({ name: Author.name }).sql().includes("COUNT"), false);
    } finally { await db.dropTables(); await db.close(); }
  });
}

for (const dialect of ["sqlite", "postgres"] as const) {
  test(`a default annotation applies to prefetched rows: ${dialect}`, async (t) => {
    if (!queryDefaults) {
      t.skip("needs the monorm.query-defaults package");
      return;
    }
    const { db, Author, Book, a, b } = await open(dialect, true, true);
    try {
      const ann = await a.insert({ name: "ann", bio: "large", note: null });
      await b.insert({ title: "one", authorId: ann.pk });
      await b.insert({ title: "two", authorId: ann.pk });
      const books = await b.withoutRelated().load(Book.author.objects.asPrefetch()).orderBy(Book.title).all();
      assert.equal(Number(books[0].author.bookCount), 2);
      const node = await b.withoutRelated().load(Book.author.objects.asPrefetch().withoutDefaults("counts")).orderBy(Book.title).all();
      assert.equal(node[0].author.bookCount, undefined);
      const parent = await b.withoutDefaults().load(Book.author.objects.asPrefetch()).orderBy(Book.title).all();
      assert.equal(parent[0].author.bookCount, undefined);
    } finally { await db.dropTables(); await db.close(); }
  });
}

// case: selection_defaults::schema_default_order_one_source_per_option
test("schema default order: one source per option", (t) => {
  if (!queryDefaults) {
    t.skip("needs the monorm.query-defaults package");
    return;
  }
  const both = ordered.replace('@@query.defaults(order: ["-body"])', '@@query.defaults(order: ["-body"])\n  @@query.order("id")');
  assert.throws(() => loads(both, { registry: new Registry() }), /Note: order is set by both @@query\.defaults\(order:\) at .* and @@query\.order at/);
  assert.throws(() => loads(ordered.replace('"-body"', '"nope"'), { registry: new Registry() }), /order column "nope" is not a field/);
  assert.throws(() => loads(ordered.replace('"-rank nulls last"', '"-rank"'), { registry: new Registry() }), /Topic: order column "rank" can be NULL: write "-rank nulls first" or "-rank nulls last"/);
  assert.throws(() => loads(ordered.replace('"-rank nulls last", "id"', "-name"), { registry: new Registry() }), /elsewhere quote it: "-name"/);
  assert.throws(() => loads(ordered.replace("  rank Int?\n", "  rank Int?\n  k Json @db.Json\n").replace('"-rank nulls last", "id"', '"k"').replace('"sqlite"', '"postgresql"'), { registry: new Registry() }), /order column "k" is json, which the database can't order/);
});


for (const dialect of ["sqlite", "postgres"] as const) {
  test(`load() of a path with columns unites the default selection: ${dialect}`, async (t) => {
    if (!queryDefaults) {
      t.skip("needs the monorm.query-defaults package");
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

async function openUnset(dialect: "sqlite" | "postgres", filtered: boolean, composite = false, targetLast = false) {
  const registry = new Registry();
  let source = unsetSource.replace("//FILTER", filtered ? '@@query.filter("active", active == true)' : "").replace('"sqlite"', dialect === "postgres" ? '"postgresql"' : '"sqlite"');
  if (composite) {
    source = source.replace("id Int @id\n  name", "id Int\n  region Int @default(1)\n  @@id([id, region])\n  name")
      .replace("author_id Int?", "author_region Int? @default(1)\n  author_id Int?")
      .replace("author_id Int @unique", "author_region Int @default(1)\n  @@unique([author_id, author_region])\n  author_id Int @unique")
      .replaceAll("fields: [author_id], references: [id]", "fields: [author_id, author_region], references: [id, region]");
  }
  if (targetLast) {
    const start = source.indexOf("model UAuthor"), end = source.indexOf("model UBook");
    source = source.slice(0, start) + source.slice(end) + source.slice(start, end);
  }
  const models = loads(source, { registry }) as any;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables(); await db.createTables();
  if (dialect === "sqlite") await db.execute("PRAGMA foreign_keys = OFF");
  else {
    await db.execute("ALTER TABLE g2_js_books DROP CONSTRAINT IF EXISTS g2_js_books_author_id_fkey");
    await db.execute("ALTER TABLE g2_js_profiles DROP CONSTRAINT IF EXISTS g2_js_profiles_author_id_fkey");
  }
  if (composite && dialect === "postgres") {
    for (const table of ["g2_js_books", "g2_js_profiles"]) await db.execute(`ALTER TABLE ${table} DROP CONSTRAINT IF EXISTS ${table}_author_id_author_region_fkey`);
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
  for (const method of ["instance", "join", "prefetch"] as const) {
    test(`a reference target declared last supports ${method}: ${dialect}`, async () => {
      const { db, Book, Profile } = await openUnset(dialect, false, false, true);
      try {
        for (const Model of [Book, Profile]) {
          let rows = Model.objects.using(db);
          if (method !== "instance") rows = rows.load(method === "join" ? Model.author : Model.author.objects.asPrefetch());
          const row = await rows.get(Model.id.eq(1));
          if (method === "instance") {
            assert.throws(() => row.author, NotLoaded);
            await row.load(Model.author);
          }
          assert.equal(row.author.name, "ann");
          // The getter must still reject a cached target after the source key changes.
          row.authorId = 3;
          assert.throws(() => row.author, NotLoaded);
          await row.load(Model.author);
          assert.equal(row.author.name, "cy");
        }
      } finally { await db.dropTables(); await db.close(); }
    });
  }

  test(`the target flag does not depend on model order: ${dialect}`, async (t) => {
    if (!queryDefaults) { t.skip("needs the monorm.query-defaults package"); return; }
    const { db, Book } = await openUnset(dialect, true, false, true);
    try {
      const books = Book.objects.using(db).orderBy(Book.id);
      for (const path of [Book.author, Book.author.objects.asPrefetch()]) {
        const loaded = await books.load(path).all();
        assert.equal(loaded[0].author.name, "ann");
        assert.deepEqual(loaded.slice(1).map((row: any) => row.author), [null, null, null]);
      }
    } finally { await db.dropTables(); await db.close(); }
  });
}

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
    if (!queryDefaults) { t.skip("needs the monorm.query-defaults package"); return; }
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
    if (!queryDefaults) { t.skip("needs the monorm.query-defaults package"); return; }
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


for (const dialect of ["sqlite", "postgres"] as const) for (const composite of [false, true]) for (const filtered of [false, true]) for (const method of ["join", "prefetch", "instance"]) {
  test(`absent reference message tracks the loaded key (${dialect}, ${composite}, ${filtered}, ${method})`, async (t) => {
    if (filtered && !queryDefaults) { t.skip("needs query-defaults"); return; }
    const { db, Book, Profile } = await openUnset(dialect, filtered, composite);
    const specific = (error: unknown) => error instanceof NotLoaded && error.message.includes("the key is set and no row of the target has it");
    const generic = (error: unknown) => error instanceof NotLoaded && !error.message.includes("the key is set and no row of the target has it");
    try {
      for (const Model of [Book, Profile]) {
        let rows = Model.objects.using(db).withoutDefaults();
        if (method !== "instance") rows = rows.load(method === "join" ? Model.author : Model.author.objects.asPrefetch());
        const loaded = await rows.get(Model.id.eq(2));
        if (method === "instance") await loaded.load(Model.author.objects.withoutDefaults().asPrefetch(), { reload: true });
        assert.throws(() => loaded.author, specific);
        loaded[composite ? "authorRegion" : "authorId"] = composite ? 2 : 98;
        assert.throws(() => loaded.author, generic);
        const row = await Model.objects.using(db).get(Model.id.eq(2));
        assert.throws(() => row.author, generic);
        await row.load(Model.author.objects.withoutDefaults().asPrefetch(), { reload: true });
        assert.throws(() => row.author, specific);
        await row.update({ authorId: 3 });
        assert.throws(() => row.author, generic);
        await row.load(Model.author.objects.asPrefetch(), { reload: true });
        assert.equal(row.author.name, "cy");
      }
      const row = await Book.objects.using(db).withoutDefaults().load(Book.author).get(Book.id.eq(3));
      assert.equal(row.author, null);
      if (filtered) {
        const hidden = await Book.objects.using(db).load(Book.author).get(Book.id.eq(4));
        assert.equal(hidden.author, null);
      }
    } finally { await db.dropTables(); await db.close(); }
  });
}

for (const dialect of ["sqlite", "postgres"] as const) {
  test(`one row missing and required: ${dialect}`, async () => {
    const registry = new Registry();
    const models = loads(ordered.replace('@@query.defaults(order: ["-body"])', "").replace('@@query.order("-rank nulls last", "id")', "").replace('"sqlite"', dialect === "postgres" ? '"postgresql"' : '"sqlite"'), { registry }) as any;
    const { Topic } = models;
    const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
    const db = await connect(url, { registry, default: false });
    try {
      await db.dropTables(); await db.createTables();
      const t = Topic.objects.using(db);
      await t.insertMany([["c", 3], ["b", 2], ["a", 1]].map(([name, rank]) => ({ name, rank })));
      assert.equal(await t.at(param("i")).prepare().run({ i: 99 }), null);
      assert.equal(await t.at(99), null);
      assert.equal(await t.filter(Topic.name.eq("missing")).first(), null);
      await assert.rejects(t.require().at(99), Topic.DoesNotExist);
      await assert.rejects(t.filter(Topic.name.eq("missing")).require().last(), Topic.DoesNotExist);
      const lazy = t.require().at(5);
      await new Promise((resolve) => setTimeout(resolve, 10));
      await assert.rejects(lazy, Topic.DoesNotExist);
      await assert.rejects(t.require().filter(Topic.name.eq(param("name"))).first().prepare().run({ name: "missing" }), Topic.DoesNotExist);
    } finally { await db.dropTables(); await db.close(); }
  });
}

test("unknown bypass name lists the defaults of the model", async () => {
  const { db, Author, a } = await open("sqlite", false);
  try {
    await assert.rejects(a.withoutDefaults("nope").count(), /unknown default "nope"; available defaults: \[\]/);
    await assert.rejects(a.withoutDefaults("nope").filter(Author.name.eq("x")).update({ name: "y" }), /unknown default "nope"/);
    await assert.rejects(a.withoutDefaults("nope").filter(Author.name.eq("x")).delete(), /unknown default "nope"/);
    assert.equal(await a.withoutDefaults("nope").withoutDefaults().count(), 0);
  } finally { await db.dropTables(); await db.close(); }
});

// -- twins of the Python selection-defaults tests ---

test("a joined model brings its related default", async (t) => {
  if (!queryDefaults) return t.skip("needs the monorm.query-defaults package");
  const registry = new Registry();
  const field = (name: string, type = "string", flags = {}) => ({ name, column: name, type, ...flags });
  const rel = (name: string, target: string, from: string) => ({ name, kind: "one", target, from, to: "id", foreign_key: true });
  const models = define({ dialect: "sqlite", models: [
    { name: "JAuthor", table: "j_authors", fields: [field("id", "int", { primary_key: true }), field("name")] },
    { name: "JBook", table: "j_books", fields: [field("id", "int", { primary_key: true }), field("author_id", "int")], relations: [rel("author", "JAuthor", "author_id")] },
    { name: "JShelf", table: "j_shelves", fields: [field("id", "int", { primary_key: true }), field("book_id", "int")], relations: [rel("book", "JBook", "book_id")] },
  ], behavior: { schema_contract: 5, read_defaults: [{ source: "monorm.query-defaults", name: "query", model: "JBook", load: [["author"]] }] } } as never, { registry }) as Record<string, any>;
  const { JAuthor: Author, JBook: Book, JShelf: Shelf } = models;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await Author.objects.using(db).insert({ id: 1, name: "ann" });
    await Book.objects.using(db).insert({ id: 1, authorId: 1 });
    await Shelf.objects.using(db).insert({ id: 1, bookId: 1 });
    // JBook's default joins its author also when JBook is itself joined.
    const shelf = await Shelf.objects.using(db).load(Shelf.book).get();
    assert.equal(shelf.book.author.name, "ann");
  } finally { await db.close(); }
});

test("aggregate to-one tail applies the target default in the join", async (t) => {
  if (!queryDefaults) return t.skip("needs the monorm.query-defaults package");
  const { db, Shelf, a, b } = await open("postgres", true, false, true);
  try {
    const shelf = await Shelf.objects.using(db).insert({});
    const [shown, hidden] = await a.withoutDefaults().insertMany([{ name: "shown", bio: "x" }, { name: "hidden", bio: "x", visible: false }]).returning();
    await b.insertMany([{ title: "a", authorId: shown.pk, shelfId: shelf.pk }, { title: "b", authorId: hidden.pk, shelfId: shelf.pk }]);
    // The hidden author is no row of the join, but its book stays an element: the default sits in the ON.
    const row = await Shelf.objects.using(db).select({
      names: func.arrayAgg(Shelf.books.author.name, { orderBy: Shelf.books.title }),
      n: func.count(Shelf.books.author),
    }).first();
    assert.deepEqual([row.names, Number(row.n)], [["shown", null], 1]);
  } finally { await db.dropTables(); await db.close(); }
});

for (const dialect of ["sqlite", "postgres"] as const) {
  test(`bypass by name keeps the other defaults: ${dialect}`, async (t) => {
    if (!queryDefaults) return t.skip("needs the monorm.query-defaults package");
    const { db, Author, a } = await open(dialect, true);
    try {
      await a.insertMany([{ name: "visible", bio: "x" }, { name: "hidden", bio: "x", visible: false }]);
      assert.equal(await a.withoutDefaults("query").count(), 2);
      await assert.rejects(a.withoutDefaults("nope").count(), /unknown default "nope"; available defaults: \["query"\]/);
      const full = await a.withoutDefaults("query").orderBy(Author.id).first();
      assert.equal(full.bio, "x");
    } finally { await db.dropTables(); await db.close(); }
  });
}
