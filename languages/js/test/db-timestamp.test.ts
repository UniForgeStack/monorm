/** `@db.Timestamp` writes and filters keep the UTC wall clock under any session zone. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, param, Registry } from "../src/index.js";
import { MICROS, MICROS_MS } from "../src/date-micros.js";

const SCHEMA = `
model StampRow {
  id Int @id
  at DateTime @unique @db.Timestamp(6)
  maybe DateTime? @db.Timestamp(6)
  ats DateTime[] @db.Timestamp(6)
  refs StampRef[]
}
model StampCopy {
  id Int @id
  at DateTime @db.Timestamp(6)
  maybe DateTime? @db.Timestamp(6)
}
type StampAddr {
  city String
  opened DateTime
}
model StampDoc {
  id Int @id
  addr StampAddr? @storage(json)
  src DateTime @db.Timestamp(6)
}
model StampRef {
  id Int @id
  at DateTime @db.Timestamp(6)
  parent StampRow @relation(fields: [at], references: [at])
}
`;
const NAIVE = "2024-01-02 07:08:09.123456";
const ZONES = ["Asia/Tehran", "Pacific/Honolulu"];

function stamp(iso: string, micros = 0): Date {
  const d = new Date(iso);
  Object.defineProperty(d, MICROS, { value: micros });
  Object.defineProperty(d, MICROS_MS, { value: d.getTime() });
  return d;
}
const WHEN = () => stamp("2024-01-02T07:08:09.123Z", 456);
const LATER = () => stamp("2024-01-03T07:08:09.123Z", 456);
function same(actual: Date, expected: Date): void {
  assert.ok(actual instanceof Date);
  assert.equal(actual.toISOString(), expected.toISOString());
  assert.equal(Reflect.get(actual, MICROS) ?? 0, Reflect.get(expected, MICROS) ?? 0);
}
async function world() {
  const registry = new Registry();
  const classes = loads(SCHEMA, { registry }) as any;
  const db = await connect(process.env.ORM_TEST_DATABASE_URL!, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  return { db, classes };
}

test("timestamp writes keep the utc wall clock under a session zone", async (t) => {
  for (const zone of ZONES) await t.test(zone, async () => {
    const { db, classes } = await world();
    try {
      const M = classes.StampRow, Ref = classes.StampRef;
      const q = M.objects.using(db);
      await db.transaction(async () => {
        await db.execute(`SET LOCAL TIME ZONE '${zone}'`);
        const row = await q.insert({ id: 1, at: WHEN(), maybe: WHEN(), ats: [WHEN(), LATER()] });
        same(row.at, WHEN()); same(row.ats[0], WHEN()); same(row.ats[1], LATER());
        const stored = (await db.fetch('SELECT at::text AS at, maybe::text AS maybe, ats::text AS ats FROM "stamprow" WHERE id = 1'))[0]!;
        assert.equal(stored.at, NAIVE); assert.equal(stored.maybe, NAIVE);
        assert.equal(stored.ats, '{"2024-01-02 07:08:09.123456","2024-01-03 07:08:09.123456"}');
        assert.equal(await q.filter(M.at.eq(WHEN())).count(), 1);
        assert.equal(await q.filter(M.at.in([WHEN(), LATER()])).count(), 1);
        assert.equal(await q.filter(M.ats.eq([WHEN(), LATER()])).count(), 1);
        await q.filter(M.id.eq(1)).update({ at: LATER(), maybe: null, ats: [LATER()] });
        assert.equal((await db.fetch('SELECT at::text AS at FROM "stamprow"'))[0]!.at, "2024-01-03 07:08:09.123456");
        same((await q.get(M.id.eq(1))).at, LATER());
        await q.insertMany([{ id: 1, at: LATER(), maybe: WHEN(), ats: [] }]).onConflict([M.id], { update: true });
        same((await q.get(M.id.eq(1))).maybe, WHEN());
        assert.equal((await db.fetch('SELECT maybe::text AS maybe FROM "stamprow"'))[0]!.maybe, NAIVE);
        await Ref.objects.using(db).insert({ id: 1, at: LATER() });
        const related = await Ref.objects.using(db).load(Ref.parent).get(Ref.id.eq(1));
        same(related.parent.at, LATER());
        const rows = await q.load(M.refs).all();
        same(rows[0].refs.cached[0].at, LATER());
      });
    } finally { await db.dropTables(); await db.close(); }
  });
});

test("timestamp bulk writes keep the utc wall clock under a session zone", async (t) => {
  for (const zone of ZONES) await t.test(zone, async () => {
    const { db, classes } = await world();
    try {
      const M = classes.StampRow, C = classes.StampCopy;
      const q = M.objects.using(db);
      const stamps = [stamp("2024-01-01T00:00:00.000Z"), WHEN(), stamp("2024-12-31T23:59:59.999Z", 999)];
      await db.transaction(async () => {
        await db.execute(`SET LOCAL TIME ZONE '${zone}'`);
        await q.insertMany(stamps.map((s, i) => ({ id: i + 1, at: s, maybe: s, ats: [s] })));
        await C.objects.using(db).insertMany(stamps.map((s, i) => ({ id: i + 1, at: s, maybe: s })), { copy: true });
        const rows = await q.filter(M.id.lt(10)).orderBy(M.id).all();
        stamps.forEach((s, i) => { same(rows[i].at, s); same(rows[i].maybe, s); same(rows[i].ats[0], s); });
        const stored = await db.fetch('SELECT at::text AS at FROM "stamprow" WHERE id < 10 ORDER BY id');
        assert.deepEqual(stored.map((r: any) => r.at), ["2024-01-01 00:00:00", NAIVE, "2024-12-31 23:59:59.999999"]);
        const copied = await db.fetch('SELECT at::text AS at, maybe::text AS maybe FROM "stampcopy" ORDER BY id');
        assert.deepEqual(copied.map((r: any) => r.at), ["2024-01-01 00:00:00", NAIVE, "2024-12-31 23:59:59.999999"]);
        assert.deepEqual(copied.map((r: any) => r.maybe), copied.map((r: any) => r.at));
      });
    } finally { await db.dropTables(); await db.close(); }
  });
});

test("timestamp prepared statements keep the utc wall clock under a session zone", async (t) => {
  for (const zone of ZONES) await t.test(zone, async () => {
    const { db, classes } = await world();
    try {
      const M = classes.StampRow;
      const q = M.objects.using(db);
      await db.transaction(async () => {
        await db.execute(`SET LOCAL TIME ZONE '${zone}'`);
        await q.insert({ id: param("i"), at: param("a"), maybe: param("a"), ats: [] }).prepare().run({ i: 1, a: WHEN() });
        await q.insertMany(param("rows", { fields: [M.id, M.at, M.ats] })).prepare().run({ rows: [{ id: 2, at: LATER(), ats: [LATER()] }] });
        const stored = await db.fetch('SELECT at::text AS at, maybe::text AS maybe, ats::text AS ats FROM "stamprow" ORDER BY id');
        assert.deepEqual(stored.map((r: any) => r.at), [NAIVE, "2024-01-03 07:08:09.123456"]);
        assert.equal(stored[0]!.maybe, NAIVE);
        assert.equal(stored[1]!.ats, '{"2024-01-03 07:08:09.123456"}');
        assert.deepEqual((await q.filter(M.at.eq(param("a"))).prepare().all({ a: WHEN() })).map((r: any) => r.id), [1]);
        assert.deepEqual((await q.filter(M.at.in(param("v"))).orderBy(M.id).prepare().all({ v: [WHEN(), LATER()] })).map((r: any) => r.id), [1, 2]);
        const later = stamp("2024-01-04T07:08:09.123Z", 456);
        await q.filter(M.id.eq(1)).update({ at: param("a") }).prepare().run({ a: later });
        assert.equal((await db.fetch('SELECT at::text AS at FROM "stamprow" WHERE id = 1'))[0]!.at, "2024-01-04 07:08:09.123456");
      });
    } finally { await db.dropTables(); await db.close(); }
  });
});

test("timestamp nested expression and prepared keys and rows keep the utc wall clock under a session zone", async (t) => {
  for (const zone of ZONES) await t.test(zone, async () => {
    const { db, classes } = await world();
    try {
      const { StampRow: M, StampRef: Ref, StampDoc: Doc } = classes;
      const q = M.objects.using(db);
      await db.transaction(async () => {
        await db.execute(`SET LOCAL TIME ZONE '${zone}'`);
        await q.insertMany([{ id: 1, at: WHEN(), ats: [] }, { id: 2, at: LATER(), ats: [] }]);
        await Ref.objects.using(db).insertMany([{ id: 1, at: WHEN() }, { id: 2, at: LATER() }]);
        await Doc.objects.using(db).insert({ id: 1, addr: new classes.$types.StampAddr({ city: "Oslo", opened: LATER() }), src: WHEN() });
        await Doc.objects.using(db).filter(Doc.id.eq(1)).update({ addr: { opened: Doc.src } });
        assert.equal((await Doc.objects.using(db).get(Doc.id.eq(1))).addr.opened.toISOString(), WHEN().toISOString());
        const rows = await q.load(M.refs).orderBy(M.id).prepare().all({});
        assert.deepEqual(rows.map((r: any) => r.refs.cached.map((c: any) => c.id)), [[1], [2]]);
        const edit = q.updateMany(param("rows", { fields: [M.id, M.maybe] })).prepare();
        await edit.run({ rows: [{ id: 1, maybe: stamp("2024-01-04T07:08:09.123Z", 456) }, { id: 2, maybe: stamp("2024-01-05T07:08:09.123Z", 456) }] });
        const stored = await db.fetch('SELECT maybe::text AS maybe FROM "stamprow" ORDER BY id');
        assert.deepEqual(stored.map((r: any) => r.maybe), ["2024-01-04 07:08:09.123456", "2024-01-05 07:08:09.123456"]);
      });
    } finally { await db.dropTables(); await db.close(); }
  });
});
