/** `db.fetch` and error details: stale plans, int2/float4 arrays, `__proto__` columns, error stacks, parameter types. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, DatabaseError, IntegrityError, loads, Registry } from "../src/index.js";
import { fromNative } from "../src/errors.js";
import { DATABASE_URL } from "./helpers.js";

const sqliteSource = `datasource db {
  provider = "sqlite"
}
model Person {
  id Int @id
}
`;

async function lite() {
  const registry = new Registry();
  loads(sqliteSource, { registry });
  return connect("sqlite://:memory:", { registry, default: false });
}

async function single() {
  return connect(DATABASE_URL, { registry: new Registry(), default: false, maxConnections: 1 });
}

test("a fetch after a schema change prepares again", async () => {
  const db = await single();
  try {
    await db.execute("DROP TABLE IF EXISTS fx_stale");
    await db.execute("CREATE TABLE fx_stale (id int)");
    await db.execute("INSERT INTO fx_stale VALUES (1)");
    const sql = "SELECT * FROM fx_stale WHERE id = $1";
    assert.deepEqual(await db.fetch(sql, 1), [{ id: 1 }]);
    await db.execute("ALTER TABLE fx_stale ADD COLUMN extra int DEFAULT 5");
    await assert.rejects(db.fetch(sql, 1), (e: unknown) => e instanceof DatabaseError && e.sqlstate === "0A000");
    assert.deepEqual(await db.fetch(sql, 1), [{ id: 1, extra: 5 }]);
  } finally {
    await db.execute("DROP TABLE IF EXISTS fx_stale");
    await db.close();
  }
});

test("postgres: fetch reads int2 and float4 arrays", async () => {
  const db = await single();
  try {
    assert.deepEqual(await db.fetch("SELECT ARRAY[1::int2, NULL] AS a, ARRAY[1.5::float4] AS b"), [{ a: [1, null], b: [1.5] }]);
  } finally {
    await db.close();
  }
});

test("a column named __proto__ is an own key of the row", async () => {
  const db = await lite();
  try {
    const [row] = await db.fetch(`SELECT 1 AS "__proto__", 2 AS b`);
    assert.deepEqual(Object.keys(row!), ["__proto__", "b"]);
    assert.equal(Object.getPrototypeOf(row), Object.prototype);
    assert.equal(Object.getOwnPropertyDescriptor(row, "__proto__")?.value, 1n);
  } finally {
    await db.close();
  }
});

test("fromNative keeps the stack frames and not the lines of a multi-line message", () => {
  const e = new Error("[orm:DatabaseError] first line\nDETAIL: second line");
  const out = fromNative(e) as Error;
  assert.ok(out instanceof DatabaseError);
  const lines = out.stack!.split("\n");
  assert.equal(lines[0], "DatabaseError: first line");
  assert.equal(lines[1], "DETAIL: second line");
  assert.ok(lines[2]!.trim().startsWith("at "), lines[2]);
  assert.equal(lines.filter((l) => l.includes("second line")).length, 1);
});

test("sqlite: NOT NULL and CHECK give a sqlstate, a unique error has no constraint", async () => {
  const db = await lite();
  try {
    await db.execute("CREATE TABLE fx (id integer PRIMARY KEY, n text NOT NULL, c integer CHECK (c > 0), u text UNIQUE)");
    const failure = async (sql: string) => {
      try {
        await db.execute(sql);
      } catch (e) {
        assert.ok(e instanceof IntegrityError);
        return [e.sqlstate, e.constraint];
      }
      throw new Error("no error");
    };
    assert.deepEqual(await failure("INSERT INTO fx (id, n) VALUES (1, NULL)"), ["23502", null]);
    assert.deepEqual((await failure("INSERT INTO fx (id, n, c) VALUES (1, 'a', 0)"))[0], "23514");
    await db.execute("INSERT INTO fx (id, n, u) VALUES (1, 'a', 'x')");
    assert.deepEqual(await failure("INSERT INTO fx (id, n, u) VALUES (2, 'a', 'x')"), ["23505", null]);
  } finally {
    await db.close();
  }
});

test("postgres: NOT NULL and CHECK give a sqlstate", async () => {
  const db = await single();
  try {
    await db.execute("DROP TABLE IF EXISTS fx_stale");
    await db.execute("CREATE TABLE fx_stale (id int NOT NULL, c int CONSTRAINT fx_c CHECK (c > 0))");
    await assert.rejects(db.execute("INSERT INTO fx_stale (id) VALUES (NULL)"), (e: unknown) => e instanceof IntegrityError && e.sqlstate === "23502");
    await assert.rejects(db.execute("INSERT INTO fx_stale (id, c) VALUES (1, 0)"), (e: unknown) => e instanceof IntegrityError && e.sqlstate === "23514" && e.constraint === "fx_c");
  } finally {
    await db.execute("DROP TABLE IF EXISTS fx_stale");
    await db.close();
  }
});

test("fetch parameters of the types bool, float, date and list", async () => {
  const day = new Date("2026-01-02T00:00:00Z");
  const pg = await single();
  try {
    const [row] = await pg.fetch("SELECT $1::bool AS b, $2::float8 AS f, $3::timestamptz AS d, $4::jsonb AS l", true, 1.5, day, [1, "x"]);
    assert.deepEqual(row, { b: true, f: 1.5, d: day, l: [1, "x"] });
  } finally {
    await pg.close();
  }
  const db = await lite();
  try {
    const [row] = await db.fetch("SELECT ? AS b, ? AS f, ? AS l", true, 1.5, [1, "x"]);
    assert.deepEqual(row, { b: 1n, f: 1.5, l: '[1,"x"]' });
  } finally {
    await db.close();
  }
});
