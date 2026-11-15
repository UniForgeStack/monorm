/** Native clocks across storage and language boundaries. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { connect, loads, Registry, DatabaseError, DecodeError, QueryError } from "../src/index.js";
import { native } from "../src/native.js";
import { MICROS, MICROS_MS } from "../src/date-micros.js";

const SCHEMA = `
type Clock {
  at DateTime @db.Time
  missing DateTime? @db.Time
}
type Clocks {
  times DateTime[] @db.Time
}
model ListClock {
  id Int @id
  clocks Clocks @storage(json)
}
model TimeRow {
  id Int @id
  at DateTime @unique @db.Time
  zone DateTime? @db.Timetz
  clock Clock? @storage(json)
  columns Clock?
  refs TimeRef[]
}
model TimeCopy {
  id Int @id
  at DateTime @db.Time
}
model TimeRef {
  id Int @id
  at DateTime @db.Time
  parent TimeRow @relation(fields: [at], references: [at])
}
`;
function when(): Date {
  const d = new Date("1970-01-01T07:08:09.123Z");
  Object.defineProperty(d, MICROS, { value: 456 });
  Object.defineProperty(d, MICROS_MS, { value: d.getTime() });
  return d;
}
function clockEqual(actual: Date, expected = when()): void {
  assert.ok(actual instanceof Date);
  assert.equal(actual.toISOString(), expected.toISOString());
  assert.equal(Reflect.get(actual, MICROS) ?? 0, Reflect.get(expected, MICROS) ?? 0);
}
async function world(kind: string) {
  const registry = new Registry();
  let schema = kind === "sqlite" ? 'datasource db { provider = "sqlite" }\n' + SCHEMA : SCHEMA.replace('  zone DateTime?', '  times DateTime[] @db.Time\n  zones DateTime[] @db.Timetz\n  zone DateTime?');
  // Dynamic schemas intentionally exercise the runtime classes.
  const classes = loads(schema, { registry }) as any;
  const db = await connect(kind === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL!, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  return { db, classes };
}

test("native clock round trip", async (t) => {
  for (const kind of ["postgres", "sqlite"]) await t.test(kind, async () => {
    const { db, classes } = await world(kind);
    try {
      const M = classes.TimeRow, Ref = classes.TimeRef, Clock = classes.$types.Clock;
      const q = M.objects.using(db);
      const clock = new Clock({ at: when(), missing: null });
      const extra = kind === "postgres" ? { times: [when(), null], zones: [when(), null] } : {};
      await db.transaction(async () => {
        if (kind === "postgres") await db.execute("SET LOCAL TIME ZONE 'Asia/Tehran'");
        const row = await q.insert({ id: 1, at: when(), zone: when(), clock, columns: clock, ...extra });
        clockEqual(row.at); clockEqual(row.zone);
        clockEqual(row.clock.at); clockEqual(row.columns.at);
        assert.equal(row.clock.missing, null);
        if (kind === "postgres") {
          clockEqual(row.times[0]); clockEqual(row.zones[0]);
          assert.equal(row.times[1], null); assert.equal(row.zones[1], null);
          clockEqual((await q.select({ zones: M.zones }).scalar())[0]);
          assert.equal(await q.filter(M.zones.eq([when(), null])).count(), 1);
          assert.equal((await db.fetch('SELECT zone::text AS zone FROM "timerow"'))[0]!.zone, "07:08:09.123456+00");
        }
        const listed = await classes.ListClock.objects.using(db).insert({ id: 1, clocks: new classes.$types.Clocks({ times: [when(), new Date(0)] }) });
        clockEqual(listed.clocks.times[0]); clockEqual(listed.clocks.times[1], new Date(0));
        await q.filter(M.id.eq(1)).update({ clock: { at: M.at } });
        clockEqual((await q.get(M.id.eq(1))).clock.at);
        clockEqual(await q.select({ at: M.clock.at }).scalar());
        assert.equal(await q.filter(M.zone.eq(when())).count(), 1);
        assert.equal(await q.filter(M.at.eq(when())).count(), 1);
        assert.equal(await q.filter(M.clock.at.eq(when())).count(), 1);
        await Ref.objects.using(db).insert({ id: 1, at: when() });
        const related = await Ref.objects.using(db).load(Ref.parent).get(Ref.id.eq(1));
        clockEqual(related.parent.at);
        const rows = await q.load(M.refs).all();
        clockEqual(rows[0].refs.cached[0].at);
        if (kind === "postgres") {
          await db.execute("UPDATE timerow SET zone = '00:30:00+02'::timetz, zones = ARRAY['00:30:00+02'::timetz,NULL] WHERE id = 1");
          assert.equal(await q.filter(M.zone.eq(new Date("1970-01-01T22:30:00Z"))).count(), 1);
          assert.equal(await q.filter(M.zones.eq([new Date("1970-01-01T22:30:00Z"), null])).count(), 1);
        }
        await row.update({ zone: null });
        assert.equal(row.zone, null);
      });
    } finally { await db.dropTables(); await db.close(); }
  });
});

test("native clock bulk writes", async (t) => {
  for (const kind of ["postgres", "sqlite"]) await t.test(kind, async () => {
    const { db, classes } = await world(kind);
    try {
      const M = classes.TimeRow, C = classes.TimeCopy;
      const q = M.objects.using(db);
      const times = [new Date(0), when(), (() => { const d = new Date("1970-01-01T23:59:59.999Z"); Object.defineProperty(d, MICROS, { value: 999 }); Object.defineProperty(d, MICROS_MS, { value: d.getTime() }); return d; })()];
      const cells = (i: number, d: Date) => ({ id: i, at: d, zone: d, ...(kind === "postgres" ? { times: [d, null], zones: [d, null] } : {}) });
      await db.transaction(async () => {
        if (kind === "postgres") await db.execute("SET LOCAL TIME ZONE 'Asia/Tehran'");
        await q.insertMany(times.map((d, i) => cells(i + 1, d)));
        if (kind === "postgres") {
          await C.objects.using(db).insertMany(times.map((d, i) => ({ id: i + 11, at: d })), { copy: true });
          await assert.rejects(q.insertMany([cells(21, when())], { copy: true }), (e: unknown) => e instanceof QueryError && /writes through SQL/.test(e.message));
        }
        const rows = await q.filter(M.id.lt(10)).orderBy(M.id).all();
        times.forEach((d, i) => { clockEqual(rows[i].at, d); clockEqual(rows[i].zone, d); });
        if (kind === "postgres") {
          times.forEach((d, i) => { clockEqual(rows[i].zones[0], d); assert.equal(rows[i].zones[1], null); clockEqual(rows[i].times[0], d); assert.equal(rows[i].times[1], null); });
          const copied = await C.objects.using(db).orderBy(C.id).all();
          times.forEach((d, i) => clockEqual(copied[i].at, d));
          assert.equal((await db.fetch('SELECT zone::text AS zone FROM "timerow" WHERE id = 3'))[0]!.zone, "23:59:59.999999+00");
        }
      });
    } finally { await db.dropTables(); await db.close(); }
  });
});

test("a nested clock of 24 hours is a range error", async () => {
  const { db, classes } = await world("postgres");
  try {
    const M = classes.TimeRow;
    const q = M.objects.using(db);
    await db.transaction(async () => {
      await db.execute("INSERT INTO timerow (id, at, times, zones) VALUES (1, '24:00:00', ARRAY[]::time[], ARRAY[]::timetz[])");
      await q.filter(M.id.eq(1)).update({ clock: { at: M.at } });
      assert.match((await db.fetch('SELECT clock::text AS clock FROM "timerow"'))[0]!.clock as string, /24:00:00/);
      await assert.rejects(q.select({ clock: M.clock }).scalar(), (e: unknown) => e instanceof DecodeError);
    });
  } finally { await db.dropTables(); await db.close(); }
});

test("raw clock edges", async (t) => {
  for (const kind of ["postgres", "sqlite"]) await t.test(kind, async () => {
    const { db } = await world(kind);
    try {
      if (kind === "sqlite") {
        assert.equal((await db.fetch("SELECT '07:08:09.123456' AS clock"))[0]!.clock, "07:08:09.123456");
        return;
      }
      const row = (await db.fetch("SELECT '07:08:09.123456'::time AS clock, '00:30:00+02'::timetz AS east, '23:30:00-02'::timetz AS west, ARRAY['07:08:09.123456'::time,NULL] AS times, ARRAY['00:30:00+02'::timetz,NULL] AS zones, NULL::time AS missing"))[0] as any;
      clockEqual(row.clock);
      clockEqual(row.east, new Date("1970-01-01T22:30:00Z"));
      clockEqual(row.west, new Date("1970-01-01T01:30:00Z"));
      clockEqual(row.times[0]); assert.equal(row.times[1], null);
      clockEqual(row.zones[0], new Date("1970-01-01T22:30:00Z")); assert.equal(row.zones[1], null);
      assert.equal(row.missing, null);
      await assert.rejects(db.fetch("SELECT '24:00:00'::time"), (e: unknown) => e instanceof DatabaseError && /24:00:00.*not representable/.test(e.message));
      await assert.rejects(db.fetch("SELECT ARRAY['24:00:00'::timetz]"), /24:00:00.*not representable/);
    } finally { await db.dropTables(); await db.close(); }
  });
});

test("native clock codegen", () => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-db-time-"));
  try {
    const file = join(dir, "schema.prisma"); writeFileSync(file, SCHEMA);
    const generated = native().generateTypescript(file, null, {});
    assert.match(generated, /at: Date/);
    assert.match(generated, /"type":\s*"time"/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
