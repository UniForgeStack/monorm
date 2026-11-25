/** Code steps: a `run_code` operation names `file.ts:run` (`export async function run(db)`) next to the migration file. */

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, unlinkSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

import { Migrations, Migrator, Registry, connect, type Database, type Migration } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";
import { appendOp, writeMigration } from "./planned.js";

const DATA_DB = new URL(DATABASE_URL).pathname.slice(1) + "_data_js";
const DATA_URL = Object.assign(new URL(DATABASE_URL), { pathname: "/" + DATA_DB }).toString();

const V1 = `
model Author {
  id   BigInt @id @default(autoincrement())
  name String
}
`;
const V2 = V1.replace("name String", 'name String\n  slug String @default("")');

const FILL = `
export async function run(db) {
  // the new column is visible only inside the migration's transaction
  await db.execute("UPDATE author SET slug = lower(name)");
  const rows = await db.fetchText("SELECT count(*) FROM author WHERE slug <> ''");
  if (rows[0][0] !== "2") throw new Error("expected 2 rows, got " + rows[0][0]);
}
`;

before(async () => {
  const admin = await connect(DATABASE_URL, { maxConnections: 1, default: false, registry: new Registry() });
  try {
    if (!(await admin.fetchText(`SELECT 1 FROM pg_database WHERE datname = '${DATA_DB}'`)).length) {
      await admin.execute(`CREATE DATABASE "${DATA_DB}"`);
    }
  } finally {
    await admin.close();
  }
});

const opened: Database[] = [];
after(async () => {
  for (const db of opened) await db.close();
});

/** Adds a TypeScript `run_code` operation (and its script) to a written migration. */
function addCode(migrations: Migrations, migration: Migration, file: string, text: string, reverse: unknown = "noop"): string {
  const script = join(migrations.directory, file);
  writeFileSync(script, text);
  appendOp(migration, { run_code: { typescript: `${file}:run`, reverse } });
  return script;
}

test("a data step runs in the migration transaction", async () => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-data-"));
  const schema = join(dir, "schema.prisma");
  const migrations = new Migrations(join(dir, "migrations"), schema);
  writeFileSync(schema, V1);
  migrations.make();
  writeFileSync(schema, V2);
  const second = migrations.make("slug")!;
  addCode(migrations, second, "0002_slug.ts", FILL);

  const db = await connect(DATA_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute("DROP TABLE IF EXISTS author, extra, monorm_migrations CASCADE");
  const migrator = new Migrator(db, migrations);
  assert.deepEqual((await migrator.upgrade("1")).map((m) => m.name), ["0001_initial"]);
  await db.execute("INSERT INTO author (name) VALUES ('Ann'), ('Bob')");
  await assert.rejects(db.engine.migrateUp(migrations.directory, null, false), /0002_slug: operation 2 runs code/);
  assert.deepEqual((await migrator.upgrade()).map((m) => m.name), ["0002_slug"]);
  assert.deepEqual(await db.fetchText("SELECT slug FROM author ORDER BY id"), [["ann"], ["bob"]]);

  // a failing data step rolls back its SQL and is not recorded
  writeFileSync(schema, V2 + "\nmodel Extra {\n  id BigInt @id\n}\n");
  const third = migrations.make("extra")!;
  addCode(migrations, third, "0003_extra.ts", 'export async function run(db) { throw new Error("data step failed"); }\n');
  await assert.rejects(migrator.upgrade(), /data step failed/);
  assert.deepEqual(await db.fetchText("SELECT to_regclass('extra')"), [[null]]);
  assert.deepEqual((await migrator.status()).map((s) => s.applied), [true, true, false]);
});

// `npx monorm` as a process of its own: src/cli.ts (Bun) or dist/src/cli.js (Node)
const CLI = [".js", ".ts"].map((ext) => fileURLToPath(new URL(`../src/cli${ext}`, import.meta.url))).find(existsSync)!;

test("cli migrate runs data steps", () => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-data-cli-"));
  const sqlite = 'datasource db {\n  provider = "sqlite"\n}\n';
  const schema = join(dir, "schema.prisma");
  const migrations = new Migrations(join(dir, "migrations"), schema);
  writeFileSync(schema, sqlite + V1);
  migrations.make();
  writeFileSync(schema, sqlite + V2);
  const second = migrations.make("slug")!;
  addCode(
    migrations,
    second,
    "0002_slug.ts",
    "export async function run(db) {\n  await db.execute(\"INSERT INTO author (name, slug) VALUES ('Ann', 'ann')\");\n}\n",
  );
  const env = { ...process.env, MONORM_DATABASE_URL: `sqlite://${join(dir, "app.db")}` };
  const run = (...args: string[]) =>
    spawnSync(process.execPath, [CLI, "--schema", "schema.prisma", "--dir", "migrations", ...args], { cwd: dir, env, encoding: "utf8" });
  const done = run("migrate");
  assert.equal(done.status, 0, done.stderr);
  assert.deepEqual(done.stdout.trim().split("\n"), ["Applied 0001_initial", "Applied 0002_slug"]);
  const shown = run("showmigrations").stdout;
  assert.ok(shown.includes("[x] 0001_initial") && shown.includes("[x] 0002_slug"), shown);

  dataMigration(migrations, "0003_bad", "data.ts", 'export async function run() { throw new Error("bad data"); }\n');
  const failed = run("migrate");
  assert.equal(failed.status, 1);
  assert.ok(failed.stderr.includes("error: bad data"), failed.stderr);
});

async function fresh(): Promise<{ db: Database; migrations: Migrations; dir: string }> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-data-"));
  const schema = join(dir, "schema.prisma");
  const migrations = new Migrations(join(dir, "migrations"), schema);
  writeFileSync(schema, V1);
  migrations.make();
  const db = await connect(DATA_URL, { maxConnections: 3, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute("DROP TABLE IF EXISTS author, extra, seen, monorm_migrations CASCADE; CREATE TABLE seen (n bigint)");
  return { db, migrations, dir };
}

/** Writes the migration `name`: one `run_sql` operation, then a code step in the language of `file`. */
function dataMigration(migrations: Migrations, name: string, file: string, text: string): string {
  const extension = file.slice(file.lastIndexOf("."));
  const script = join(migrations.directory, `${name}${extension}`);
  writeFileSync(script, text);
  const language = extension === ".py" ? "python" : "typescript";
  writeMigration(migrations.directory, name, [
    { run_sql: { sql: "SELECT 1", reverse_sql: "noop" } },
    { run_code: { [language]: `${name}${extension}:run`, reverse: "noop" } },
  ]);
  return script;
}

const INDEX = JSON.stringify(fileURLToPath(new URL("../src/index.js", import.meta.url)));

test("a data step can nest transactions and register onCommit callbacks", async () => {
  const { db, migrations } = await fresh();
  dataMigration(
    migrations,
    "0002_nested",
    "data.ts",
    `import { Registry, connect } from ${INDEX};
export async function run(db) {
  await db.transaction(async () => { await db.execute("INSERT INTO author (name) VALUES ('Cy')"); });
  await db.onCommit(async () => {
    // another connection sees the row only after the commit
    const other = await connect(db.url, { maxConnections: 1, default: false, registry: new Registry() });
    try { await other.execute("INSERT INTO seen (n) SELECT count(*) FROM author"); } finally { await other.close(); }
  });
}
`,
  );
  assert.deepEqual((await new Migrator(db, migrations).upgrade()).map((m) => m.name), ["0001_initial", "0002_nested"]);
  assert.deepEqual(await db.fetchText("SELECT n FROM seen"), [["1"]]);

  // a failing data step drops its callbacks and releases the migration lock
  dataMigration(
    migrations,
    "0003_rolled_back",
    "data.ts",
    `export async function run(db) {
  await db.onCommit(() => { throw new Error("onCommit ran for a rolled-back migration"); });
  await db.transaction(async () => { await db.execute("UPDATE author SET name = name"); });
  throw new Error("after onCommit");
}
`,
  );
  for (let i = 0; i < 2; i++) await assert.rejects(new Migrator(db, migrations).upgrade(), /after onCommit/);
  assert.deepEqual((await new Migrator(db, migrations).status()).map((s) => s.applied), [true, true, false]);
});

test("data steps are checked before anything runs", async () => {
  const { db, migrations } = await fresh();
  dataMigration(migrations, "0002_py", "data.py", "async def run(db):\n    pass\n");
  await assert.rejects(new Migrator(db, migrations).upgrade(), /0002_py: operation 2 has no run_code entry for typescript/);
  assert.deepEqual(await db.fetchText("SELECT to_regclass('author')"), [[null]]);

  const { db: db2, migrations: m2 } = await fresh();
  dataMigration(m2, "0002_no_run", "data.ts", "export const value = 1;\n");
  await assert.rejects(new Migrator(db2, m2).upgrade(), /needs `export async function run\(db\)`/);
  assert.deepEqual(await db2.fetchText("SELECT to_regclass('author')"), [[null]]);
});

// Bun caches a module by path and ignores the version query
test("an edited data step loads again in the same process", { skip: "bun" in process.versions }, async () => {
  const { db, migrations } = await fresh();
  const step = (v: string) => `export async function run(db) { await db.execute("INSERT INTO author (name) VALUES ('${v}')"); }\n`;
  const script = dataMigration(migrations, "0002_edit", "data.ts", step("v1"));
  const migrator = new Migrator(db, migrations);
  await migrator.upgrade();
  await migrator.downgrade();
  writeFileSync(script, step("v2"));
  utimesSync(script, new Date(), new Date(Date.now() + 5000));
  await migrator.upgrade();
  assert.deepEqual(await db.fetchText("SELECT name FROM author ORDER BY id"), [["v1"], ["v2"]]);
});

test("concurrent upgrades run a data step once", async () => {
  const { db, migrations } = await fresh();
  await new Migrator(db, migrations).upgrade();
  dataMigration(
    migrations,
    "0002_slow",
    "data.ts",
    `export async function run(db) {
  await db.execute("INSERT INTO author (name) VALUES ('once')");
  await new Promise((r) => setTimeout(r, 500));
}
`,
  );
  const results = await Promise.all([new Migrator(db, migrations).upgrade(), new Migrator(db, migrations).upgrade()]);
  assert.deepEqual(results.map((r) => r.map((m) => m.name)).sort((a, b) => a.length - b.length), [[], ["0002_slow"]]);
  assert.deepEqual(await db.fetchText("SELECT count(*) FROM author"), [["1"]]);
});

test("cli migrate needs no schema file and skips applied data steps", () => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-data-cli-"));
  const schema = join(dir, "schema.prisma");
  const migrations = new Migrations(join(dir, "migrations"), schema);
  writeFileSync(schema, 'datasource db {\n  provider = "sqlite"\n}\n' + V1);
  migrations.make();
  const old = dataMigration(migrations, "0002_old", "data.ts", "export async function run() {}\n");
  const env = { ...process.env, MONORM_DATABASE_URL: `sqlite://${join(dir, "app.db")}` };
  const run = (...args: string[]) =>
    spawnSync(process.execPath, [CLI, "--schema", "schema.prisma", "--dir", "migrations", ...args], { cwd: dir, env, encoding: "utf8" });
  assert.equal(run("migrate").status, 0);
  // an applied data step whose import fails today must not stop later migrations
  writeFileSync(old, 'import "./gone.js";\nexport async function run() {}\n');
  dataMigration(migrations, "0003_new", "data.ts", "export async function run(db) { await db.execute(\"INSERT INTO author (name) VALUES ('x')\"); }\n");
  unlinkSync(schema);
  const done = run("migrate");
  assert.equal(done.status, 0, done.stderr);
  assert.deepEqual(done.stdout.trim().split("\n"), ["Applied 0003_new"]);
});

test("npx monorm migrate with a data step and no database is a usage error", () => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-data-cli-"));
  const schema = join(dir, "schema.prisma");
  const migrations = new Migrations(join(dir, "migrations"), schema);
  writeFileSync(schema, V1);
  migrations.make();
  dataMigration(migrations, "0002_step", "data.ts", "export async function run() {}\n");
  const env = { ...process.env, MONORM_DATABASE_URL: "" };
  const done = spawnSync(process.execPath, [CLI, "--schema", "schema.prisma", "--dir", "migrations", "migrate"], { cwd: dir, env, encoding: "utf8" });
  assert.equal(done.status, 2, done.stderr);
  assert.ok(done.stderr.includes("no database"), done.stderr);
});
