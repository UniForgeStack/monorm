/** `db.onQuery` hooks, `monorm/otel` spans, and the N+1 finder on the same events. */
import assert from "node:assert/strict";
import { AsyncLocalStorage } from "node:async_hooks";
import { test } from "node:test";
import { connect, debug, IntegrityError, loads, param, Registry, type Database, type QueryEvent } from "../src/index.js";
import { instrument, type Span, type Tracer } from "../src/otel.js";

const source = `datasource db {
  provider = "sqlite"
}
model Person {
  id        Int        @id
  name      String     @unique
  customers Customer[]
}
model Customer {
  id        Int    @id
  person_id Int
  person    Person @relation(fields: [person_id], references: [id])
}
`;

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any
async function shop(): Promise<{ Person: Any; Customer: Any; db: Database }> {
  const registry = new Registry();
  const models = loads(source, { registry }) as Record<string, Any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  const { Person, Customer } = models;
  await Person.objects.using(db).insertMany([0, 1, 2].map((i) => ({ id: i, name: `p${i}` })));
  await Customer.objects.using(db).insertMany([0, 1, 2, 3].map((i) => ({ id: i, personId: i % 2 })));
  return { Person, Customer, db };
}

test("a hook gets each statement with its shape, time and rows", async () => {
  const { Person, Customer, db } = await shop();
  try {
    const events: QueryEvent[] = [];
    const off = db.onQuery((e) => events.push(e));
    const people = await Person.objects.using(db).load(Person.customers).orderBy(Person.id).all();
    assert.equal(people.length, 3);
    const [select, prefetch] = events;
    assert.match(select!.sql, /^SELECT .*"person"/);
    assert.equal(select!.rows, 3);
    assert.match(prefetch!.sql, /"customer".*\?/);
    assert.equal(prefetch!.rows, 4);
    assert.equal(select!.error, null);
    assert.ok(select!.duration >= 0 && select!.duration < 5000 && select!.start <= prefetch!.start);
    assert.ok(Math.abs(select!.start - Date.now()) < 60_000);

    events.length = 0;
    assert.equal(await Customer.objects.using(db).updateMany([0, 1, 2].map((i) => ({ id: i, personId: 2 })), { batchSize: 1 }), 3);
    assert.deepEqual(events.map((e) => e.rows), [1, 1, 1]);

    events.length = 0;
    assert.equal(await db.execute("DELETE FROM customer WHERE id = 3"), 1);
    assert.deepEqual(events.map((e) => [e.sql, e.rows]), [["DELETE FROM customer WHERE id = 3", 1]]);

    off();
    events.length = 0;
    await Person.objects.using(db).count();
    assert.deepEqual(events, []);
  } finally { await db.close(); }
});

test("a failed statement gives an event with the error", async () => {
  const { Person, db } = await shop();
  try {
    const events: QueryEvent[] = [];
    db.onQuery((e) => events.push(e));
    await assert.rejects(Person.objects.using(db).insert({ id: 9, name: "p0" }), IntegrityError);
    assert.equal(events.length, 1);
    assert.match(events[0]!.sql, /^INSERT INTO/);
    assert.equal(events[0]!.rows, 0);
    assert.match(events[0]!.error!, /UNIQUE/);
  } finally { await db.close(); }
});

test("hooks run in the caller's context and see prepared queries", async () => {
  const { Person, db } = await shop();
  try {
    const request = new AsyncLocalStorage<string>();
    const seen: [string | undefined, number][] = [];
    db.onQuery((e) => seen.push([request.getStore(), e.rows]));
    const byName = Person.objects.using(db).filter(Person.name.eq(param("name"))).prepare();
    await request.run("r1", async () => assert.equal((await byName.get({ name: "p1" })).id, 1));
    await request.run("r2", () => db.transaction(() => Person.objects.using(db).filter(Person.id.eq(1)).update({ name: "x" })));
    assert.deepEqual(seen, [["r1", 1], ["r2", 1]]);
  } finally { await db.close(); }
});

test("an error a hook throws rejects the call", async () => {
  const { Person, db } = await shop();
  try {
    db.onQuery(() => { throw new Error("exporter down"); });
    await assert.rejects(Person.objects.using(db).count(), /exporter down/);
  } finally { await db.close(); }
});

test("an async hook is awaited, and its rejection rejects the call", async () => {
  const { Person, db } = await shop();
  try {
    const seen: string[] = [];
    const off = db.onQuery(async (e) => { await new Promise((resolve) => setImmediate(resolve)); seen.push(e.sql); });
    await Person.objects.using(db).count();
    assert.equal(seen.length, 1);
    off();
    db.onQuery(async () => { await Promise.resolve(); throw new Error("async exporter down"); });
    await assert.rejects(Person.objects.using(db).count(), /async exporter down/);
  } finally { await db.close(); }
});

test("a hook error does not replace the error of the call", async () => {
  const { Person, db } = await shop();
  try {
    const later: QueryEvent[] = [];
    db.onQuery(() => { throw new Error("exporter down"); });
    db.onQuery((e) => { later.push(e); });
    await assert.rejects(Person.objects.using(db).insert({ id: 9, name: "p0" }), (error: Error) => {
      assert.ok(error instanceof IntegrityError);
      assert.match((error.cause as Error).message, /exporter down/);
      return true;
    });
    assert.equal(later.length, 1);
    assert.match(later[0]!.error!, /UNIQUE/);
  } finally { await db.close(); }
});

test("every hook gets every event before the first hook error rejects the call", async () => {
  const { Person, db } = await shop();
  try {
    const calls: string[] = [];
    for (const name of ["first", "second"]) db.onQuery(() => { calls.push(name); throw new Error(name); });
    await assert.rejects(Person.objects.using(db).load(Person.customers).all(), (error: Error) => { // two statements
      assert.equal(error.message, "first");
      assert.ok(error.cause instanceof AggregateError);
      assert.equal(error.cause.errors.length, 3);
      return true;
    });
    assert.deepEqual(calls, ["first", "first", "second", "second"]);
  } finally { await db.close(); }
});

test("the statements of a hook give no events and no N+1 count", async () => {
  const { Person, db } = await shop();
  try {
    await db.execute("CREATE TABLE audit (sql TEXT)");
    const calls: string[] = [];
    const off = db.onQuery(async (e) => {
      calls.push(e.sql);
      if (calls.length > 5) return; // a hook that sees its own statements would never end
      for (let i = 0; i < 3; i++) await db.fetch("SELECT count(*) AS n FROM audit");
      await db.execute("INSERT INTO audit VALUES ('x')");
    });
    await debug.nPlusOne(async () => { await Person.objects.using(db).count(); }, { threshold: 2, fail: true });
    off();
    assert.equal(calls.length, 1);
    assert.deepEqual(await db.fetch("SELECT count(*) AS n FROM audit"), [{ n: 1n }]);
  } finally { await db.close(); }
});

test("remove takes out its own registration once", async () => {
  const { Person, db } = await shop();
  try {
    const seen: QueryEvent[] = [];
    const hook = (e: QueryEvent): void => { seen.push(e); };
    const removeFirst = db.onQuery(hook);
    db.onQuery(hook);
    removeFirst();
    removeFirst();
    await Person.objects.using(db).count();
    assert.equal(seen.length, 1);
  } finally { await db.close(); }
});

test("a hook that changes the hooks does not change the call in progress", async () => {
  const { Person, db } = await shop();
  try {
    const seen: string[] = [];
    const removeOnce = db.onQuery(() => {
      seen.push("once");
      removeOnce();
      db.onQuery(() => { seen.push("late"); });
    });
    db.onQuery(() => { seen.push("always"); });
    await Person.objects.using(db).count();
    assert.deepEqual(seen, ["once", "always"]);
    await Person.objects.using(db).count();
    assert.deepEqual(seen, ["once", "always", "always", "late"]);
  } finally { await db.close(); }
});

class FakeSpan implements Span {
  status: { code: number; message?: string } | null = null;
  endTime: number | undefined;
  constructor(readonly name: string, readonly options: Parameters<Tracer["startSpan"]>[1]) {}
  setStatus(status: { code: number; message?: string }): void { this.status = status; }
  end(endTime?: number): void { this.endTime = endTime; }
}

class FakeTracer implements Tracer {
  readonly spans: FakeSpan[] = [];
  startSpan(name: string, options?: Parameters<Tracer["startSpan"]>[1]): FakeSpan {
    this.spans.push(new FakeSpan(name, options));
    return this.spans.at(-1)!;
  }
}

test("otel gives a client span per statement", async () => {
  const { Person, db } = await shop();
  try {
    const tracer = new FakeTracer();
    const stop = await instrument(db, { tracer });
    await Person.objects.using(db).filter(Person.id.eq(1)).all();
    await assert.rejects(Person.objects.using(db).insert({ id: 9, name: "p0" }), IntegrityError);
    const [ok, failed] = tracer.spans;
    assert.equal(ok!.name, "SELECT");
    assert.equal(ok!.options!.kind, 2); // SpanKind.CLIENT
    assert.equal(ok!.status, null);
    assert.equal(ok!.options!.attributes!["db.system.name"], "sqlite");
    assert.equal(ok!.options!.attributes!["db.response.returned_rows"], 1);
    assert.match(String(ok!.options!.attributes!["db.query.text"]), /^SELECT .*\?/);
    assert.ok(ok!.endTime! >= ok!.options!.startTime!);
    assert.equal(failed!.name, "INSERT");
    assert.equal(failed!.status!.code, 2); // SpanStatusCode.ERROR
    assert.match(failed!.status!.message!, /UNIQUE/);
    stop();
    await Person.objects.using(db).count();
    assert.equal(tracer.spans.length, 2);
  } finally { await db.close(); }
});

test("otel names a statement that has no word", async () => {
  const { db } = await shop();
  try {
    const tracer = new FakeTracer();
    await instrument(db, { tracer });
    await assert.rejects(db.execute("("));
    assert.equal(tracer.spans[0]!.name, "SQL");
    assert.equal(tracer.spans[0]!.status!.code, 2);
  } finally { await db.close(); }
});

test("the N+1 finder counts the traced SQL", async () => {
  const { Person, db } = await shop();
  try {
    const report = await debug.nPlusOne(async () => {
      for (const i of [0, 1, 2]) await Person.objects.using(db).insert({ id: 10 + i, name: `n${i}` });
    }, { threshold: 2 }).then(() => null, (e: unknown) => e);
    assert.equal(report, null); // fail is off: a warning, no error
    const error = await debug.nPlusOne(async () => {
      for (const i of [0, 1, 2]) await Person.objects.using(db).insert({ id: 20 + i, name: `m${i}` });
    }, { threshold: 2, fail: true }).then(() => null, (e: unknown) => e);
    assert.ok(error instanceof debug.NPlusOne);
    const [shape] = error.report.repeated;
    assert.equal(shape!.count, 3);
    assert.match(shape!.sql, /^INSERT INTO "person".*\?/);
  } finally { await db.close(); }
});

const postgresUrl = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const blog = `datasource db {
  provider = "postgresql"
}
model Account {
  id    Int    @id @default(autoincrement())
  email String @unique
}
`;

test("postgres: the error of an event has the first line only", async () => {
  const registry = new Registry();
  const { Account } = loads(blog, { registry }) as Record<string, Any>;
  const db = await connect(postgresUrl, { registry, default: false });
  try {
    await db.execute("DROP TABLE IF EXISTS account");
    await db.createTables();
    await Account.objects.using(db).insert({ email: "dup@example.com" });
    const events: QueryEvent[] = [];
    db.onQuery((e) => { events.push(e); });
    await assert.rejects(Account.objects.using(db).insert({ email: "dup@example.com" }), (error: Error) => {
      assert.ok(error instanceof IntegrityError);
      assert.match(error.message, /dup@example\.com/);
      return true;
    });
    assert.equal(events.length, 1);
    assert.ok(!events[0]!.error!.includes("\n") && !events[0]!.error!.includes("dup@example.com"));
  } finally { await db.execute("DROP TABLE IF EXISTS account"); await db.close(); }
});

test("postgres: a failed nowait lock reports 0 rows and a won lock 1", async () => {
  const db = await connect(postgresUrl, { default: false });
  const other = await connect(postgresUrl, { default: false });
  try {
    const events: QueryEvent[] = [];
    db.onQuery((e) => { events.push(e); });
    await other.transaction(async () => {
      await other.lock(4243);
      await db.transaction(async () => {
        assert.equal(await db.lock(4243, { nowait: true }), false);
      });
    });
    await db.transaction(async () => {
      assert.equal(await db.lock(4243, { nowait: true }), true);
    });
    assert.deepEqual(events.filter((e) => e.sql.includes("advisory")).map((e) => e.rows), [0, 1]);
  } finally { await other.close(); await db.close(); }
});

test("otel cuts a long statement at 4096 characters", async () => {
  const { db } = await shop();
  try {
    const tracer = new FakeTracer();
    const stop = await instrument(db, { tracer, rawSql: true });
    await db.execute("SELECT 1 /* " + "x".repeat(5000) + " */");
    await db.execute("SELECT 2");
    stop();
    const [long, short] = tracer.spans.map((s) => String(s.options!.attributes!["db.query.text"]));
    assert.equal(long!.length, 4099);
    assert.ok(long!.endsWith("..."));
    assert.equal(short, "SELECT 2");
  } finally { await db.close(); }
});

test("raw SQL without parameters has the raw flag and no span text by default", async () => {
  const { Person, db } = await shop();
  try {
    const events: QueryEvent[] = [];
    db.onQuery((e) => { events.push(e); });
    const tracer = new FakeTracer();
    const stop = await instrument(db, { tracer });
    await db.execute("UPDATE person SET name = 'secret-literal' WHERE id = 1");
    await db.fetch("SELECT name FROM person WHERE id = ?", 1);
    await Person.objects.using(db).filter(Person.id.eq(1)).all();
    stop();
    assert.deepEqual(events.map((e) => e.raw), [true, false, false]);
    assert.ok(events[0]!.sql.includes("secret-literal"));
    const [execute, fetch, built] = tracer.spans;
    assert.equal("db.query.text" in execute!.options!.attributes!, false);
    assert.equal(execute!.name, "UPDATE");
    assert.match(String(fetch!.options!.attributes!["db.query.text"]), /^SELECT name/);
    assert.ok(built!.options!.attributes!["db.query.text"]);
    const opted = new FakeTracer();
    const stopRaw = await instrument(db, { tracer: opted, rawSql: true });
    await db.execute("UPDATE person SET name = 'secret-literal' WHERE id = 1");
    stopRaw();
    assert.match(String(opted.spans[0]!.options!.attributes!["db.query.text"]), /secret-literal/);
  } finally { await db.close(); }
});
