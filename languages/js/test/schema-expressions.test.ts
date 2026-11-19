/** Native checks: a bad row is rejected by the database, and each dialect gives the same answer. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, IntegrityError, loads, Registry } from "../src/index.js";
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
