/** `qs.explain()` and `db.fetch(sql, ...params)` on Postgres and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, DatabaseError, Decimal, getDatabase, loads, QueryError, Registry, type QueryEvent } from "../src/index.js";
import { Post, User } from "./blog/models.js";
import { useDatabase } from "./helpers.js";

useDatabase();

const source = `datasource db {
  provider = "sqlite"
}
model Person {
  id   Int    @id
  name String @unique
}
`;

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const people = async (body: (Person: Any, db: Any) => Promise<void>) => {
  const registry = new Registry();
  const { Person } = loads(source, { registry }) as Record<string, Any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await Person.objects.using(db).insertMany([0, 1, 2].map((i) => ({ id: i, name: `p${i}` })));
    await body(Person, db);
  } finally { await db.close(); }
};

// case: explain_fetch::explain_on_sqlite
test("explain on SQLite", () => people(async (Person, db) => {
  const plan = await Person.objects.using(db).filter(Person.name.eq("p1")).explain();
  assert.match(plan, /SEARCH/);
  assert.match(plan, /person/);
  const scan = await Person.objects.using(db).filter(Person.name.contains("p")).orderBy(Person.name).explain();
  assert.match(scan, /SCAN/);
  await assert.rejects(Person.objects.using(db).explain({ analyze: true }), (e: unknown) => e instanceof QueryError && /ANALYZE/.test(e.message));
}));

// case: explain_fetch::fetch_on_sqlite
test("fetch on SQLite", () => people(async (_Person, db) => {
  const rows = await db.fetch("SELECT id, name, id * 0.5 AS half FROM person WHERE id >= ? ORDER BY id", 1);
  assert.deepEqual(rows, [{ id: 1n, name: "p1", half: 0.5 }, { id: 2n, name: "p2", half: 1 }]);
  const events: QueryEvent[] = [];
  db.onQuery((e: QueryEvent) => events.push(e));
  assert.deepEqual(await db.fetch("SELECT count(*) AS n FROM person"), [{ n: 3n }]);
  assert.deepEqual(events.map((e) => [e.sql, e.rows]), [["SELECT count(*) AS n FROM person", 1]]);
}));

// case: explain_fetch::explain_on_postgres
test("explain on Postgres", async () => {
  const user = await User.objects.insert({ email: "e@example.com", name: "E" });
  const qs = Post.objects.filter(Post.authorId.eq(user.id)).orderBy(Post.id);
  const plan = await qs.explain();
  assert.match(plan, /Scan/);
  assert.doesNotMatch(plan, /actual time/);
  assert.match(await qs.explain({ analyze: true }), /actual time[\s\S]*Execution Time/);
  const events: QueryEvent[] = [];
  const remove = getDatabase().onQuery((e) => events.push(e));
  try {
    await User.objects.filter(User.id.eq(user.id)).explain();
  } finally { remove(); }
  assert.deepEqual(events.map((e) => e.sql.startsWith("EXPLAIN SELECT") && e.sql.includes("$1")), [true]);
  await assert.rejects(Post.objects.lock().explain({ analyze: true }), /row locks/);
});

// case: explain_fetch::fetch_on_postgres
test("fetch on Postgres", async () => {
  const db = getDatabase();
  const user = await User.objects.insert({ email: "f@example.com", name: "F" });
  assert.deepEqual(await db.fetch("SELECT id, email FROM users WHERE id = $1 AND name = $2", user.id, "F"), [{ id: user.id, email: "f@example.com" }]);
  const u = "0190a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2b";
  const when = new Date("2026-01-02T03:04:05Z");
  const [row] = await db.fetch(
    "SELECT $1::uuid AS u, $2::timestamptz AS t, $3::numeric AS d, $4::jsonb AS j, 2::int2 AS small, "
      + "1.5::float4 AS f, ARRAY[1, 2] AS a, NULL::text AS n, true AS b, DATE '2026-01-02' AS day",
    u, when, "1.50", { k: [1] },
  );
  assert.deepEqual(row, { u, t: when, d: row!.d, j: { k: [1] }, small: 2, f: 1.5, a: [1, 2], n: null, b: true, day: new Date("2026-01-02T00:00:00Z") });
  assert.ok(row!.d instanceof Decimal && row!.d.toFixed(2) === "1.50");
  await db.transaction(async () => {
    await db.execute("UPDATE users SET name = 'G'");
    assert.deepEqual(await db.fetch("SELECT name FROM users"), [{ name: "G" }]);
  });
  await assert.rejects(db.fetch("SELECT interval '1 day' AS i"), (e: unknown) => e instanceof DatabaseError && /interval/.test(e.message));
  assert.deepEqual(await db.fetch("SELECT id FROM users WHERE false"), []);
});
