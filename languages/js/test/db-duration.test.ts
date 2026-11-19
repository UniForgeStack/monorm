/** A Duration column: whole microseconds, a `Duration` in TypeScript. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, writeFileSync, rmSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { connect, loads, Registry, Duration, func, param } from "../src/index.js";
import { native } from "../src/native.js";

const SCHEMA = `
model Job {
  id Int @id
  timeout Duration?
}
`;
const us = (n: bigint) => Duration.fromMicros(n);
const DAY = 86_400_000_000n;
const VALUES = [
  us(0n),
  us(1n),
  us(-1n),
  Duration.fromParts({ hours: 1, minutes: 30 }),
  Duration.fromParts({ days: 999, micros: 123456 }),
  us(-3n * DAY - 7_000_000n),
];
const sorted = [...VALUES].sort((a, b) => a.compare(b));

async function world(kind: string) {
  const registry = new Registry();
  const schema = kind === "sqlite" ? 'datasource db { provider = "sqlite" }\n' + SCHEMA : SCHEMA;
  // Dynamic schemas intentionally exercise the runtime classes.
  const classes = loads(schema, { registry }) as any;
  const db = await connect(kind === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL!, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  return { db, Job: classes.Job };
}
const fill = (q: any) => q.insertMany([...VALUES.map((v, i) => ({ id: i + 1, timeout: v })), { id: 7, timeout: null }]);
const same = (actual: unknown, expected: Duration | null) => {
  if (expected === null) return assert.equal(actual, null);
  assert.ok(actual instanceof Duration, `${String(actual)} is no Duration`);
  assert.equal(actual.micros, expected.micros);
};

for (const kind of ["postgres", "sqlite"]) {
  test(`a duration round trips and orders (${kind})`, async () => {
    const { db, Job } = await world(kind);
    try {
      const q = Job.objects.using(db);
      const row = await q.insert({ id: 100, timeout: Duration.fromParts({ days: 1, hours: 2, micros: 3 }) });
      same(row.timeout, Duration.fromParts({ days: 1, hours: 2, micros: 3 }));
      await q.filter(Job.id.eq(100)).delete();
      await fill(q);
      const rows = await q.filter(Job.id.lt(10)).orderBy(Job.id).all();
      [...VALUES, null].forEach((v, i) => same(rows[i].timeout, v));
      const ordered = await q.filter(Job.timeout.isNotNull()).orderBy(Job.timeout).all();
      sorted.forEach((v, i) => same(ordered[i].timeout, v));
      await q.filter(Job.id.eq(1)).update({ timeout: Duration.fromParts({ minutes: 5 }) });
      same((await q.get(Job.id.eq(1))).timeout, Duration.fromParts({ minutes: 5 }));
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`comparison between and in work on durations (${kind})`, async () => {
    const { db, Job } = await world(kind);
    try {
      const q = Job.objects.using(db);
      await fill(q);
      const ids = async (cond: unknown) => (await q.filter(cond).all()).map((r: any) => r.id).sort();
      assert.deepEqual(await ids(Job.timeout.gt(us(0n))), [2, 4, 5]);
      assert.deepEqual(await ids(Job.timeout.lte(us(0n))), [1, 3, 6]);
      assert.deepEqual(await ids(Job.timeout.eq(us(-1n))), [3]);
      assert.deepEqual(await ids(Job.timeout.ne(us(0n)).andNotNull()), [2, 3, 4, 5, 6]);
      assert.deepEqual(await ids(Job.timeout.between(us(-1n), us(1n))), [1, 2, 3]);
      assert.deepEqual(await ids(Job.timeout.in([Duration.fromParts({ hours: 1, minutes: 30 }), us(0n)])), [1, 4]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`sum avg min max return durations (${kind})`, async () => {
    const { db, Job } = await world(kind);
    try {
      const q = Job.objects.using(db);
      await fill(q);
      const small = q.filter(Job.id.in([2, 3, 4]));
      same(await small.select({ v: func.sum(Job.timeout) }).scalar(), Duration.fromParts({ hours: 1, minutes: 30 }));
      same(await small.select({ v: func.min(Job.timeout) }).scalar(), us(-1n));
      same(await small.select({ v: func.max(Job.timeout) }).scalar(), Duration.fromParts({ hours: 1, minutes: 30 }));
      const two = q.filter(Job.id.in([2, 3, 4]).and(Job.timeout.lt(Duration.fromParts({ hours: 1 }))));
      same(await two.select({ v: func.avg(Job.timeout) }).scalar(), us(0n));
      same(await q.filter(Job.id.eq(7)).select({ v: func.sum(Job.timeout) }).scalar(), null);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a cursor pages through durations (${kind})`, async () => {
    const { db, Job } = await world(kind);
    try {
      const q = Job.objects.using(db);
      await fill(q);
      const qs = q.filter(Job.timeout.isNotNull()).orderBy(Job.timeout.asc({ nulls: "last" }), Job.id);
      const seen: Duration[] = [];
      let cursor: string | null = null;
      for (;;) {
        const page: any = await qs.paginate({ first: 2, after: cursor });
        seen.push(...page.items.map((r: any) => r.timeout));
        if (!page.hasNext) break;
        cursor = page.nextCursor;
      }
      sorted.forEach((v, i) => same(seen[i], v));
      assert.equal(seen.length, sorted.length);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a duration in a union branch and a prepared parameter filter (${kind})`, async () => {
    const { db, Job } = await world(kind);
    try {
      const q = Job.objects.using(db);
      await fill(q);
      const low = q.filter(Job.timeout.lt(us(0n))).select({ id: Job.id });
      const high = q.filter(Job.timeout.gt(Duration.fromParts({ hours: 1 }))).select({ id: Job.id });
      assert.deepEqual((await low.unionAll(high).orderBy("id")).map((r: any) => r.id), [3, 4, 5, 6]);
      const lt = q.filter(Job.timeout.lt(param("v"))).orderBy(Job.id).prepare();
      assert.deepEqual((await lt.all({ v: us(0n) })).map((r: any) => r.id), [3, 6]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a prepared in list and a copy load take a duration (${kind})`, async () => {
    const { db, Job } = await world(kind);
    try {
      const q = Job.objects.using(db);
      await fill(q);
      const found = await q.filter(Job.timeout.in(param("v"))).orderBy(Job.id).prepare().all({ v: [us(-1n), us(0n)] });
      assert.deepEqual(found.map((r: any) => r.id), [1, 3]);
      if (kind === "postgres") {
        await q.filter(Job.id.lt(100)).delete();
        await q.insertMany([{ id: 1, timeout: us(0n) }, { id: 2, timeout: us(-1n) }, { id: 3, timeout: null }], { copy: true });
        assert.deepEqual((await q.orderBy(Job.id)).map((r: any) => r.timeout === null ? null : r.timeout.micros), [0n, -1n, null]);
      }
    } finally { await db.dropTables(); await db.close(); }
  });
}

test("a duration beyond 64 bit microseconds is an overflow error", () => {
  assert.throws(() => Duration.fromMicros(1n << 63n), RangeError);
  assert.throws(() => Duration.parse("106751992 00:00:00"), /duration|range/i);
  assert.equal(Duration.fromMicros((1n << 63n) - 1n).micros, (1n << 63n) - 1n);
});

test("a postgres interval reads months as 30 days", async () => {
  const { db, Job } = await world("postgres");
  try {
    await db.execute(`INSERT INTO "job" (id, timeout) VALUES (1, '1 month 2 days'), (2, '1 year'), (3, '-1 microsecond')`);
    const rows = await Job.objects.using(db).orderBy(Job.id).all();
    assert.deepEqual(rows.map((r: any) => r.timeout.micros), [32n * DAY, 360n * DAY, -1n]);
    assert.equal((await db.fetch(`SELECT timeout::text AS t FROM "job" WHERE id = 3`))[0]!.t, "-00:00:00.000001");
  } finally { await db.dropTables(); await db.close(); }
});

test("a duration has the text form of the Rust core", () => {
  const vectors = JSON.parse(readFileSync(new URL("../../../test-vectors/duration-text.json", import.meta.url), "utf8"));
  for (const [micros, text] of vectors.roundtrip as [string, string][]) {
    assert.equal(us(BigInt(micros)).toString(), text);
    assert.equal(Duration.parse(text).micros, BigInt(micros));
  }
  for (const [text, micros] of vectors.parse_only as [string, string][]) assert.equal(Duration.parse(text).micros, BigInt(micros), text);
  for (const bad of vectors.refused as string[]) assert.throws(() => Duration.parse(bad), Error, JSON.stringify(bad));
});

test("a duration field has the duration type", () => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-db-duration-"));
  try {
    const file = join(dir, "schema.prisma"); writeFileSync(file, SCHEMA);
    const generated = native().generateTypescript(file, null, {});
    assert.match(generated, /timeout: Duration/);
    assert.match(generated, /"type":\s*"duration"/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
