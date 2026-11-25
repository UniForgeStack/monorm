import assert from "node:assert/strict";
import { readFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { connect, DatabaseError, DoesNotExist, func, IntegrityError, MultipleObjectsReturned, loads, Migrations, MigrationError, Migrator, outer, QueryError, Registry, SchemaError, statementInspection } from "../src/index.js";
import { User } from "./blog/models.js";
import { Author, Book, Status, sqliteRegistry } from "./sqlite/models.js";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Any = any;

test("array_agg is Postgres only", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    await assert.rejects(Author.objects.using(db).select({ n: func.arrayAgg(Author.name) }).scalar(), /sqlite does not support array_agg/);
    const [a, b, c] = await Author.objects.using(db).insertMany([
      { email: "a", name: "A" }, { email: "b", name: "B" }, { email: "c", name: "C" },
    ]).returning();
    await Book.objects.using(db).insertMany([
      { authorId: a!.id, title: "one" }, { authorId: a!.id, title: "two", pages: 7 }, { authorId: b!.id, title: "three", pages: 3 },
    ]);
    const rows = await Author.objects.using(db).select({ name: Author.name, n: func.count(Author.books), p: func.max(Author.books.pages) }).orderBy(Author.name).all();
    assert.deepEqual(rows.map((r) => [r.name, Number(r.n), r.p]), [["A", 2, 7], ["B", 1, 3], ["C", 0, null]]);
    const unpaged = Author.objects.using(db).select({ id: Author.id, n: func.count(Author.books), p: func.max(Author.books.pages) });
    assert.deepStrictEqual(unpaged.inspect(), { statementCount: 1, hasBatching: false, statements: [statementInspection({ scalarCorrelated: 2 })] });
    const page = Author.objects.using(db).select({ id: Author.id, n: func.count(Author.books), p: func.max(Author.books.pages) })
      .orderBy(Author.name).limit(10).offset(1);
    assert.deepStrictEqual(page.inspect().statements, [statementInspection({ scalarCorrelated: 2 })]);
    assert.ok(!page.sql().includes("LATERAL"));
    assert.deepEqual((await page.all()).map((r) => [r.id, Number(r.n), r.p]), [[b!.id, 1, 3], [c!.id, 0, null]]);
  } finally {
    await db.close();
  }
});

test("crud relations defaults and upserts", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const a = await Author.objects.using(db).insert({ email: "a@example.com", name: "Alice" });
    assert.equal(typeof a.id, "bigint");
    assert.ok(a.active && a.createdAt instanceof Date);
    const b = await Author.objects.using(db).insert({ email: "b@example.com", name: "Bob" });
    const books = await Book.objects.using(db).insertMany([
      { authorId: a.id, title: "one" },
      { authorId: a.id, title: "two", pages: 7, metadata: { tags: [1, true] } },
      { authorId: b.id, title: "three", status: Status.published },
    ]).returning();
    assert.deepEqual(books.map((x) => x.pages), [0, 7, 0]);
    assert.equal(books[0]!.status, Status.draft);
    assert.deepEqual(books[1]!.metadata, { tags: [1, true] });
    const joined = await Book.objects.using(db).load(Book.author).orderBy(Book.id).all();
    assert.deepEqual(joined.map((x) => x.author.name), ["Alice", "Alice", "Bob"]);
    const prefetched = await Author.objects.using(db).load(Author.books).orderBy(Author.id).all();
    assert.deepEqual(prefetched.map((x) => x.books.cached.length), [2, 1]);
    assert.equal(await Author.objects.using(db).filter(Author.books.pages.gt(0)).count(), 1);
    assert.ok(await Book.objects.using(db).exists());
    assert.equal(await Author.objects.using(db).filter(Author.name.icontains("ali")).count(), 1);
    assert.equal(await Author.objects.using(db).filter(Author.name.contains("ali")).count(), 0);
    const same = await Author.objects.using(db).insert({ email: a.email, name: "Updated" }).onConflict(Author.email, { update: true }).returning();
    assert.ok(same!.id === a.id && same!.name === "Updated");
    const rows = await Book.objects.using(db).filter(Book.pages.gt(0)).update({ pages: Book.pages.add(1) }).returning();
    assert.equal(rows[0]!.pages, 8);
    await assert.rejects(Book.objects.using(db).insert({ authorId: 999n, title: "orphan" }), (e: unknown) => {
      assert.ok(e instanceof IntegrityError);
      assert.deepEqual([e.sqlstate, e.constraint], ["23503", null]);
      return true;
    });
    await assert.rejects(Author.objects.using(db).insert({ email: a.email, name: "dup" }), (e: unknown) => e instanceof IntegrityError && e.sqlstate === "23505");
    await assert.rejects(Book.objects.using(db).insert({ authorId: a.id, title: "bad", status: "invalid" as never }), IntegrityError);
    assert.equal(await Book.objects.using(db).filter(Book.id.eq(books[0]!.id)).delete(), 1);
    assert.equal((sqliteRegistry.ir() as Any)["dialect"], "sqlite");
  } finally { await db.close(); }
});

test("aggregates windows ctes sliced prefetch and bulk updates", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const authors = await Author.objects.using(db).insertMany([{ email: "a", name: "A" }, { email: "b", name: "B" }]).returning();
    const books = await Book.objects.using(db).insertMany(
      authors.flatMap((a) => [3, 7, 11].map((pages) => ({ authorId: a.id, title: `${a.name}${pages}`, pages }))),
    ).returning();
    assert.equal(await Book.objects.using(db).select({ total: func.sum(Book.pages) }).scalar(), 42n);
    const ranked = await Book.objects.using(db).select({
      title: Book.title,
      rank: func.rowNumber().over({ partitionBy: Book.authorId, orderBy: Book.pages.desc() }),
    }).orderBy(Book.title).all();
    assert.deepEqual(ranked.map((r) => [r.title, r.rank]), [["A11", 1n], ["A3", 3n], ["A7", 2n], ["B11", 1n], ["B3", 3n], ["B7", 2n]]);
    const totals = Book.objects.using(db).select({ authorId: Book.authorId, pages: func.sum(Book.pages) }).groupBy(Book.authorId).cte("totals");
    const joined = await Author.objects.using(db).join(totals, totals.c.authorId.eq(Author.id)).select({ name: Author.name, pages: totals.c.pages }).orderBy(Author.id).all();
    assert.deepEqual(joined.map((r) => [r.name, r.pages]), [["A", 21n], ["B", 21n]]);
    const sliced = await Author.objects.using(db).load(Author.books.objects.orderBy(Book.pages.desc()).slice(1, 2)).orderBy(Author.id).all();
    assert.deepEqual(sliced.map((a) => a.books.cached.map((b) => b.title)), [["A7"], ["B7"]]);
    const updated = await Book.objects.using(db).updateMany(books.map((b) => ({ id: b.id, pages: b.pages + 1 }))).returning();
    assert.deepEqual(updated.map((b) => b.pages).sort((x, y) => x - y), [4, 4, 8, 8, 12, 12]);
    await assert.rejects(
      Book.objects.using(db).updateMany(books.map((b, i) => ({ id: b.id, pages: i === 1 ? -1 : 99 })), { batchSize: 1 }),
      IntegrityError,
    );
    assert.equal(await Book.objects.using(db).select({ total: func.sum(Book.pages) }).scalar(), 48n);
  } finally { await db.close(); }
});

test("transactions concurrency and abandonment", async (t) => {
  const g = globalThis as { gc?: () => void; Bun?: { gc(sync: boolean): void } };
  const gc = g.gc ?? (g.Bun ? () => g.Bun!.gc(true) : undefined);
  if (!gc) {
    t.skip("needs node --expose-gc");
    return;
  }
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    await db.transaction(async () => {
      await Author.objects.using(db).insert({ email: "outer", name: "outer" });
      await assert.rejects(db.transaction(async () => {
        await Author.objects.using(db).insert({ email: "inner", name: "inner" });
        throw new Error("rollback savepoint");
      }), /rollback savepoint/);
    });
    assert.equal(await Author.objects.using(db).count(), 1);
    await assert.rejects(db.transaction(async () => {
      await Author.objects.using(db).insert({ email: "rolled", name: "rolled" });
      throw new Error("rollback");
    }), /rollback/);
    await (async () => {
      const tx = await db.engine.begin(null);
      await db.engine.execute("INSERT INTO author (email, name) VALUES ('abandoned', 'abandoned')", tx);
    })();
    for (let i = 0; i < 5; i++) {
      gc();
      await new Promise((r) => setTimeout(r, 20));
    }
    assert.equal(await Author.objects.using(db).count(), 1);
    assert.deepEqual(await Promise.all(Array.from({ length: 20 }, () => Author.objects.using(db).count())), Array(20).fill(1));
    const { wait } = await import("../src/native.js");
    const tx = await db.engine.begin(null);
    await tx.commit();
    await assert.rejects(wait(() => db.engine.execute("SELECT 1", tx)), (e: unknown) => e instanceof DatabaseError && /closed|already committed/.test(e.message));
  } finally { await db.close(); }
});

test("unsupported queries", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    await assert.rejects(async () => Author.objects.using(db).select({ n: Author.name }).distinct(Author.name).all(), /does not support/);
    await db.transaction(async () => {
      await assert.rejects(async () => Author.objects.using(db).lock().all(), (e: unknown) => e instanceof QueryError && /does not support/.test(e.message));
      await assert.rejects(db.lock("example"), /advisory locks/);
    });
  } finally { await db.close(); }
});

test("file database and target mismatch", async () => {
  const root = [join(import.meta.dirname, "..", "..", ".."), join(import.meta.dirname, "..", "..", "..", "..")].find((d) => {
    try { readFileSync(join(d, "examples/sqlite/schema.prisma")); return true; } catch { return false; }
  })!;
  const source = readFileSync(join(root, "examples/sqlite/schema.prisma"), "utf8");
  const reg = new Registry();
  loads(source, { registry: reg });
  const dir = mkdtempSync(join(tmpdir(), "monorm-sqlite-"));
  const migrations = new Migrations(join(dir, "migrations"), reg);
  migrations.make("initial");
  const url = `sqlite://${join(dir, "database.db")}`;
  const db = await connect(url, { registry: reg, default: false });
  try {
    const runner = new Migrator(db, migrations);
    await runner.upgrade();
    await db.execute("INSERT INTO author (email, name) VALUES ('persisted', 'Persisted')");
    assert.equal((await runner.status())[0]!.applied, true);
    await db.close();
    const reopened = await connect(url, { registry: reg, default: false });
    try {
      assert.equal((await reopened.fetchText("SELECT name FROM author"))[0]![0], "Persisted");
      await new Migrator(reopened, migrations).downgrade({ target: "zero", allowDestructive: true });
    } finally { await reopened.close(); }
    await assert.rejects(connect("sqlite://:memory:", { registry: new Registry(), default: false }), (e: unknown) => e instanceof SchemaError && /schema targets postgres, connection uses sqlite/.test(e.message));
    assert.throws(() => loads(source.replace("provider = \"sqlite\"", "provider = \"postgresql\""), { registry: reg }), /same database/);
  } finally {
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
});

test("kept rename hints do not redirect later rebuilds", async () => {
  const root = [join(import.meta.dirname, "..", "..", ".."), join(import.meta.dirname, "..", "..", "..", "..")].find((d) => {
    try { readFileSync(join(d, "examples/sqlite/schema.prisma")); return true; } catch { return false; }
  })!;
  const source = readFileSync(join(root, "examples/sqlite/schema.prisma"), "utf8");
  const renamed = source.replace("  books      Book[]", "  books      Book[]\n  @@map(\"writer\")\n  @@renamed_from(\"author\")")
    .replace("  title     String", "  name      String @renamed_from(\"title\")").replace("@@index([title]", "@@index([name]");
  const regs = [source, renamed, renamed.replace("  pages     Int", "  added     Int @default(42)\n  pages     Int")].map((text) => {
    const reg = new Registry();
    loads(text, { registry: reg });
    return reg;
  });
  const dir = mkdtempSync(join(tmpdir(), "monorm-sqlite-"));
  ["initial", "rename", "later"].forEach((name, i) => new Migrations(dir, regs[i]!).make(name));
  const db = await connect("sqlite://:memory:", { registry: regs[0]!, default: false });
  try {
    const runner = new Migrator(db, new Migrations(dir, regs[0]!));
    await runner.upgrade("1");
    await db.execute("INSERT INTO author (id, email, name) VALUES (100, 'gone', 'gone'); DELETE FROM author; INSERT INTO author (email, name) VALUES ('kept', 'kept')");
    await db.execute("INSERT INTO book (author_id, title) VALUES (101, 'kept')");
    await runner.upgrade("2");
    await runner.upgrade();
    assert.deepEqual(await db.fetchText("SELECT w.email, b.name, b.added FROM writer w JOIN book b ON b.author_id = w.id"), [["kept", "kept", "42"]]);
    assert.deepEqual(await db.fetchText("PRAGMA foreign_key_check"), []);
    await db.execute("INSERT INTO writer (email, name) VALUES ('next', 'next')");
    assert.equal((await db.fetchText("SELECT max(id) FROM writer"))[0]![0], "102");
    await runner.downgrade({ allowDestructive: true });
    assert.deepEqual(await db.fetchText("SELECT w.email, b.name FROM writer w JOIN book b ON b.author_id = w.id"), [["kept", "kept"]]);
  } finally {
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
});

test("outer() through a relation path", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const alice = await Author.objects.using(db).insert({ email: "a@example.com", name: "Alice" });
    const bob = await Author.objects.using(db).insert({ email: "b@example.com", name: "Bob" });
    await Book.objects.using(db).insertMany([{ authorId: alice.id, title: "one" }, { authorId: bob.id, title: "two" }]);
    const name = Author.objects.filter(Author.email.eq(outer(Book.author.email))).select({ n: Author.name }).asScalar();
    assert.deepEqual(await Book.objects.using(db).orderBy(Book.id).select({ title: Book.title, name }).all(), [{ title: "one", name: "Alice" }, { title: "two", name: "Bob" }]);
  } finally { await db.close(); }
});

test("related sets use the database of their instance", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const alice = await Author.objects.using(db).insert({ email: "a@example.com", name: "Alice" });
    await alice.books.insert({ title: "one" });
    assert.deepEqual((await alice.books.all()).map((b) => b.title), ["one"]);
    assert.equal(await alice.books.filter(Book.pages.eq(0)).count(), 1);
    const registry = new Registry();
    const { Person } = loads(FOLLOW, { registry }) as Record<string, Any>;
    const followDb = await connect("sqlite://:memory:", { registry, default: false });
    try {
      await followDb.createTables();
      const [ann, bob] = await Person.objects.using(followDb).insertMany([{ name: "ann" }, { name: "bob" }]).returning();
      await ann.following.add(bob);
      assert.deepEqual((await ann.following.all()).map((p: Any) => p.name), ["bob"]);
    } finally { await followDb.close(); }
  } finally { await db.dropTables(); await db.close(); }
});

test("string functions and concatenation", async () => {
  const registry = new Registry();
  const Note = loads(`
    datasource db { provider = "sqlite" }
    model Note {
      id BigInt @id @default(autoincrement())
      title String
      tag String?
    }`, { registry })["Note"] as any;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await Note.objects.using(db).insertMany([{ title: "  a-b  ", tag: "x" }, { title: "c", tag: null }]);
    const rows = await Note.objects.using(db).orderBy(Note.id).select({
      c: func.concat(Note.title, Note.tag), p: Note.title.concat(Note.tag), t: func.trim(Note.title), l: func.ltrim(Note.title),
      r: func.rtrim(Note.title), x: func.replace(Note.title, "-", "+"), s: func.substr(func.trim(Note.title), 2, 1), i: func.strpos(Note.title, "b"),
    }).all();
    assert.deepEqual(rows, [
      { c: "  a-b  x", p: "  a-b  x", t: "a-b", l: "a-b  ", r: "  a-b", x: "  a+b  ", s: "-", i: 5 },
      { c: "c", p: null, t: "c", l: "c", r: "c", x: "c", s: "", i: 0 },
    ]);
    assert.equal(await Note.objects.using(db).filter(Note.title.concat("!").eq("c!")).count(), 1);
  } finally { await db.close(); }
});

test("split_part is refused and random and substr expressions run", async () => {
  const registry = new Registry();
  const Note = loads(`
    datasource db { provider = "sqlite" }
    model Note {
      id BigInt @id @default(autoincrement())
      title String
    }`, { registry })["Note"] as any;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await Note.objects.using(db).insertMany([{ title: "abc" }, { title: "de" }]);
    await assert.rejects(async () => Note.objects.using(db).select({ p: func.splitPart(Note.title, "-", 1) }).all(), /sqlite does not support split_part/);
    const tails = await Note.objects.using(db).orderBy(Note.id).select({ s: func.substr(Note.title, func.length(Note.title)) }).all();
    assert.deepEqual(tails, [{ s: "c" }, { s: "e" }]);
    const values = await Note.objects.using(db).select({ r: func.random() }).all();
    assert.ok(values.length === 2 && values.every(({ r }: { r: number }) => typeof r === "number" && r >= 0 && r <= 1));
  } finally { await db.close(); }
});

test("onConflict infers the partial unique index", async () => {
  const registry = new Registry();
  const Account = loads(`
    datasource db { provider = "sqlite" }
    model Account {
      id BigInt @id @default(autoincrement())
      email String
      active Boolean @default(true)
      @@index([email], unique: true, where: raw("active"))
    }`, { registry })["Account"] as any;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const accounts = Account.objects.using(db);
    await accounts.insert({ email: "a" }).onConflict(Account.email, { update: false });
    await accounts.insert({ email: "a" }).onConflict(Account.email, { update: false });
    await accounts.insertMany([{ email: "a" }, { email: "b" }]).onConflict(Account.email, { update: false });
    assert.equal(await accounts.count(), 2);
    const [row, created] = await accounts.getOrInsert({ email: "c" });
    const [again, createdAgain] = await accounts.getOrInsert({ email: "c" });
    assert.deepEqual([created, createdAgain, again.id], [true, false, row.id]);
  } finally { await db.close(); }
});

test("CASE expression", async () => {
  const registry = new Registry();
  const Note = loads(`
    datasource db { provider = "sqlite" }
    model Note {
      id BigInt @id @default(autoincrement())
      n Int
    }`, { registry })["Note"] as any;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await Note.objects.using(db).insertMany([{ n: 1 }, { n: 5 }, { n: 10 }]);
    const size = func.case([Note.n.gte(10), "big"], [Note.n.gte(5), "mid"], { default: "small" });
    assert.deepEqual((await Note.objects.using(db).orderBy(Note.id).select({ s: size }).all()).map((r: any) => r.s), ["small", "mid", "big"]);
    assert.equal(await Note.objects.using(db).select({ t: func.sum(func.case([Note.n.gte(5), 1], { default: 0 })) }).scalar(), 2n);
    await Note.objects.using(db).all().update({ n: func.case([Note.n.eq(1), 100], { default: Note.n }) });
    assert.deepEqual((await Note.objects.using(db).orderBy(Note.id).all()).map((r: any) => r.n), [100, 5, 10]);
  } finally { await db.close(); }
});

test("aggregate FILTER", async () => {
  const registry = new Registry();
  const Note = loads(`
    datasource db { provider = "sqlite" }
    model Note {
      id BigInt @id @default(autoincrement())
      n Int
    }`, { registry })["Note"] as any;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await Note.objects.using(db).insertMany([{ n: 1 }, { n: 5 }, { n: 10 }]);
    const row = await Note.objects.using(db).select({ c: func.count({ filter: Note.n.gte(5) }), s: func.sum(Note.n, { filter: Note.n.lt(10) }) }).get();
    assert.deepEqual(row, { c: 2n, s: 6n });
    const selected = Note.objects.using(db).select({ n: Note.n });
    assert.deepEqual(await selected.get(Note.n.eq(5)), { n: 5 });
    await assert.rejects(selected.require().get(Note.n.eq(-1)), DoesNotExist);
    await assert.rejects(selected.get(), MultipleObjectsReturned);
  } finally { await db.close(); }
});

test("aggregate FILTER through relations", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const alice = await Author.objects.using(db).insert({ email: "a@example.com", name: "Alice" });
    const bob = await Author.objects.using(db).insert({ email: "b@example.com", name: "Bob" });
    await Book.objects.using(db).insertMany([
      { authorId: alice.id, title: "one", pages: 5 }, { authorId: alice.id, title: "two", pages: 7 }, { authorId: bob.id, title: "three", pages: 100 },
    ]);
    const books = Book.objects.using(db);
    assert.equal(await books.select({ s: func.sum(Book.pages, { filter: Book.author.name.eq("Alice") }) }).scalar(), 12n);
    assert.equal(await books.select({ c: func.count({ filter: Book.author.name.eq("Alice") }) }).scalar(), 2n);
    const rows = await Author.objects.using(db).orderBy(Author.id).select({ n: Author.name, c: func.count(Author.books, { filter: Author.name.eq("Alice") }) }).all();
    assert.deepEqual(rows, [{ n: "Alice", c: 2n }, { n: "Bob", c: 0n }]);
  } finally { await db.close(); }
});

test("UUID, date, timestamp and inline trigger", async () => {
  const registry = new Registry();
  const Event = loads(`
    datasource db { provider = "sqlite" }
    model Event {
      id String @id @db.Uuid
      day DateTime @db.Date
      at DateTime
      name String
      @@trigger(uppercase, when: after, operation: [insert], func: "UPDATE event SET name = upper(NEW.name) WHERE id = NEW.id")
    }
  `, { registry })["Event"]!;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const id = "c689c080-a6d0-4d3e-9305-7fe896158ac9";
    const day = new Date("2026-03-04T00:00:00Z");
    const at = new Date("2026-03-04T12:34:56.123+03:30");
    await Event.objects.using(db).insert({ id, day, at, name: "hello" });
    const loaded = await Event.objects.using(db).get() as { id: string; day: Date; at: Date; name: string };
    assert.equal(loaded.id, id);
    assert.equal((loaded.day as Date).toISOString(), day.toISOString());
    assert.equal((loaded.at as Date).toISOString(), at.toISOString());
    assert.equal(loaded.name, "HELLO");
  } finally { await db.close(); }
});

test("full-text search on SQLite is an error", async () => {
  const lite = new Registry();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const Note = loads(`datasource db { provider = "sqlite" }\nmodel Note {\n  id Int @id\n  body String\n}`, { registry: lite })["Note"] as any;
  const db = await connect("sqlite://:memory:", { registry: lite, default: false });
  try {
    await db.createTables();
    await assert.rejects(Note.objects.using(db).filter(func.toTsvector(Note.body).matches("x")).count(), /sqlite does not support full-text search/);
  } finally { await db.close(); }
});

test("load reload reads the row again", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const row = await Author.objects.using(db).insert({ email: "a", name: "Alice" });
    await Author.objects.using(db).filter(Author.id.eq(row.id)).update({ name: "Al" });
    assert.equal((await row.load({ reload: true })).name, "Al");
    await Author.objects.using(db).filter(Author.id.eq(row.id)).update({ name: "Ali" });
    assert.equal((await row.load(Author.name, { reload: true })).name, "Ali");
  } finally {
    await db.close();
  }
});

test("relation comparison rejects null-prototype objects clearly", () => {
  for (const compare of [() => Book.author.eq(Object.create(null) as never), () => Book.author.ne(Object.create(null) as never)]) {
    assert.throws(compare, { name: "TypeError", message: "Book.author compares with a Author instance" });
  }
});

test("SQLite related lazy inserts join the transaction at await", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const author = await Author.objects.using(db).insert({ email: "lazy", name: "Lazy" });
    const statements = [
      author.books.insert({ title: "one" }),
      author.books.insertMany([{ title: "many" }]).returning(),
    ];
    const rollback = new Error("rollback related insert");
    for (const statement of statements) {
      await assert.rejects(db.transaction(async () => {
        await statement;
        throw rollback;
      }), (e) => e === rollback);
    }
    assert.equal(await Book.objects.using(db).count(), 0);
  } finally { await db.close(); }
});

test("bound load guards and withoutRelated", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const a = await Author.objects.using(db).insert({ email: "a", name: "Alice" });
    const b = await Author.objects.using(db).insert({ email: "b", name: "Bob" });
    await Book.objects.using(db).insertMany([
      { authorId: a.id, title: "one" }, { authorId: a.id, title: "two" }, { authorId: b.id, title: "three" },
    ]);
    const bound = Author.books.objects.using(db).filter(Book.title.eq("one"));
    // @ts-expect-error bound query sets cannot be awaited directly
    for (const run of [async () => await bound, async () => await bound.count(), async () => await bound.delete(),
      async () => await bound.insertMany([]), async () => await bound.insertMany([], { copy: true })]) {
      await assert.rejects(run, { name: "TypeError", message: /inside load/ });
    }
    assert.equal(await Book.objects.using(db).count(), 3);
    for (const path of [Author.books, Author.books.author]) {
      assert.throws(() => Author.objects.load(path, bound), /label\(\)/);
    }
    const authors = await Author.objects.using(db).load(Author.name, Author.books, Author.books.objects).withoutRelated().orderBy(Author.id);
    assert.deepEqual(authors.map((a) => a.books.cached.length), [2, 1]);
    const books = await Book.objects.using(db).load(Book.title, Book.author).withoutRelated();
    assert.deepEqual(books.map((b) => b.title).sort(), ["one", "three", "two"]);
    assert.throws(() => books[0]!.author, /not loaded/i);
  } finally {
    await db.close();
  }
});


test("indexed relation labels", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const [a, b] = await Author.objects.using(db).insertMany([
      { email: "a", name: "A" }, { email: "b", name: "B" }, { email: "c", name: "C" },
    ]).returning();
    await Book.objects.using(db).insertMany([
      { authorId: a!.id, title: "one" }, { authorId: b!.id, title: "other" },
      { authorId: a!.id, title: "two" }, { authorId: a!.id, title: "three" },
    ]);
    const rows = await Author.objects.using(db).orderBy(Author.id).load(
      Author.books.objects.at(0).label("firstBook"),
      Author.books.objects.at(1).label("secondBook"),
      Author.books.objects.at(-1).label("lastBook"),
    );
    assert.deepEqual(rows.map((a) => [a.firstBook?.title ?? null, a.secondBook?.title ?? null, a.lastBook?.title ?? null]), [
      ["one", "two", "three"], ["other", null, "other"], [null, null, null],
    ]);
    const empty = await Author.objects.using(db).load(Author.books.objects.at(5).label("outside"));
    assert.ok(empty.every((a) => a.outside === null));
    assert.throws(() => Author.objects.using(db).load(Author.books.objects.at(0) as never), TypeError);
    await assert.rejects(async () => { await Book.objects.using(db).load(Book.author.objects.at(0).label("person")); }, /to-many/);
  } finally { await db.close(); }
});

test("sql shape sends ESCAPE on SQLite only", () => {
  for (const build of [(M: Any) => M.name.contains("a%"), (M: Any) => M.name.startsWith("a"), (M: Any) => M.name.endsWith("a")]) {
    assert.ok(Author.objects.filter(build(Author)).sql().includes("ESCAPE '\\'"));
    assert.ok(!User.objects.filter(build(User)).sql().includes("ESCAPE"));
  }
  assert.ok(Author.objects.filter(Author.name.icontains("a")).sql().includes("ESCAPE '\\'"));
  assert.ok(!User.objects.filter(User.name.icontains("a")).sql().includes("ESCAPE"));
  assert.ok(Author.objects.filter(Author.name.like("a_")).sql().includes("ESCAPE '\\'"));
});

test("like helpers match the escaped text", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const objects = Author.objects.using(db);
    await objects.insertMany(["50% off", "50x off", "a_b", "axb", "C:\\dir"].map((name, i) => ({ email: `e${i}`, name })));
    const names = async (qs: { all(): PromiseLike<readonly { name: string }[]> }) => (await qs.all()).map((r) => r.name);
    assert.deepEqual(await names(objects.filter(Author.name.contains("50%")).orderBy(Author.id)), ["50% off"]);
    assert.deepEqual(await names(objects.filter(Author.name.startsWith("a_")).orderBy(Author.id)), ["a_b"]);
    assert.deepEqual(await names(objects.filter(Author.name.icontains("A_B")).orderBy(Author.id)), ["a_b"]);
    assert.deepEqual(await names(objects.filter(Author.name.endsWith("\\dir")).orderBy(Author.id)), ["C:\\dir"]);
    assert.deepEqual(await names(objects.filter(Author.name.like("50_ off")).orderBy(Author.id)), ["50% off", "50x off"]);
  } finally {
    await db.close();
  }
});

test("bulk writes", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    await assert.rejects(Author.objects.using(db).insertMany([{ email: "a@x.io", name: "A" }], { copy: true }), (e: Error) => e instanceof QueryError && /needs Postgres/.test(e.message));
    // 32 766 parameters at most: 20 000 rows of two fields go to two statements.
    const rows = Array.from({ length: 20_000 }, (_, i) => ({ email: `u${i}@x.io`, name: `U${i}` }));
    const statement = Author.objects.using(db).insertMany(rows).returning();
    const result = statement.inspect();
    assert.equal(result.statementCount, 2);
    assert.ok(result.hasBatching && result.statements.every((s) => s.hasBatching));
    const events: unknown[] = [];
    const off = db.onQuery((e) => events.push(e));
    assert.equal((await statement).length, 20_000);
    off();
    assert.equal(events.length, 2);
    const [first, created] = await Author.objects.using(db).getOrInsert({ email: "u0@x.io" }, { defaults: { name: "x" } });
    assert.ok(!created && first.name === "U0");
    // A failed later batch rolls back the earlier ones.
    await assert.rejects(
      Author.objects.using(db).insertMany([{ email: "n1@x.io", name: "N" }, { email: "u1@x.io", name: "N" }], { batchSize: 1 }),
      IntegrityError,
    );
    assert.equal(await Author.objects.using(db).filter(Author.email.eq("n1@x.io")).exists(), false);
  } finally { await db.close(); }
});

const FOLLOW = `
datasource db {
  provider = "sqlite"
}

model Person {
  id        BigInt   @id @default(autoincrement())
  name      String
  following Person[] @relation(through: Follow, through_fields: [follower, followee])
  followers Person[] @relation(through: Follow, through_fields: [followee, follower])
}

model Follow {
  id          BigInt @id @default(autoincrement())
  follower_id BigInt
  followee_id BigInt
  follower    Person @relation("follower", fields: [follower_id], references: [id])
  followee    Person @relation("followee", fields: [followee_id], references: [id])

  @@unique([follower_id, followee_id])
}
`;

test("self many to many has both sides", async () => {
  const registry = new Registry();
  const { Person, Follow } = loads(FOLLOW, { registry }) as Record<string, Any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const people = Person.objects.using(db);
    const [ann, bob, cat] = await people.insertMany([{ name: "ann" }, { name: "bob" }, { name: "cat" }]).returning();
    await ann.following.add(bob, cat);
    await bob.following.add(cat);
    assert.equal(await Follow.objects.using(db).count(), 3);
    // each side reads the join rows in its own direction
    assert.deepEqual((await ann.following.orderBy(Person.id).all()).map((p: Any) => p.name), ["bob", "cat"]);
    assert.deepEqual((await cat.followers.orderBy(Person.id).all()).map((p: Any) => p.name), ["ann", "bob"]);
    assert.equal(await ann.followers.count(), 0);
    // a link removed from one side is gone from the other
    await cat.followers.remove(ann);
    assert.deepEqual((await ann.following.all()).map((p: Any) => p.name), ["bob"]);
    assert.deepEqual((await people.filter(Person.followers.name.eq("bob")).all()).map((p: Any) => p.name), ["cat"]);
    const rows = await people.select({ name: Person.name, n: func.count(Person.followers) }).orderBy(Person.id).all();
    assert.deepEqual(rows.map((r: Any) => [r.name, Number(r.n)]), [["ann", 0], ["bob", 1], ["cat", 1]]);
    const loaded = await people.load(Person.following, Person.followers).orderBy(Person.id).all();
    assert.deepEqual(loaded.map((p: Any) => [p.following.cached.map((f: Any) => f.name), p.followers.cached.map((f: Any) => f.name)]), [
      [["bob"], []],
      [["cat"], ["ann"]],
      [[], ["bob"]],
    ]);
  } finally {
    await db.dropTables();
    await db.close();
  }
});
