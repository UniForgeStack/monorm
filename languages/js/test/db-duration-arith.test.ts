/** Arithmetic on a DateTime and a Duration: the same microseconds on both databases. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry, Duration, param } from "../src/index.js";
import { extraMicros, keepMicros } from "../src/date-micros.js";

const SCHEMA = `
model Event {
  id Int @id
  at DateTime
  until DateTime
  span Duration
  late DateTime?
  rest Duration?
}
`;
const us = (n: bigint) => Duration.fromMicros(n);
const DAY = 86_400_000_000n;
/** A `Date` of whole milliseconds since the epoch plus `micros` below them. */
const stamp = (iso: string, micros = 0) => {
  const d = new Date(iso);
  keepMicros(d, micros);
  return d;
};
/** Microseconds since the epoch of a `Date`, with the hidden microseconds. */
const epoch = (d: Date) => BigInt(d.getTime()) * 1000n + BigInt(extraMicros(d));
const AT = [stamp("2024-01-01T00:00:00Z"), stamp("2024-03-01T12:00:00.500Z"), stamp("2024-01-01T00:00:00Z")];
const UNTIL = [stamp("2024-01-01T00:00:00Z", 1), stamp("2024-03-01T12:00:00Z"), stamp("2024-01-01T00:00:00Z")];
const SPAN = [us(DAY), us(10n), us(11n)];

async function world(kind: string) {
  const registry = new Registry();
  const schema = kind === "sqlite" ? 'datasource db { provider = "sqlite" }\n' + SCHEMA : SCHEMA;
  // Dynamic schemas intentionally exercise the runtime classes.
  const classes = loads(schema, { registry }) as any;
  const db = await connect(kind === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL!, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  const Event = classes.Event;
  const q = Event.objects.using(db);
  await q.insertMany([0, 1, 2].map((i) => ({ id: i + 1, at: AT[i], until: UNTIL[i], span: SPAN[i] })));
  return { db, Event, q };
}
const column = async (q: any, Event: any, expr: any, args?: object) => {
  const shaped = q.orderBy(Event.id).select({ x: expr });
  const rows = args ? await shaped.prepare().all(args) : await shaped.all();
  return rows.map((r: any) => r.x);
};
const dates = (rows: Date[]) => rows.map(epoch);
const durations = (rows: Duration[]) => rows.map((d) => d.micros);

for (const kind of ["postgres", "sqlite"]) {
  test(`a datetime and a duration add and subtract to the microsecond (${kind})`, async () => {
    const { db, Event, q } = await world(kind);
    try {
      const plus = [epoch(AT[0]!) + DAY, epoch(AT[1]!) + 10n, epoch(AT[2]!) + 11n];
      assert.deepEqual(dates(await column(q, Event, Event.at.add(Event.span))), plus);
      assert.deepEqual(dates(await column(q, Event, Event.span.add(Event.at))), plus);
      assert.deepEqual(dates(await column(q, Event, Event.at.sub(Event.span))), [epoch(AT[0]!) - DAY, epoch(AT[1]!) - 10n, epoch(AT[2]!) - 11n]);
      assert.deepEqual(dates(await column(q, Event, Event.at.add(us(1n)))), AT.map((a) => epoch(a) + 1n));
      assert.deepEqual(dates(await column(q, Event, Event.at.add(Duration.fromParts({ micros: 500_000 })))), AT.map((a) => epoch(a) + 500_000n));
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`the difference of two datetimes is a signed duration (${kind})`, async () => {
    const { db, Event, q } = await world(kind);
    try {
      assert.deepEqual(durations(await column(q, Event, Event.until.sub(Event.at))), [1n, -500_000n, 0n]);
      assert.deepEqual(durations(await column(q, Event, Event.at.sub(Event.until))), [-1n, 500_000n, 0n]);
      const far = stamp("2024-01-01T00:00:00Z");
      assert.deepEqual(durations(await column(q, Event, Event.at.sub(far))), [0n, epoch(AT[1]!) - epoch(far), 0n]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`durations add subtract scale and round to the nearest microsecond (${kind})`, async () => {
    const { db, Event, q } = await world(kind);
    try {
      assert.deepEqual(durations(await column(q, Event, Event.span.add(Event.span))), [2n * DAY, 20n, 22n]);
      assert.deepEqual(durations(await column(q, Event, Event.span.sub(Event.span.mul(3)))), [-2n * DAY, -20n, -22n]);
      assert.deepEqual(durations(await column(q, Event, Event.span.mul(3))), [3n * DAY, 30n, 33n]);
      // 10 / 3 = 3.33 rounds to 3, 11 / 3 = 3.67 rounds to 4.
      assert.deepEqual(durations(await column(q, Event, Event.span.div(3))), [DAY / 3n, 3n, 4n]);
      assert.deepEqual(durations(await column(q, Event, Event.span.mul(0.3))), [25_920_000_000n, 3n, 3n]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a duration parameter takes part in arithmetic (${kind})`, async () => {
    const { db, Event, q } = await world(kind);
    try {
      const expr = Event.at.add(param("d"));
      assert.equal(epoch((await column(q, Event, expr, { d: us(1n) }))[0]), epoch(AT[0]!) + 1n);
      assert.equal(epoch((await column(q, Event, expr, { d: Duration.fromParts({ micros: 500_000 }) }))[0]), epoch(AT[0]!) + 500_000n);
      const late = await q.filter(Event.at.add(Event.span).gt(stamp("2024-01-01T00:00:00Z", 5))).orderBy(Event.id).all();
      assert.deepEqual(late.map((r: any) => r.id), [1, 2, 3]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a pair the table does not list is an error (${kind})`, async () => {
    const { db, Event, q } = await world(kind);
    try {
      for (const expr of [Event.at.add(Event.until), Event.span.mul(Event.span), Event.span.sub(Event.at), Event.at.mul(2)]) {
        await assert.rejects(async () => q.select({ x: expr }).all(), /is not defined/);
      }
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a null operand gives null (${kind})`, async () => {
    const { db, Event, q } = await world(kind);
    try {
      const nulls = [null, null, null];
      for (const expr of [Event.at.add(Event.rest), Event.late.sub(Event.span), Event.late.sub(Event.at), Event.rest.add(Event.span), Event.rest.mul(2), Event.rest.div(2)]) {
        assert.deepEqual(await column(q, Event, expr), nulls);
      }
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a difference over a day and over a month is exact (${kind})`, async () => {
    const { db, Event, q } = await world(kind);
    try {
      const month = 60n * DAY + 43_200_500_000n;
      assert.deepEqual(durations(await column(q, Event, Event.at.sub(stamp("2023-12-31T00:00:00Z")))), [DAY, month + DAY, DAY]);
      assert.deepEqual(durations(await column(q, Event, Event.at.sub(stamp("2024-01-01T00:00:00Z")))), [0n, month, 0n]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a float parameter scales a duration (${kind})`, async () => {
    const { db, Event, q } = await world(kind);
    try {
      const expected = [25_920_000_000n, 3n, 3n];
      assert.deepEqual(durations(await column(q, Event, Event.span.mul(param("n", "float")), { n: 0.3 })), expected);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a division by zero is an error on both databases (${kind})`, async () => {
    const { db, Event, q } = await world(kind);
    try {
      for (const zero of [0, 0.0]) await assert.rejects(async () => q.select({ x: Event.span.div(zero) }).all(), /division by zero/);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a duration out of the 64-bit range is an error on both databases (${kind})`, async () => {
    const { db, Event, q } = await world(kind);
    try {
      const big = us(9_223_372_036_854_000_000n);
      const huge = 9_000_000_000_000_000_000n;
      for (const expr of [Event.span.mul(huge), Event.span.add(big), Event.span.sub(us(-9_223_372_036_854_000_000n)), Event.at.add(big)]) {
        await assert.rejects(async () => q.select({ x: expr }).all(), /out of/);
      }
    } finally { await db.dropTables(); await db.close(); }
  });
}
