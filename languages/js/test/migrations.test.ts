/** Schema files, migration generation (no database) and running migrations against a
 * database of their own (migrations create and drop extensions, which are per database). */

import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";

import { DatabaseError, IntegrityError, MigrationError, Migrations, Migrator, Registry, SchemaError, connect, loads, type Database } from "../src/index.js";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { DATABASE_URL } from "./helpers.js";

// `npx monorm` as a process of its own: src/cli.ts (Bun) or dist/src/cli.js (Node)
const CLI = [".js", ".ts"].map((ext) => fileURLToPath(new URL(`../src/cli${ext}`, import.meta.url))).find(existsSync)!;

function cli(args: string[], cwd?: string): { code: number; out: string; err: string } {
  const r = spawnSync(process.execPath, [CLI, ...args], { cwd, encoding: "utf8", env: { ...process.env, MONORM_DATABASE_URL: "" } });
  return { code: r.status ?? -1, out: r.stdout, err: r.stderr };
}

// the repository root: from test/ (Bun) or dist/test/ (Node)
const ROOT = [join(import.meta.dirname, "..", "..", ".."), join(import.meta.dirname, "..", "..", "..", "..")].find((d) => existsSync(join(d, "examples")))!;

const V1 = `
model Author {
  id    BigInt @id @default(autoincrement())
  email String @unique @db.VarChar(254)
  books Book[]

  @@map("authors")
}

model Book {
  id        BigInt @id @default(autoincrement())
  author_id BigInt
  title     String @db.VarChar(200)
  pages     Int    @default(0)
  author    Author @relation(fields: [author_id], references: [id], onDelete: Cascade)

  @@index([author_id])
  @@map("books")
}
`;

// v1 + an extension type, a renamed column, new columns, indexes, constraints, a trigger
const V2 = String.raw`
model Author {
  id    BigInt @id @default(autoincrement())
  email String @unique @db.Citext
  books Book[]

  @@map("authors")
}

model Book {
  id         BigInt   @id @default(autoincrement())
  author_id  BigInt
  name       String   @renamed_from("title") @db.VarChar(200)
  pages      Int      @default(0) @check("pages >= 0")
  meta       Json?    @comment("free-form attributes")
  updated_at DateTime @default(now())
  author     Author   @relation(fields: [author_id], references: [id], onDelete: Cascade)

  @@index([author_id])
  @@index([author_id, updated_at(sort: Desc)], where: raw("pages > 0"))
  @@index([name(ops: raw("gin_trgm_ops"))], type: Gin)
  @@unique([author_id, name])
  @@map("books")
  @@trigger(touch, before: [update], body: "BEGIN\n    NEW.updated_at := now();\n    RETURN NEW;\nEND;")
}
`;

function models(source: string): Registry {
  const registry = new Registry();
  loads(source, { registry });
  return registry;
}

const tmp = () => mkdtempSync(join(tmpdir(), "monorm-migrations-"));

// -- no database ----------------------------------------------------------------------------------

test("loads() builds models from schema text", () => {
  const reg = models(V2);
  const book = reg.get("Book");
  assert.equal(book.table, "books");
  assert.deepEqual([...book.fields.keys()], ["id", "authorId", "name", "pages", "meta", "updatedAt"]);
  assert.ok(book.field("meta").nullable);
  assert.ok(book.field("updatedAt").hasServerValue);
  assert.equal(book.relations.get("author")!.target, "Author");
  // schema objects travel in the IR untouched by TypeScript
  assert.ok((book.ir["triggers"] as { body: string }[])[0]!.body.startsWith("BEGIN\n    NEW.updated_at"));
});

test("schema errors name the line", () => {
  assert.throws(() => loads("model Book {\n  id    BigInt @id\n  title Strin\n}", { registry: new Registry() }), (e: unknown) => e instanceof SchemaError && /<schema>:3:9: Book.title: unknown type Strin/.test(e.message));
});

test("the first migration", () => {
  const migs = new Migrations(tmp(), models(V1));
  const m = migs.make()!;
  assert.equal(m.name, "0001_initial");
  const up = m.upSql;
  assert.ok(up.indexOf('CREATE TABLE "authors"') < up.indexOf('CREATE TABLE "books"'));
  assert.ok(up.includes('CONSTRAINT "books_author_id_fkey" FOREIGN KEY ("author_id") REFERENCES "authors" ("id") ON DELETE CASCADE'));
  assert.ok(m.downSql.includes('DROP TABLE "books";'));
  const snap = JSON.parse(readFileSync(join(m.path, "snapshot.json"), "utf8")) as { tables: { name: string }[] };
  assert.deepEqual(snap.tables.map((t) => t.name), ["authors", "books"]);
  assert.equal(migs.make(), null); // nothing changed
});

test("the second migration alters in place", () => {
  const dir = tmp();
  new Migrations(dir, models(V1)).make();
  const migs = new Migrations(dir, models(V2));
  const sql = migs.plan().up.map((s) => s.sql);
  assert.equal(sql[0], 'CREATE EXTENSION IF NOT EXISTS "citext"');
  assert.ok(sql.includes('ALTER TABLE "books" RENAME COLUMN "title" TO "name"'));
  assert.ok(migs.plan().up.some((s) => s.warning?.includes("changes authors.email from varchar(254) to citext")));
  const m = migs.make("catalog changes")!;
  assert.equal(m.name, "0002_catalog_changes");
  assert.equal(migs.make(), null);
  // checksums are the Python package's: sha256 of up.sql
  assert.match(m.checksum, /^[0-9a-f]{64}$/);
});

test("plan steps say which ones lose data", () => {
  const dir = tmp();
  const first = new Migrations(dir, models(V1));
  assert.ok(!first.plan().up.some((s) => s.destructive));
  assert.ok(first.plan().down.some((s) => s.destructive));
  first.make();
  const second = new Migrations(dir, models(V2));
  const flagged = second.plan().up.filter((s) => s.destructive).map((s) => s.sql);
  assert.deepEqual(flagged, ['ALTER TABLE "authors" ALTER COLUMN "email" TYPE citext USING "email"::citext']);
  const m = second.make("catalog")!;
  assert.ok(m.upSql.includes("-- monorm: destructive alter-type authors.email\n"));
  assert.ok(m.downSql.includes("-- monorm: destructive alter-type authors.email\n"));
});

test("migrations from a schema file", () => {
  const dir = tmp();
  writeFileSync(join(dir, "schema.prisma"), V1);
  const m = new Migrations(join(dir, "migrations"), join(dir, "schema.prisma")).make();
  assert.ok(m !== null);
});

test("the generated models are current", async () => {
  const { native } = await import("../src/native.js");
  const schema = join(ROOT, "examples/blog/schema.prisma");
  assert.deepEqual(readFileSync(join(ROOT, "examples/blog/models.ts"), "utf8"), native().generateTypescript(schema, "monorm"), "run `monorm generate typescript`");
  assert.deepEqual(readFileSync(join(ROOT, "languages/js/test/blog/models.ts"), "utf8"), native().generateTypescript(schema, "../../src/index.js"));
  const nestedSchema = join(ROOT, "examples/nested/schema.prisma");
  assert.deepEqual(readFileSync(join(ROOT, "examples/nested/models.ts"), "utf8"), native().generateTypescript(nestedSchema, "monorm"), "run `monorm generate typescript` in examples/nested");
  assert.deepEqual(readFileSync(join(ROOT, "languages/js/test/nested/models.ts"), "utf8"), native().generateTypescript(nestedSchema, "../../src/index.js"));
  const out = join(tmp(), "models.ts");
  const sets = ["--query-set", "Post=./queries.js#PostQueries", "--query-set", "Tag=./queries.js#TagQueries"];
  assert.equal(cli(["--schema", schema, "generate", "typescript", "-o", out, "--import", "../../../src/index.js", ...sets]).code, 0);
  assert.deepEqual(readFileSync(join(ROOT, "languages/js/test/typing/queries/models.ts"), "utf8"), readFileSync(out, "utf8"), "regenerate languages/js/test/typing/queries/models.ts");
});

test("the CLI", () => {
  const dir = tmp();
  writeFileSync(join(dir, "schema.prisma"), V1);
  const run = (...args: string[]) => cli(args, dir);
  assert.equal(run("check").code, 0);
  assert.equal(run("generate", "-o", "app/models.ts").code, 0);
  assert.ok(readFileSync(join(dir, "app/models.ts"), "utf8").includes("export interface BookSpec"));
  assert.equal(run("generate").code, 0); // TypeScript by default under npx monorm
  assert.ok(existsSync(join(dir, "models.ts")));
  writeFileSync(join(dir, "package.json"), JSON.stringify({ monorm: { querySets: { Author: "./queries.js#AuthorQueries" } } }));
  assert.equal(run("generate", "-o", "q/models.ts").code, 0);
  assert.ok(readFileSync(join(dir, "q/models.ts"), "utf8").includes("useQuerySet(Author, () => _q0.AuthorQueries);"));
  assert.match(run("generate", "-o", "q/models.ts", "--query-set", "Nope=./x.js#X").err, /no model Nope/);
  assert.equal(run("makemigrations", "--check").code, 1);
  const made = run("makemigrations");
  assert.equal(made.code, 0);
  assert.match(made.out, /^Created /);
  assert.equal(run("makemigrations", "--check").code, 0);
  const sql = run("sqlmigrate", "1");
  assert.equal(sql.code, 0);
  assert.ok(sql.out.includes('CREATE TABLE "books"'));
  assert.equal(run("nope").code, 2);
  assert.match(run("--help").out, /^usage: npx monorm /);
  assert.equal(run("migrate").code, 2); // no database URL
  writeFileSync(join(dir, "package.json"), JSON.stringify({ monorm: { schema: "db/schema.prisma", migrations: "db/migrations" } }));
  assert.match(run("check").err, /db\/schema.prisma/);
  writeFileSync(join(dir, "package.json"), "{}");
  writeFileSync(join(dir, "schema.prisma"), "model X {");
  const bad = run("check");
  assert.equal(bad.code, 1);
  assert.ok(bad.err.includes("schema.prisma:1:1: model X is not closed"));
});

// -- against a database -----------------------------------------------------------------------------

const url = new URL(DATABASE_URL);
const MIGRATIONS_DB = url.pathname.slice(1) + "_migrations";
const MIGRATIONS_URL = Object.assign(new URL(DATABASE_URL), { pathname: "/" + MIGRATIONS_DB }).toString();

before(async () => {
  const admin = await connect(DATABASE_URL, { maxConnections: 1, default: false, registry: new Registry() });
  try {
    if (!(await admin.fetchText(`SELECT 1 FROM pg_database WHERE datname = '${MIGRATIONS_DB}'`)).length) {
      await admin.execute(`CREATE DATABASE "${MIGRATIONS_DB}"`);
    }
  } finally {
    await admin.close();
  }
});

const opened: Database[] = [];
after(async () => {
  for (const db of opened) {
    await db.close();
  }
});

async function database(reg: Registry): Promise<Database> {
  const db = await connect(MIGRATIONS_URL, { maxConnections: 2, default: false, registry: reg });
  opened.push(db);
  await db.execute("DROP TABLE IF EXISTS books, authors, monorm_migrations CASCADE; DROP FUNCTION IF EXISTS books_touch() CASCADE");
  return db;
}

async function scalar(db: Database, sql: string): Promise<string | null> {
  return (await db.fetchText(sql))[0]?.[0] ?? null;
}

test("upgrade and downgrade round trip", async () => {
  const dir = tmp();
  const [reg1, reg2] = [models(V1), models(V2)];
  const db = await database(reg2);
  new Migrations(dir, reg1).make();
  const migrator = new Migrator(db, new Migrations(dir, reg2));
  assert.deepEqual((await migrator.upgrade()).map((m) => m.name), ["0001_initial"]);
  await db.execute("INSERT INTO authors (email) VALUES ('Ann@Example.com'); INSERT INTO books (author_id, title) VALUES (1, 'Dune')");

  new Migrations(dir, reg2).make("v2");
  assert.deepEqual((await migrator.status()).map((s) => s.applied), [true, false]);
  await migrator.upgrade(undefined, { allowDestructive: true });

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const [Author, Book] = [reg2.get("Author").model, reg2.get("Book").model] as any[];
  // the renamed column kept its data; citext compares case-insensitively
  const book = await Book.objects.using(db).get(Book.name.eq("Dune"));
  assert.equal(await Author.objects.using(db).filter(Author.email.eq("ann@example.COM")).count(), 1);
  const before = book.updatedAt;
  await book.update({ pages: 10 }); // the trigger
  assert.ok(book.updatedAt > before);
  await assert.rejects(book.update({ pages: -1 }), IntegrityError);
  await assert.rejects(Author.objects.using(db).insert({ email: "ANN@example.com" }), IntegrityError);
  await assert.rejects(Book.objects.using(db).insert({ authorId: 1, name: "Dune" }), IntegrityError);

  // back to zero and up again: the down migrations are valid DDL too
  assert.deepEqual((await migrator.downgrade({ target: "zero", allowDestructive: true })).map((m) => m.name), ["0002_v2", "0001_initial"]);
  assert.equal(await scalar(db, "SELECT to_regclass('books')::text"), null);
  assert.equal((await migrator.upgrade()).length, 2);
  assert.deepEqual(await migrator.upgrade(), []);
  assert.deepEqual((await migrator.downgrade({ allowDestructive: true })).map((m) => m.name), ["0002_v2"]);
  assert.equal(await scalar(db, "SELECT data_type::text FROM information_schema.columns WHERE table_name = 'books' AND column_name = 'title'"), "character varying");
});

test("an edited applied migration is refused", async () => {
  const dir = tmp();
  const db = await database(models(V1));
  const migs = new Migrations(dir, models(V1));
  const m = migs.make()!;
  const migrator = new Migrator(db, migs);
  await migrator.upgrade();
  writeFileSync(join(m.path, "up.sql"), m.upSql + "\n-- edited\n");
  new Migrations(dir, models(V2)).make();
  await assert.rejects(migrator.upgrade(), (e: unknown) => e instanceof MigrationError && /changed after it was applied/.test(e.message));
});

test("a failed migration rolls back", async () => {
  const dir = tmp();
  const db = await database(models(V1));
  const migs = new Migrations(dir, models(V1));
  const m = migs.make()!;
  writeFileSync(join(m.path, "up.sql"), m.upSql + "\nSELECT no_such_function();\n");
  await assert.rejects(new Migrator(db, migs).upgrade(), DatabaseError);
  assert.equal(await scalar(db, "SELECT to_regclass('authors')::text"), null);
  assert.deepEqual((await new Migrator(db, migs).status()).map((s) => s.applied), [false]);
});

test("the CLI against a database", async () => {
  const dir = tmp();
  writeFileSync(join(dir, "schema.prisma"), V1);
  await database(models(V1));
  const run = (...args: string[]) => cli(["--schema", join(dir, "schema.prisma"), "--dir", join(dir, "migrations"), "--url", MIGRATIONS_URL, ...args]);
  assert.equal(run("makemigrations").code, 0);
  assert.deepEqual(run("migrate"), { code: 0, out: "Applied 0001_initial\n", err: "" });
  assert.match(run("showmigrations").out, /^\[x\] 0001_initial {2}\(/);
  assert.equal(run("migrate").out, "Nothing to apply.\n");
  assert.deepEqual(run("rollback", "--to", "zero"), { code: 0, out: "Reverted 0001_initial\n", err: "" });
});

test("Node launcher converts native expressions offline and preserves pending changes", () => {
  const dir = tmp();
  const path = join(dir, "schema.prisma");
  const old = 'model Item {\n id Int @id\n qty Int @check(raw("qty > 0"))\n}\n';
  writeFileSync(path, old);
  assert.equal(cli(["makemigrations", "initial"], dir).code, 0);
  const history = readFileSync(join(dir, "migrations/0001_initial/snapshot.json"), "utf8");
  writeFileSync(path, old.replace(" qty Int", " note String?\n qty Int"));
  const result = cli(["fmt", "--native"], dir);
  assert.equal(result.code, 0, result.err);
  const converted = readFileSync(path, "utf8");
  assert.ok(converted.includes("note String?"));
  assert.ok(converted.includes("@check((qty > 0))"));
  assert.equal(cli(["makemigrations", "--check"], dir).code, 1);
  assert.equal(cli(["fmt", "--native"], dir).code, 0);
  assert.equal(readFileSync(path, "utf8"), converted);
  assert.equal(readFileSync(join(dir, "migrations/0001_initial/snapshot.json"), "utf8"), history);
});

// -- the destructive gate ---------------------------------------------------------------------------

const GATE_SQLITE = 'datasource db { provider = "sqlite" }\n';
const GATE_KEEP = 'model Keep {\n  id BigInt @id @default(autoincrement())\n\n  @@map("keep")\n}\n';
const GATE_SHOP = 'model Shop {\n  id   BigInt @id @default(autoincrement())\n  name String\n  note String?\n\n  @@map("shop")\n}\n';
const GATE_TABLES = ["shop", "venue", "keep", "ghost_done", "after_tx", "half", "extra", "monorm_migrations"];
const GATE_DIALECTS = ["postgres", "sqlite"] as const;
type GateDialect = (typeof GATE_DIALECTS)[number];

async function gateDatabase(dialect: GateDialect): Promise<{ db: Database; prefix: string }> {
  const prefix = dialect === "sqlite" ? GATE_SQLITE : "";
  const registry = models(prefix + GATE_KEEP);
  const db = await connect(dialect === "sqlite" ? "sqlite://:memory:" : MIGRATIONS_URL, { maxConnections: 2, default: false, registry });
  opened.push(db);
  await db.execute(GATE_TABLES.map((t) => `DROP TABLE IF EXISTS ${t}`).join("; "));
  return { db, prefix };
}

/** Writes the next migration for `source` without applying it. */
function gateNext(dir: string, source: string) {
  const migs = new Migrations(dir, models(source));
  const migration = migs.make();
  assert.ok(migration);
  return { migs, migration };
}

async function gateApply(db: Database, dir: string, source: string) {
  const { migs, migration } = gateNext(dir, source);
  await new Migrator(db, migs).upgrade();
  return migration;
}

async function gateRows(db: Database, table: string): Promise<number> {
  return (await db.fetchText(`SELECT * FROM ${table}`)).length;
}

async function gateHasTable(db: Database, dialect: GateDialect, table: string): Promise<boolean> {
  const sql = dialect === "sqlite" ? `SELECT name FROM sqlite_master WHERE name = '${table}'` : `SELECT tablename FROM pg_tables WHERE tablename = '${table}'`;
  return (await db.fetchText(sql)).length > 0;
}

const refusal = (lead: string, step: string, table: string) => (e: unknown) =>
  e instanceof MigrationError && e.message.includes(`${lead} ${step}, and ${table} has rows; pass --allow-destructive (Python: allow_destructive=True, TypeScript: { allowDestructive: true })`);

for (const dialect of GATE_DIALECTS) {
  test(`destructive gate refuses a drop on a table with rows (${dialect})`, async () => {
    const dir = tmp();
    const { db, prefix } = await gateDatabase(dialect);
    await gateApply(db, dir, prefix + GATE_KEEP + GATE_SHOP);
    await db.execute("INSERT INTO shop (name) VALUES ('a')");
    const { migs, migration } = gateNext(dir, prefix + GATE_KEEP);
    assert.ok(migration.upSql.includes("-- monorm: destructive drop-table shop\n"));
    await assert.rejects(new Migrator(db, migs).upgrade(), refusal(migration.name, "drops table shop", "shop"));
    assert.equal(await gateRows(db, "shop"), 1);
    assert.deepEqual((await new Migrator(db, migs).status()).map((s) => s.applied), [true, false]);
  });

  test(`destructive gate allows the drop with the flag (${dialect})`, async () => {
    const dir = tmp();
    const { db, prefix } = await gateDatabase(dialect);
    await gateApply(db, dir, prefix + GATE_KEEP + GATE_SHOP);
    await db.execute("INSERT INTO shop (name) VALUES ('a')");
    const { migs, migration } = gateNext(dir, prefix + GATE_KEEP);
    const done = await new Migrator(db, migs).upgrade(undefined, { allowDestructive: true });
    assert.deepEqual(done.map((m) => m.name), [migration.name]);
    assert.equal(await gateHasTable(db, dialect, "shop"), false);
  });

  test(`destructive gate allows the drop on an empty or missing table (${dialect})`, async () => {
    const dir = tmp();
    const { db, prefix } = await gateDatabase(dialect);
    await gateApply(db, dir, prefix + GATE_KEEP + GATE_SHOP);
    const { migs, migration } = gateNext(dir, prefix + GATE_KEEP);
    assert.deepEqual((await new Migrator(db, migs).upgrade()).map((m) => m.name), [migration.name]); // empty table
    // a marker for a table that does not exist: nothing to lose
    mkdirSync(join(dir, "0003_ghost"));
    writeFileSync(join(dir, "0003_ghost", "up.sql"), "-- monorm: destructive drop-table ghost\nCREATE TABLE ghost_done (id integer);\n");
    assert.deepEqual((await new Migrator(db, migs).upgrade()).map((m) => m.name), ["0003_ghost"]);
  });

  test(`destructive gate ignores a migration with no marker (${dialect})`, async () => {
    const dir = tmp();
    const { db, prefix } = await gateDatabase(dialect);
    await gateApply(db, dir, prefix + GATE_KEEP + GATE_SHOP);
    await db.execute("INSERT INTO shop (name) VALUES ('a')");
    const wider = GATE_SHOP.replace("  note String?", "  note String?\n  extra String?");
    const added = await gateApply(db, dir, prefix + GATE_KEEP + wider);
    assert.ok(!added.upSql.includes("-- monorm: destructive"));
    assert.equal(await gateRows(db, "shop"), 1);
  });

  test(`destructive gate sees a drop after a table rename (${dialect})`, async () => {
    const dir = tmp();
    const { db, prefix } = await gateDatabase(dialect);
    await gateApply(db, dir, prefix + GATE_KEEP + GATE_SHOP);
    await db.execute("INSERT INTO shop (name, note) VALUES ('a', 'n')");
    const venue = GATE_SHOP.replace('@@map("shop")', '@@map("venue")\n  @@renamed_from("shop")').replace("model Shop", "model Venue").replace("  note String?\n", "");
    const { migs, migration } = gateNext(dir, prefix + GATE_KEEP + venue);
    // the marker names the table as the database has it before the migration
    assert.ok(migration.upSql.includes("-- monorm: destructive drop-column shop.note\n"));
    await assert.rejects(new Migrator(db, migs).upgrade(), refusal(migration.name, "drops column shop.note", "shop"));
    assert.equal(await gateRows(db, "shop"), 1);
    await new Migrator(db, migs).upgrade(undefined, { allowDestructive: true });
    assert.equal(await gateRows(db, "venue"), 1);
  });

  test(`rollback gate refuses a down drop on a table with rows (${dialect})`, async () => {
    const dir = tmp();
    const { db, prefix } = await gateDatabase(dialect);
    await gateApply(db, dir, prefix + GATE_KEEP);
    const { migs, migration } = gateNext(dir, prefix + GATE_KEEP + GATE_SHOP);
    const migrator = new Migrator(db, migs);
    await migrator.upgrade(); // creating a table is not destructive
    await db.execute("INSERT INTO shop (name) VALUES ('a')");
    assert.ok(migration.downSql.includes("-- monorm: destructive drop-table shop\n"));
    await assert.rejects(migrator.downgrade(), refusal(`rolling back ${migration.name}`, "drops table shop", "shop"));
    assert.equal(await gateRows(db, "shop"), 1);
    assert.deepEqual((await migrator.downgrade({ allowDestructive: true })).map((m) => m.name), [migration.name]);
  });
}

test("destructive gate waits for a concurrent insert on postgres", async () => {
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  await gateApply(db, dir, GATE_KEEP + GATE_SHOP);
  const { migs, migration } = gateNext(dir, GATE_KEEP);
  const writer = await connect(MIGRATIONS_URL, { maxConnections: 1, default: false, registry: models(GATE_KEEP + GATE_SHOP) });
  opened.push(writer);
  let upgrade: Promise<unknown> | undefined;
  let settled = false;
  await writer.transaction(async () => {
    await writer.execute("INSERT INTO shop (name) VALUES ('a')"); // not committed yet
    upgrade = new Migrator(db, migs).upgrade();
    upgrade.then(
      () => (settled = true),
      () => (settled = true),
    );
    await new Promise((resolve) => setTimeout(resolve, 500));
    assert.equal(settled, false); // the gate waits for the table lock
  });
  await assert.rejects(upgrade!, refusal(migration.name, "drops table shop", "shop"));
  assert.equal(await gateRows(db, "shop"), 1);
});

// -- no-transaction migrations ----------------------------------------------------------------------

const NO_TX_INDEX = "-- monorm: no-transaction create-index shop_name_idx\nCREATE UNIQUE INDEX CONCURRENTLY shop_name_idx ON shop (name);\n";
const NO_TX_DROP = "-- monorm: no-transaction drop-index shop_name_idx\nDROP INDEX CONCURRENTLY IF EXISTS shop_name_idx;\n";

function noTxFolder(dir: string, extra: Record<string, string> = {}, up = NO_TX_INDEX): Migrations {
  const folder = join(dir, "0002_shop_name_idx");
  mkdirSync(folder);
  writeFileSync(join(folder, "up.sql"), up);
  writeFileSync(join(folder, "down.sql"), NO_TX_DROP);
  for (const [file, text] of Object.entries(extra)) {
    writeFileSync(join(folder, file), text);
  }
  return new Migrations(dir, models(GATE_KEEP + GATE_SHOP));
}

async function indexValid(db: Database, name = "shop_name_idx"): Promise<string | null> {
  return scalar(db, `SELECT indisvalid FROM pg_index WHERE indexrelid = to_regclass('${name}')`);
}

test("a no-transaction migration builds an index concurrently", async () => {
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  await gateApply(db, dir, GATE_KEEP + GATE_SHOP);
  const migs = noTxFolder(dir);
  assert.deepEqual((await new Migrator(db, migs).upgrade()).map((m) => m.name), ["0002_shop_name_idx"]);
  assert.equal(await indexValid(db), "t");
  assert.equal(await gateRows(db, "monorm_migrations"), 2);
  assert.deepEqual(await new Migrator(db, migs).upgrade(), []);
});

test("a no-transaction migration repairs an invalid index", async () => {
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  await gateApply(db, dir, GATE_KEEP + GATE_SHOP);
  await db.execute("INSERT INTO shop (name) VALUES ('a'), ('a')");
  const migs = noTxFolder(dir);
  await assert.rejects(new Migrator(db, migs).upgrade(), (e) => e instanceof DatabaseError && /could not create unique index/.test(e.message));
  assert.equal(await indexValid(db), "f"); // the failed build leaves an invalid index
  assert.deepEqual((await new Migrator(db, migs).status()).map((s) => s.applied), [true, false]);
  await db.execute("DELETE FROM shop");
  assert.deepEqual((await new Migrator(db, migs).upgrade()).map((m) => m.name), ["0002_shop_name_idx"]);
  assert.equal(await indexValid(db), "t");
});

test("a no-transaction migration with a data step is refused", async () => {
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  await gateApply(db, dir, GATE_KEEP + GATE_SHOP);
  const migs = noTxFolder(dir, { "data.ts": "export async function run(db) {}\n" });
  await assert.rejects(
    new Migrator(db, migs).upgrade(),
    (e) => e instanceof MigrationError && e.message.includes("0002_shop_name_idx is a no-transaction migration and has a data step"),
  );
  assert.equal(await indexValid(db), null);
});

test("a no-transaction migration is refused on sqlite", async () => {
  const dir = tmp();
  const { db, prefix } = await gateDatabase("sqlite");
  await gateApply(db, dir, prefix + GATE_KEEP + GATE_SHOP);
  const migs = noTxFolder(dir);
  await assert.rejects(
    new Migrator(db, migs).upgrade(),
    (e) => e instanceof MigrationError && e.message.includes("0002_shop_name_idx is a no-transaction migration; sqlite has no such mode"),
  );
});

test("a no-transaction migration rolls back with its down file", async () => {
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  await gateApply(db, dir, GATE_KEEP + GATE_SHOP);
  const migrator = new Migrator(db, noTxFolder(dir));
  await migrator.upgrade();
  assert.deepEqual((await migrator.downgrade()).map((m) => m.name), ["0002_shop_name_idx"]);
  assert.equal(await indexValid(db), null);
  assert.equal(await gateRows(db, "monorm_migrations"), 1);
});

test("a no-transaction migration waits for another migrator", async () => {
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  await gateApply(db, dir, GATE_KEEP + GATE_SHOP);
  const migs = noTxFolder(dir);
  const other = await connect(MIGRATIONS_URL, { maxConnections: 1, default: false, registry: models(GATE_KEEP + GATE_SHOP) });
  opened.push(other);
  let upgrade: Promise<{ name: string }[]> | undefined;
  let settled = false;
  await other.transaction(async () => {
    await other.execute("SELECT pg_advisory_xact_lock(1869770093)"); // the lock of a normal migration
    upgrade = new Migrator(db, migs).upgrade();
    upgrade.then(
      () => (settled = true),
      () => (settled = true),
    );
    await new Promise((resolve) => setTimeout(resolve, 500));
    assert.equal(settled, false);
    assert.equal(await indexValid(db), null);
  });
  assert.deepEqual((await upgrade!).map((m) => m.name), ["0002_shop_name_idx"]);
  assert.equal(await gateRows(db, "monorm_migrations"), 2);
});

test("two migrators run one no-transaction migration once", async () => {
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  await gateApply(db, dir, GATE_KEEP + GATE_SHOP);
  const migs = noTxFolder(dir);
  const other = await connect(MIGRATIONS_URL, { maxConnections: 1, default: false, registry: models(GATE_KEEP + GATE_SHOP) });
  opened.push(other);
  let first: Promise<{ name: string }[]> | undefined;
  let second: Promise<{ name: string }[]> | undefined;
  let settled = 0;
  await other.transaction(async () => {
    await other.execute("SELECT pg_advisory_xact_lock(1869770093)");
    first = new Migrator(db, migs).upgrade();
    second = new Migrator(db, migs).upgrade();
    for (const upgrade of [first, second]) {
      upgrade.then(
        () => settled++,
        () => settled++,
      );
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
    assert.equal(settled, 0);
  });
  const done = [(await first!).map((m) => m.name), (await second!).map((m) => m.name)].sort();
  assert.deepEqual(done, [[], ["0002_shop_name_idx"]]); // the loser rechecks under the lock and does nothing
  assert.equal(await gateRows(db, "monorm_migrations"), 2);
  assert.equal(await indexValid(db), "t");
});

test("a normal migration waits for a session lock of a no-transaction migrator", async () => {
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  await gateApply(db, dir, GATE_KEEP + GATE_SHOP);
  const { migs, migration } = gateNext(dir, GATE_KEEP + GATE_SHOP + "\nmodel Extra {\n  id BigInt @id\n}\n");
  const other = await connect(MIGRATIONS_URL, { maxConnections: 1, default: false, registry: models(GATE_KEEP + GATE_SHOP) });
  opened.push(other);
  let upgrade: Promise<{ name: string }[]> | undefined;
  let settled = false;
  await other.transaction(async () => {
    await other.execute("SELECT pg_advisory_lock(1869770093)"); // one pinned connection holds the session lock
    upgrade = new Migrator(db, migs).upgrade();
    upgrade.then(
      () => (settled = true),
      () => (settled = true),
    );
    await new Promise((resolve) => setTimeout(resolve, 500));
    assert.equal(settled, false);
    await other.execute("SELECT pg_advisory_unlock(1869770093)");
  });
  assert.deepEqual((await upgrade!).map((m) => m.name), [migration.name]);
});

test("a no-transaction migration runs between normal migrations in order", async () => {
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  await gateApply(db, dir, GATE_KEEP + GATE_SHOP);
  noTxFolder(dir);
  const after = join(dir, "0003_after");
  mkdirSync(after);
  writeFileSync(join(after, "up.sql"), "CREATE TABLE after_tx (id integer);\n");
  writeFileSync(join(after, "down.sql"), "DROP TABLE after_tx;\n");
  const names = (await new Migrator(db, new Migrations(dir, models(GATE_KEEP + GATE_SHOP))).upgrade()).map((m) => m.name);
  assert.deepEqual(names.slice(-2), ["0002_shop_name_idx", "0003_after"]);
  assert.equal(await gateRows(db, "monorm_migrations"), 3);
  assert.equal(await indexValid(db), "t");
  const bad = join(dir, "0004_bad");
  mkdirSync(bad);
  writeFileSync(join(bad, "up.sql"), "CREATE TABLE half (id integer);\nSELECT 1 / 0;\n");
  writeFileSync(join(bad, "down.sql"), "DROP TABLE half;\n");
  await assert.rejects(new Migrator(db, new Migrations(dir, models(GATE_KEEP + GATE_SHOP))).upgrade(), (e) => e instanceof DatabaseError && e.message.includes("division by zero"));
  assert.equal(await scalar(db, "SELECT to_regclass('half')"), null); // the normal migration rolls back whole
  assert.equal(await gateRows(db, "monorm_migrations"), 3);
  assert.equal(await indexValid(db), "t"); // the no-transaction one stays recorded
});

test("a no-transaction down file is refused on sqlite and names the folder", async () => {
  const dir = tmp();
  const { db, prefix } = await gateDatabase("sqlite");
  await gateApply(db, dir, prefix + GATE_KEEP + GATE_SHOP);
  const migrator = new Migrator(db, noTxFolder(dir, {}, "CREATE TABLE extra (id integer);\n"));
  assert.deepEqual((await migrator.upgrade()).map((m) => m.name), ["0002_shop_name_idx"]);
  await assert.rejects(
    migrator.downgrade(),
    (e) => e instanceof MigrationError && e.message.includes("0002_shop_name_idx/down.sql is a no-transaction migration; sqlite has no such mode"),
  );
  assert.equal(await gateRows(db, "monorm_migrations"), 2);
});

test("a rollback waits for a no-transaction build without a deadlock", async () => {
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  await gateApply(db, dir, GATE_KEEP + GATE_SHOP);
  const after = join(dir, "0002_after");
  mkdirSync(after);
  writeFileSync(join(after, "up.sql"), "CREATE TABLE after_tx (id integer);\n");
  writeFileSync(join(after, "down.sql"), "DROP TABLE after_tx;\n");
  await new Migrator(db, new Migrations(dir, models(GATE_KEEP + GATE_SHOP))).upgrade();
  const folder = join(dir, "0003_shop_name_idx");
  mkdirSync(folder);
  writeFileSync(join(folder, "up.sql"), NO_TX_INDEX);
  writeFileSync(join(folder, "down.sql"), NO_TX_DROP);
  const migs = new Migrations(dir, models(GATE_KEEP + GATE_SHOP));
  const other = await connect(MIGRATIONS_URL, { maxConnections: 1, default: false, registry: models(GATE_KEEP + GATE_SHOP) });
  opened.push(other);
  let build: Promise<{ name: string }[]> | undefined;
  let rollback: Promise<{ name: string }[]> | undefined;
  let settled = 0;
  await other.transaction(async () => {
    await other.execute("INSERT INTO shop (name) VALUES ('w')"); // an open writer: the index build waits for it
    build = new Migrator(db, migs).upgrade();
    await new Promise((resolve) => setTimeout(resolve, 500));
    rollback = new Migrator(db, migs).downgrade(); // a normal step: waits for the migration lock
    for (const run of [build, rollback]) {
      run.then(
        () => settled++,
        () => settled++,
      );
    }
    await new Promise((resolve) => setTimeout(resolve, 1500)); // longer than deadlock_timeout (1 s)
    assert.equal(settled, 0);
  });
  assert.deepEqual((await build!).map((m) => m.name), ["0003_shop_name_idx"]);
  assert.deepEqual((await rollback!).map((m) => m.name), ["0002_after"]); // chosen before the build recorded its row
  assert.equal(await indexValid(db), "t");
});
