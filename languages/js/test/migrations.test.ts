/** Schema files, migration generation (no database) and running migrations against a
 * database of their own (migrations create and drop extensions, which are per database). */

import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";

import { DatabaseError, IntegrityError, MigrationError, Migrations, Migrator, Registry, SchemaError, connect, loads, type Database } from "../src/index.js";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { DATABASE_URL } from "./helpers.js";
import { appendOp, planned, plannedUp, writeMigration } from "./planned.js";

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
  @@trigger(touch, when: before, operation: [update], func: "BEGIN\n    NEW.updated_at := now();\n    RETURN NEW;\nEND;")
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
  const dir = tmp();
  const migs = new Migrations(dir, models(V1));
  const m = migs.make()!;
  assert.equal(m.name, "0001_initial");
  assert.equal(m.path, join(dir, "0001_initial.json"));
  assert.deepEqual(m.operations.map((op) => Object.keys(op)[0]), ["create_table", "create_table", "create_index"]);
  const up = m.upSql;
  assert.ok(up.indexOf('CREATE TABLE "authors"') < up.indexOf('CREATE TABLE "books"'));
  assert.ok(up.includes('CONSTRAINT "books_author_id_fkey" FOREIGN KEY ("author_id") REFERENCES "authors" ("id") ON DELETE CASCADE'));
  assert.ok(up.includes('CREATE INDEX "books_author_id_idx" ON "books" ("author_id");'));
  assert.ok(m.downSql.includes('DROP TABLE "books";'));
  assert.equal((JSON.parse(readFileSync(m.path, "utf8")) as { version: number }).version, 1);
  assert.deepEqual(readdirSync(dir), ["0001_initial.json"]);
  assert.equal(migs.make(), null); // nothing changed
});

test("the second migration alters in place", () => {
  const dir = tmp();
  new Migrations(dir, models(V1)).make();
  const migs = new Migrations(dir, models(V2));
  const plan = migs.plan();
  const [sql, down] = planned(migs);
  assert.equal(sql[0], 'CREATE EXTENSION IF NOT EXISTS "citext"');
  assert.equal(sql[1], 'CREATE EXTENSION IF NOT EXISTS "pg_trgm"');
  assert.ok(sql[2]!.startsWith('CREATE OR REPLACE FUNCTION "books_touch"() RETURNS trigger'));
  assert.ok(sql.includes('ALTER TABLE "books" RENAME COLUMN "title" TO "name"'));
  assert.ok(sql.includes('ALTER TABLE "books" ADD COLUMN "updated_at" timestamp with time zone DEFAULT now() NOT NULL'));
  assert.ok(sql.includes('ALTER TABLE "authors" ALTER COLUMN "email" TYPE citext USING "email"::citext'));
  assert.ok(sql.includes('ALTER TABLE "books" ADD CONSTRAINT "books_pages_check" CHECK (pages >= 0)'));
  assert.ok(sql.includes('CREATE INDEX "books_name_idx" ON "books" USING gin ("name" gin_trgm_ops)'));
  assert.ok(sql.at(-2)!.startsWith('CREATE TRIGGER "touch" BEFORE UPDATE ON "books" FOR EACH ROW EXECUTE FUNCTION "books_touch"()'));
  assert.equal(sql.at(-1), 'COMMENT ON COLUMN "books"."meta" IS \'free-form attributes\'');
  assert.ok(plan.operations.some((o) => o.warning?.includes("changes authors.email from varchar(254) to citext")));
  assert.ok(down.includes('ALTER TABLE "books" RENAME COLUMN "name" TO "title"'));
  assert.deepEqual(down.slice(-3), ['DROP FUNCTION "books_touch"()', 'DROP EXTENSION IF EXISTS "citext"', 'DROP EXTENSION IF EXISTS "pg_trgm"']);
  const m = migs.make("catalog changes")!;
  assert.equal(m.name, "0002_catalog_changes");
  assert.ok(m.upSql.includes("-- WARNING: changes authors.email"));
  assert.equal(migs.make(), null);
  assert.match(m.checksum, /^[0-9a-f]{64}$/);
});

test("plan operations say which ones lose data", () => {
  const dir = tmp();
  const first = new Migrations(dir, models(V1));
  assert.ok(!first.plan().operations.some((o) => o.destructive));
  first.make();
  const plan = new Migrations(dir, models(V2)).plan();
  assert.deepEqual(plan.operations.filter((o) => o.destructive).map((o) => o.summary), ["change type of authors.email to citext"]);
  assert.ok(plan.operations.every((o) => o.atomic));
  const m = new Migrations(dir, models(V2)).make("catalog")!;
  assert.ok(m.upSql.includes("-- WARNING: changes authors.email"));
  // the gate comes from the operations: no marker line is written
  assert.ok(!m.upSql.includes("monorm:") && !m.downSql.includes("monorm:") && !readFileSync(m.path, "utf8").includes("monorm:"));
});

// -- online migrations (migrations.online) ---------------------------------------------------------

const ONLINE_SHOP = (extra = "") => `model Shop {\n  id   BigInt @id @default(autoincrement())\n  name String\n${extra}\n  @@map("shop")\n}\n`;
const ONLINE_PLAIN = ONLINE_SHOP();
const ONLINE_INDEXED = ONLINE_SHOP("  @@index([name])\n");
const ONLINE_ITEM = (extra = "") => `model Item {\n  id      BigInt @id @default(autoincrement())\n  shop_id BigInt\n${extra}\n  @@map("item")\n}\n`;
const ONLINE_SHOP_WITH_ITEMS = 'model Shop {\n  id    BigInt @id @default(autoincrement())\n  name  String\n  items Item[]\n\n  @@map("shop")\n}\n';
const ONLINE_ITEM_FK = ONLINE_ITEM("  shop  Shop   @relation(fields: [shop_id], references: [id])\n");
const online = { online: true };

test("online makemigrations runs an index on an existing table outside a transaction", () => {
  const dir = tmp();
  new Migrations(dir, models(ONLINE_PLAIN), online).make();
  const migs = new Migrations(dir, models(ONLINE_INDEXED), online);
  assert.deepEqual(migs.plan().operations.map((o) => [o.summary, o.atomic]), [["create index shop_name_idx on shop", false]]);
  const first = migs.make("add_idx")!;
  assert.deepEqual(migs.all().map((m) => m.name), ["0001_initial", "0002_add_idx"]); // one change, one file
  assert.equal(first.operations[0]!["atomic"], false);
  assert.ok(first.upSql.startsWith("-- 0002_add_idx (up)\n\n-- outside a transaction\n"));
  assert.ok(first.upSql.includes('CREATE INDEX CONCURRENTLY IF NOT EXISTS "shop_name_idx" ON "shop" ("name");'));
  assert.ok(first.downSql.includes("DROP INDEX CONCURRENTLY IF EXISTS"));
  assert.equal(migs.make(), null); // nothing changed
  // a removed index and a new column: the index drop runs alone, the column in a group of its own
  const gone = new Migrations(dir, models(ONLINE_SHOP("  note String?\n")), online);
  const swapped = gone.make("swap")!;
  assert.equal(gone.all().at(-1)!.name, "0003_swap");
  assert.deepEqual(swapped.operations.map((op) => [Object.keys(op)[0], op["atomic"] ?? true]), [["drop_index", false], ["add_column", true]]);
  assert.equal(gone.plan().operations.length, 0);
});

test("online makemigrations adds a foreign key in the main group", () => {
  const dir = tmp();
  new Migrations(dir, models(ONLINE_PLAIN + ONLINE_ITEM()), online).make();
  const v2 = ONLINE_SHOP_WITH_ITEMS + ONLINE_ITEM_FK;
  const migs = new Migrations(dir, models(v2), online);
  assert.deepEqual(migs.plan().operations.map((o) => o.atomic), [true]);
  const [up, down] = planned(migs);
  assert.equal(up.length, 1);
  assert.ok(up[0]!.includes("FOREIGN KEY") && !up[0]!.includes("NOT VALID"), up[0]);
  assert.deepEqual(down, ['ALTER TABLE "item" DROP CONSTRAINT "item_shop_id_fkey"']);
  const written = migs.make("add_fk")!;
  assert.deepEqual(migs.all().map((m) => m.name), ["0001_initial", "0002_add_fk"]); // one file, not two
  assert.deepEqual(written.operations.map((op) => Object.keys(op)[0]), ["add_constraint"]);
  assert.equal(migs.plan().operations.length, 0);
  assert.equal(new Migrations(dir, models(v2)).plan().operations.length, 0); // the file ends in the same schema
});

test("online makemigrations leaves a new table alone", () => {
  const dir = tmp();
  const v1 = ONLINE_SHOP_WITH_ITEMS + ONLINE_ITEM_FK;
  const migs = new Migrations(dir, models(v1), online);
  const made = migs.make()!;
  assert.deepEqual(migs.all().map((m) => m.name), ["0001_initial"]);
  assert.ok(!made.upSql.includes("CONCURRENTLY") && !made.upSql.includes("NOT VALID"));
  assert.ok(made.upSql.includes("FOREIGN KEY"));
  const off = join(dir, "off");
  mkdirSync(off);
  const plain = new Migrations(off, models(v1)).make()!;
  assert.equal(plain.upSql.replace(/^-- .*\n/, ""), made.upSql.replace(/^-- .*\n/, ""));
  assert.equal(plain.downSql.replace(/^-- .*\n/, ""), made.downSql.replace(/^-- .*\n/, ""));
  // a table added next to an existing one, with an index: still one file
  const more = v1 + 'model Tag {\n  id   BigInt @id\n  name String\n\n  @@index([name])\n  @@map("tag")\n}\n';
  const migs2 = new Migrations(dir, models(more), online);
  const tag = migs2.make()!;
  assert.equal(migs2.all().length, 2);
  assert.ok(tag.upSql.includes('CREATE INDEX "tag_name_idx" ON "tag" ("name")'));
  assert.ok(!tag.upSql.includes("CONCURRENTLY"));
});

test("online makemigrations changes nothing on sqlite", () => {
  const source = 'datasource db { provider = "sqlite" }\n';
  const [on, off] = [join(tmp(), "on"), join(tmp(), "off")];
  mkdirSync(on);
  mkdirSync(off);
  for (const [dir, flag] of [[on, true], [off, false]] as const) {
    new Migrations(dir, models(source + ONLINE_PLAIN), { online: flag }).make();
    new Migrations(dir, models(source + ONLINE_INDEXED), { online: flag }).make();
  }
  const [a, b] = [new Migrations(on, new Registry()).all(), new Migrations(off, new Registry()).all()];
  assert.deepEqual(a.map((m) => m.name), b.map((m) => m.name));
  assert.equal(a.length, 2);
  a.forEach((m, i) => assert.equal(readFileSync(m.path, "utf8"), readFileSync(b[i]!.path, "utf8")));
  assert.ok(!readFileSync(a[1]!.path, "utf8").includes("atomic"));
});

test("the CLI online setting", () => {
  const dir = tmp();
  writeFileSync(join(dir, "schema.prisma"), ONLINE_PLAIN);
  writeFileSync(join(dir, "monorm.toml"), "[migrations]\nonline = true\n");
  const run = (...args: string[]) => cli(args, dir);
  assert.equal(run("makemigrations").code, 0);
  writeFileSync(join(dir, "schema.prisma"), ONLINE_INDEXED);
  assert.equal(run("makemigrations", "--check").code, 1);
  const made = run("makemigrations");
  assert.equal(made.code, 0);
  assert.ok(made.out.includes("0002_create_index_shop_name_idx_on_shop"));
  assert.equal(run("makemigrations", "--check").code, 0);
  const file = readdirSync(join(dir, "migrations")).find((f) => f.startsWith("0002_"))!;
  assert.ok(readFileSync(join(dir, "migrations", file), "utf8").includes('"atomic": false'));
});

test("migrations from a schema file", () => {
  const dir = tmp();
  writeFileSync(join(dir, "schema.prisma"), V1);
  assert.equal(new Migrations(join(dir, "migrations"), join(dir, "schema.prisma")).make() !== null, true);
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
  appendOp(m, { run_sql: { sql: "SELECT 1", reverse_sql: "noop" } });
  new Migrations(dir, models(V2)).make();
  await assert.rejects(migrator.upgrade(), (e: unknown) => e instanceof MigrationError && /changed after it was applied/.test(e.message));
});

test("a failed migration rolls back", async () => {
  const dir = tmp();
  const db = await database(models(V1));
  const migs = new Migrations(dir, models(V1));
  const m = migs.make()!;
  appendOp(m, { run_sql: { sql: "SELECT no_such_function()", reverse_sql: "noop" } });
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
  const history = readFileSync(join(dir, "migrations/0001_initial.json"), "utf8");
  writeFileSync(path, old.replace(" qty Int", " note String?\n qty Int"));
  const result = cli(["fmt", "--native"], dir);
  assert.equal(result.code, 0, result.err);
  const converted = readFileSync(path, "utf8");
  assert.ok(converted.includes("note String?"));
  assert.ok(converted.includes("@check((qty > 0))"));
  assert.equal(cli(["makemigrations", "--check"], dir).code, 1);
  assert.equal(cli(["fmt", "--native"], dir).code, 0);
  assert.equal(readFileSync(path, "utf8"), converted);
  assert.equal(readFileSync(join(dir, "migrations/0001_initial.json"), "utf8"), history);
});

// -- the destructive gate ---------------------------------------------------------------------------

const GATE_SQLITE = 'datasource db { provider = "sqlite" }\n';
const GATE_KEEP = 'model Keep {\n  id BigInt @id @default(autoincrement())\n\n  @@map("keep")\n}\n';
const GATE_SHOP = 'model Shop {\n  id   BigInt @id @default(autoincrement())\n  name String\n  note String?\n\n  @@map("shop")\n}\n';
const GATE_TABLES = ["item", "shop", "venue", "keep", "ghost_done", "after_tx", "half", "extra", "monorm_migrations"];
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
    assert.deepEqual(migration.operations.map((op) => Object.keys(op)[0]), ["drop_table"]);
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
  });

  test(`destructive gate ignores a migration with no destructive operation (${dialect})`, async () => {
    const dir = tmp();
    const { db, prefix } = await gateDatabase(dialect);
    await gateApply(db, dir, prefix + GATE_KEEP + GATE_SHOP);
    await db.execute("INSERT INTO shop (name) VALUES ('a')");
    const wider = GATE_SHOP.replace("  note String?", "  note String?\n  extra String?");
    const added = await gateApply(db, dir, prefix + GATE_KEEP + wider);
    assert.ok(!readFileSync(added.path, "utf8").includes("drop"));
    assert.equal(await gateRows(db, "shop"), 1);
  });

  test(`destructive gate sees a drop after a table rename (${dialect})`, async () => {
    const dir = tmp();
    const { db, prefix } = await gateDatabase(dialect);
    await gateApply(db, dir, prefix + GATE_KEEP + GATE_SHOP);
    await db.execute("INSERT INTO shop (name, note) VALUES ('a', 'n')");
    const venue = GATE_SHOP.replace('@@map("shop")', '@@map("venue")\n  @@renamed_from("shop")').replace("model Shop", "model Venue").replace("  note String?\n", "");
    const { migs, migration } = gateNext(dir, prefix + GATE_KEEP + venue);
    // the gate names the table as the database has it before the migration
    const kinds = migration.operations.map((op) => Object.keys(op)[0]);
    assert.ok(kinds[0] === "rename_table" && kinds.includes("drop_column"));
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
    assert.deepEqual(migration.operations.map((op) => Object.keys(op)[0]), ["create_table"]);
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

const NO_TX_INDEX = { create_index: { table: "shop", index: { name: "shop_name_idx", unique: true, keys: [{ column: "name" }] } }, atomic: false };
const AFTER_TX = { run_sql: { sql: "CREATE TABLE after_tx (id integer)", reverse_sql: "DROP TABLE after_tx" } };

/** Writes a migration file by hand; its index operation runs outside a transaction. */
function noTxFolder(dir: string, name = "0002_shop_name_idx", ops: Record<string, unknown>[] = [NO_TX_INDEX]): Migrations {
  writeMigration(dir, name, ops);
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
  // the migration stopped inside an operation outside a transaction: a plain run refuses it
  await assert.rejects(
    new Migrator(db, migs).upgrade(),
    (e) => e instanceof MigrationError && e.message.includes("continue it with `migrate 0002_shop_name_idx --ops 1-1`"),
  );
  await db.execute("DELETE FROM shop");
  const done = await new Migrator(db, migs).upgrade("0002_shop_name_idx", { ops: "1-1" }); // the invalid index is dropped, then built
  assert.deepEqual(done.map((m) => m.name), ["0002_shop_name_idx"]);
  assert.equal(await indexValid(db), "t");
  assert.deepEqual((await new Migrator(db, migs).status()).map((s) => s.applied), [true, true]);
});

test("a no-transaction operation with code is refused", async () => {
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  await gateApply(db, dir, GATE_KEEP + GATE_SHOP);
  writeFileSync(join(dir, "d.ts"), "export async function run(db) {}\n");
  const migs = noTxFolder(dir, "0002_shop_name_idx", [{ run_code: { typescript: "d.ts:run", reverse: "noop" }, atomic: false }]);
  await assert.rejects(
    new Migrator(db, migs).upgrade(),
    (e) => e instanceof MigrationError && /0002_shop_name_idx: operation 1.*run_code.*atomic/.test(e.message),
  );
  assert.equal(await gateRows(db, "monorm_migrations"), 1);
});

test("a no-transaction migration is refused on sqlite", async () => {
  const dir = tmp();
  const { db, prefix } = await gateDatabase("sqlite");
  await gateApply(db, dir, prefix + GATE_KEEP + GATE_SHOP);
  const migs = noTxFolder(dir);
  await assert.rejects(
    new Migrator(db, migs).upgrade(),
    (e) => e instanceof MigrationError && /0002_shop_name_idx: operation 1: .atomic.: false is not supported on SQLite/.test(e.message),
  );
});

test("a no-transaction migration rolls back with its reverse", async () => {
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
  writeMigration(dir, "0003_after", [AFTER_TX]);
  const names = (await new Migrator(db, new Migrations(dir, models(GATE_KEEP + GATE_SHOP))).upgrade()).map((m) => m.name);
  assert.deepEqual(names.slice(-2), ["0002_shop_name_idx", "0003_after"]);
  assert.equal(await gateRows(db, "monorm_migrations"), 3);
  assert.equal(await indexValid(db), "t");
  writeMigration(dir, "0004_bad", [
    { run_sql: { sql: "CREATE TABLE half (id integer)", reverse_sql: "DROP TABLE half" } },
    { run_sql: { sql: "SELECT 1 / 0", reverse_sql: "noop" } },
  ]);
  await assert.rejects(new Migrator(db, new Migrations(dir, models(GATE_KEEP + GATE_SHOP))).upgrade(), (e) => e instanceof DatabaseError && e.message.includes("division by zero"));
  assert.equal(await scalar(db, "SELECT to_regclass('half')"), null); // the normal migration rolls back whole
  assert.equal(await gateRows(db, "monorm_migrations"), 3);
  assert.equal(await indexValid(db), "t"); // the no-transaction one stays recorded
});

test("a rollback waits for a no-transaction build without a deadlock", async () => {
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  await gateApply(db, dir, GATE_KEEP + GATE_SHOP);
  writeMigration(dir, "0002_after", [AFTER_TX]);
  await new Migrator(db, new Migrations(dir, models(GATE_KEEP + GATE_SHOP))).upgrade();
  const migs = noTxFolder(dir, "0003_shop_name_idx");
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
  assert.deepEqual((await rollback!).map((m) => m.name), ["0003_shop_name_idx"]); // planned after the build finished
  assert.equal(await indexValid(db), null); // the rollback dropped the built index
});

async function onlineSchema(db: Database): Promise<[string[], string[]]> {
  const indexes = await db.fetchText("SELECT indexdef FROM pg_indexes WHERE tablename IN ('shop', 'item') ORDER BY 1");
  const constraints = await db.fetchText(
    "SELECT conname || ' ' || pg_get_constraintdef(oid) || ' ' || convalidated FROM pg_constraint WHERE conrelid IN ('shop'::regclass, 'item'::regclass) ORDER BY 1",
  );
  return [indexes.map((r) => r[0]!), constraints.map((r) => r[0]!)];
}

test("online migrate ends with no drift", async () => {
  const first = ONLINE_PLAIN + ONLINE_ITEM();
  const last = ONLINE_SHOP_WITH_ITEMS.replace("  @@map", "  @@index([name])\n  @@map") + ONLINE_ITEM_FK;
  const ends = new Map<boolean, [string[], string[]]>();
  const dir = tmp();
  const { db } = await gateDatabase("postgres");
  let migrator!: Migrator;
  for (const flag of [false, true]) { // the database ends in the online run
    const directory = join(dir, String(flag));
    mkdirSync(directory);
    await db.execute("DROP TABLE IF EXISTS item, shop, monorm_migrations");
    new Migrations(directory, models(first)).make();
    await new Migrator(db, new Migrations(directory, models(first))).upgrade();
    await db.execute("INSERT INTO shop (name) VALUES ('a'), ('b'); INSERT INTO item (shop_id) VALUES (1), (2)");
    const migs = new Migrations(directory, models(last), { online: flag });
    migs.make();
    assert.equal(migs.all().length, 2); // one change, one file, online or not
    migrator = new Migrator(db, migs);
    assert.equal((await migrator.upgrade()).length, migs.all().length - 1);
    assert.equal((await migrator.drift()).steps.length, 0);
    assert.equal(await gateRows(db, "shop"), 2);
    assert.equal(await gateRows(db, "item"), 2);
    ends.set(flag, await onlineSchema(db));
    assert.deepEqual(await new Migrator(db, migs).upgrade(), []);
  }
  assert.deepEqual(ends.get(true), ends.get(false));
  const [indexes, constraints] = ends.get(true)!;
  assert.ok(indexes.some((i) => i.includes("shop_name_idx")));
  assert.ok(constraints.some((c) => c.includes("item_shop_id_fkey") && c.endsWith(" true")));
  // one migration holds the index and the foreign key: it rolls back whole
  assert.equal((await migrator.downgrade({ steps: 1 })).length, 1);
  const [afterIndexes, afterConstraints] = await onlineSchema(db);
  assert.ok(!afterIndexes.some((i) => i.includes("shop_name_idx")) && !afterConstraints.some((c) => c.includes("item_shop_id_fkey")));
  assert.equal((await migrator.upgrade()).length, 1);
  assert.equal((await migrator.drift()).steps.length, 0);
});

// -- extension types and enums at runtime ---------------------------------------------------------

// case: migrations_db::extension_types_at_runtime
test("extension types at runtime", async (t) => {
  const reg = models(`
    model Doc {
      id        String   @id @default(dbgenerated("gen_random_uuid()")) @db.Uuid
      email     String   @db.Citext
      attrs     Json?
      embedding Unsupported("vector(3)")?

      @@map("ext_docs")
    }
  `);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const Doc = reg.get("Doc").model as any;
  const db = await connect(MIGRATIONS_URL, { maxConnections: 2, default: false, registry: reg });
  opened.push(db);
  await db.execute("DROP TABLE IF EXISTS ext_docs");
  try {
    try {
      await db.createTables();
    } catch (e) {
      if (e instanceof DatabaseError) {
        t.skip(`extension not installed: ${e.message}`);
        return;
      }
      throw e;
    }
    await db.createTables(); // idempotent
    const d = await Doc.objects.using(db).insert({ email: "Bob@X.io", attrs: { tags: ["a", "b"], n: 1, ok: true, x: null }, embedding: [1, 2.5, 3] });
    assert.match(d.id, /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
    assert.deepEqual(d.attrs, { tags: ["a", "b"], n: 1, ok: true, x: null });
    assert.deepEqual(d.embedding, [1, 2.5, 3]);
    const got = await Doc.objects.using(db).get(Doc.email.eq("bob@x.IO"));
    assert.deepEqual(got.embedding, [1, 2.5, 3]);
    assert.equal(got.id, d.id);
    assert.equal(await Doc.objects.using(db).filter(Doc.id.eq(String(d.id))).exists(), true);
    assert.equal(await Doc.objects.using(db).filter(Doc.email.in(["BOB@x.io"])).count(), 1);
    await got.update({ embedding: [0, 0, 1], attrs: [1, 2] });
    assert.deepEqual([got.embedding, got.attrs], [[0, 0, 1], [1, 2]]);
    await Doc.objects.using(db).insert({ email: "no@x.io" });
    assert.equal(await Doc.objects.using(db).filter(Doc.embedding.eq(null)).count(), 1);
  } finally {
    await db.execute("DROP TABLE IF EXISTS ext_docs");
  }
});

const ENUM_V1 = `
enum Status {
  draft
  published
}

model Doc {
  id      BigInt   @id @default(autoincrement())
  status  Status   @default(draft)
  history Status[] @default([draft])

  @@map("docs")
}
`;
// a value added in the middle and one at the end
const ENUM_V2 = ENUM_V1.replace("  draft\n  published\n", "  draft\n  review\n  published\n  archived\n");
// `review` removed again: the type is recreated
const ENUM_V3 = ENUM_V1.replace("  draft\n  published\n", "  draft\n  published\n  archived\n");

// case: migrations_db::enum_migrations
test("enum migrations", async () => {
  const dir = tmp();
  const regs = [ENUM_V1, ENUM_V2, ENUM_V3].map(models);
  const db = await connect(MIGRATIONS_URL, { maxConnections: 2, default: false, registry: regs[2]! });
  opened.push(db);
  const drop = () => db.execute("DROP TABLE IF EXISTS docs, monorm_migrations CASCADE; DROP TYPE IF EXISTS status, status_old");
  await drop();
  try {
    regs.forEach((reg, i) => new Migrations(dir, reg).make(`v${i + 1}`));
    const migrator = new Migrator(db, new Migrations(dir, regs[2]!));
    await migrator.upgrade("0002");
    const labels = "SELECT string_agg(enumlabel, ',' ORDER BY enumsortorder) FROM pg_enum JOIN pg_type t ON t.oid = enumtypid WHERE typname = 'status'";
    assert.equal(await scalar(db, labels), "draft,review,published,archived");
    await db.execute("INSERT INTO docs (status, history) VALUES ('published', '{draft,published}'), ('archived', DEFAULT)");
    await migrator.upgrade();
    assert.equal(await scalar(db, labels), "draft,published,archived");
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const Doc = regs[2]!.get("Doc").model as any;
    const docs = await Doc.objects.using(db).orderBy(Doc.id).all();
    assert.deepEqual(docs.map((d: { status: string; history: string[] }) => [d.status, d.history]), [
      ["published", ["draft", "published"]],
      ["archived", ["draft"]],
    ]);
    // a row still using a value that a migration removes makes it fail (and roll back)
    await migrator.downgrade(); // back to v2: recreated with review again
    await db.execute("UPDATE docs SET status = 'review' WHERE id = 1");
    await assert.rejects(migrator.upgrade(), (e: unknown) => e instanceof DatabaseError && /invalid input value for enum/.test(e.message));
    assert.equal(await scalar(db, labels), "draft,review,published,archived");
    await db.execute("UPDATE docs SET status = 'draft'");
    await migrator.downgrade({ target: "zero", allowDestructive: true });
    assert.equal(await scalar(db, "SELECT to_regtype('status')::text"), null);
  } finally {
    await drop();
  }
});
