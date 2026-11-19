/** Native checks: a bad row is rejected by the database, and each dialect gives the same answer. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { connect, IntegrityError, loads, Migrations, Migrator, pull, Registry } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const PROVIDERS = { postgres: ["postgresql", DATABASE_URL], sqlite: ["sqlite", "sqlite://:memory:"] } as const;
type Provider = keyof typeof PROVIDERS;

const source = (provider: string, table: string, check: string, extra = "") =>
  `datasource db {\n  provider = "${provider}"\n}\n` +
  `model Stock {\n  id Int @id @default(autoincrement())\n  qty Int\n  reserved Int\n  name String\n${extra}` +
  `  @@map("${table}")\n  @@check(${check})\n}\n`;

async function opened(name: Provider, check: string, extra = "") {
  const [provider, url] = PROVIDERS[name];
  const registry = new Registry();
  const classes = loads(source(provider, `pl12_stock_js_${name}`, check, extra), { registry }) as Record<string, Any>;
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  return { db, objects: classes["Stock"].objects.using(db) };
}

for (const dialect of Object.keys(PROVIDERS) as Provider[]) {
  // case: schema_expressions::a_native_check_rejects_a_bad_row
  test(`a native check rejects a bad row (${dialect})`, async () => {
    const { db, objects } = await opened(dialect, "reserved <= qty & qty >= 0");
    try {
      await objects.insert({ qty: 5, reserved: 2, name: "ok" });
      for (const bad of [{ qty: 1, reserved: 2 }, { qty: -1, reserved: -2 }]) {
        await assert.rejects(objects.insert({ name: "bad", ...bad }), IntegrityError);
      }
      assert.equal(await objects.count(), 1);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}

const ROW = { qty: 1, reserved: 0, name: "n" };
const utc = (...parts: [number, number, number, number?]) => new Date(Date.UTC(parts[0], parts[1] - 1, parts[2], parts[3] ?? 0));
// check, a row it accepts, a row it rejects (null: it rejects none). Each once gave a different answer on SQLite.
const SAME_ON_BOTH: Record<string, [string, Record<string, unknown>, Record<string, unknown> | null]> = {
  "datetime literal": ['seen > "2024-01-01T00:00:00Z"', { seen: utc(2024, 1, 1, 12) }, { seen: utc(2023, 12, 31, 12) }],
  "empty suffix": ['name.endswith("")', { name: "abc" }, null],
  "escaped ilike": [String.raw`name.ilike("50\\%")`, { name: "50%" }, { name: "500" }],
};

for (const dialect of Object.keys(PROVIDERS) as Provider[]) {
  for (const [label, [check, good, bad]] of Object.entries(SAME_ON_BOTH)) {
    // case: schema_expressions::a_native_check_gives_the_same_answer_on_both_dialects
    test(`a native check gives the same answer on both dialects (${dialect}, ${label})`, async () => {
      const { db, objects } = await opened(dialect, check, "  seen DateTime?\n");
      try {
        await objects.insert({ ...ROW, ...good });
        if (bad) await assert.rejects(objects.insert({ ...ROW, ...bad }), IntegrityError);
        assert.equal(await objects.count(), 1);
      } finally {
        await db.dropTables();
        await db.close();
      }
    });
  }
}

const CLI = [".js", ".ts"].map((ext) => fileURLToPath(new URL(`../src/cli${ext}`, import.meta.url))).find(existsSync)!;

for (const dialect of Object.keys(PROVIDERS) as Provider[]) {
  // case: schema_expressions::offline_native_conversion_preserves_live_rules_and_later_migrations
  test(`offline native conversion preserves live rules and later migrations (${dialect})`, async () => {
    const [provider, url] = PROVIDERS[dialect];
    const dir = mkdtempSync(join(tmpdir(), "monorm-conversion-"));
    const path = join(dir, "schema.prisma");
    const legacy =
      `datasource db {\n provider = "${provider}"\n}\n` +
      'model Item {\n id Int @id\n qty Int @map("Qty")\n @@map("conversion_item_js")\n' +
      ' @@check(raw("((\\"Qty\\" > 0))"))\n @@index([sql("abs( \\"Qty\\" )")], where: raw("(\\"Qty\\" > 0)"))\n}\n';
    writeFileSync(path, legacy);
    const migrations = new Migrations(join(dir, "migrations"), path);
    assert.equal(migrations.make().length, 1);
    const registry = new Registry();
    loads(`datasource db { provider = "${provider}" }\n`, { registry });
    const db = await connect(url, { registry, default: false, maxConnections: 1 });
    if (dialect === "postgres") await db.execute("DROP SCHEMA IF EXISTS pl12_conversion_js CASCADE; CREATE SCHEMA pl12_conversion_js; SET search_path = pl12_conversion_js");
    const runner = new Migrator(db, migrations);
    const insert = (id: number, qty: number) => db.execute(`INSERT INTO conversion_item_js (id, "Qty") VALUES (${id}, ${qty})`);
    try {
      await runner.upgrade();
      await db.execute('INSERT INTO conversion_item_js (id, "Qty") VALUES (1, 2), (2, 3)');
      const converted = spawnSync(process.execPath, [CLI, "--schema", path, "--dir", migrations.directory, "fmt", "--native"], { encoding: "utf8" });
      assert.equal(converted.status, 0, converted.stderr);
      assert.ok(readFileSync(path, "utf8").includes("@@check((((qty > 0))))"));
      assert.deepEqual(migrations.plan().up, []);
      assert.deepEqual(migrations.plan().down, []);
      assert.deepEqual(await runner.upgrade(), []);
      await assert.rejects(insert(3, -1), IntegrityError);
      const pulled = await pull(db);
      assert.deepEqual(pulled.differences, [], pulled.schema);
      assert.deepEqual((await runner.drift()).steps, []);
      writeFileSync(path, readFileSync(path, "utf8").replace(" qty Int", " note String?\n qty Int"));
      assert.equal(migrations.make("unrelated").length, 1);
      await runner.upgrade();
      assert.deepEqual(migrations.plan().up, []);
      writeFileSync(path, readFileSync(path, "utf8").replace("qty > 0", "qty > 1"));
      const [real] = migrations.make("stricter");
      assert.ok(real!.downSql);
      await runner.upgrade();
      await assert.rejects(insert(3, 1), IntegrityError);
      await runner.downgrade({ allowDestructive: true });
      await insert(3, 1);
    } finally {
      if (dialect === "postgres") await db.execute("SET search_path = public; DROP SCHEMA IF EXISTS pl12_conversion_js CASCADE");
      await db.close();
      rmSync(dir, { recursive: true });
    }
  });
}
