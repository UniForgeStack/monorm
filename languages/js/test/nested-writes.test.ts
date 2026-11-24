/** A JSON-stored nested type takes no dynamic default; a `date` subfield stores a date. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, loads, Registry, type Database } from "../src/index.js";
import { keepMicros } from "../src/date-micros.js";
import { DATABASE_URL } from "./helpers.js";

const SCHEMA = `
type Stamp {
  at  DateTime @default(now())
  tok String   @client_default(uuid())
  n   Int
}

model Cols {
  id Int   @id
  s  Stamp
}

type Dated {
  day DateTime @db.Date
}

model Event {
  id Int   @id
  d  Dated @storage(json)
}

model EventCols {
  id Int   @id
  d  Dated
}

type Moment {
  at DateTime
}

model Clock {
  id Int    @id
  m  Moment @storage(json)
}
`;

const SQLITE = 'datasource db { provider = "sqlite" }\n';
const WHEN = new Date("2024-01-02T03:04:05Z");

/* eslint-disable @typescript-eslint/no-explicit-any */
type Any = any;

interface World {
  db: Database;
  classes: Any;
}

async function world(kind: "postgres" | "sqlite"): Promise<World> {
  const registry = new Registry();
  const classes = loads((kind === "sqlite" ? SQLITE : "") + SCHEMA, { registry }) as Any;
  const db = await connect(kind === "sqlite" ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false, maxConnections: 2 });
  await db.dropTables();
  await db.createTables();
  return { db, classes };
}

for (const field of ["at DateTime @default(now())", 'at String @client_default("x")', `at String @default(dbgenerated("'x'"))`]) {
  test(`a json field refuses a type with a dynamic default (${field}) (${field})`, () => {
    const schema = `type Stamp {\n  ${field}\n}\nmodel M {\n  id Int @id\n  s Stamp @storage(json)\n}`;
    assert.throws(() => loads(schema, { registry: new Registry() }), /Stamp\.at has a dynamic default/);
  });
}

test("a json field refuses a dynamic default below a nested level", () => {
  const schema = "type In {\n  at DateTime @default(now())\n}\ntype Out {\n  i In\n}\nmodel M {\n  id Int @id\n  o Out @storage(json)\n}";
  assert.throws(() => loads(schema, { registry: new Registry() }), /In\.at has a dynamic default/);
});

for (const kind of ["postgres", "sqlite"] as const) {
  const scenario = (what: string, body: (w: World) => Promise<void>): void => {
    test(`nested writes ${what} (${kind})`, async () => {
      const w = await world(kind);
      try {
        await body(w);
      } finally {
        await w.db.dropTables();
        await w.db.close();
      }
    });
  };

  scenario("a columns subfield with a dynamic default may be left out", async ({ db, classes }) => {
    const q = classes.Cols.objects.using(db);
    await q.insert({ id: 1, s: { n: 1 } });
    const got = (await q.get(classes.Cols.id.eq(1))).s;
    assert.equal(got.n, 1);
    assert.ok(got.at !== null && got.tok);
  });

  scenario("a datetime for a date subfield is stored as a date", async ({ db, classes }) => {
    const q = classes.Event.objects.using(db);
    await q.insertMany([{ id: 1, d: { day: WHEN } }, { id: 2, d: { day: new Date("2024-01-02T00:00:00Z") } }]);
    await classes.EventCols.objects.using(db).insert({ id: 1, d: { day: WHEN } });
    const days = (await q.orderBy(classes.Event.id)).map((e: Any) => e.d.day.toISOString().slice(0, 10));
    assert.deepEqual(days, ["2024-01-02", "2024-01-02"]);
    const cols = await classes.EventCols.objects.using(db).get(classes.EventCols.id.eq(1));
    assert.equal(cols.d.day.toISOString().slice(0, 10), "2024-01-02");
  });

  scenario("a datetime subfield is stored as fixed-width utc text that sorts by time", async ({ db, classes }) => {
    const at = (iso: string, micros = 0): Date => {
      const d = new Date(iso);
      keepMicros(d, micros);
      return d;
    };
    const times = [at("2024-01-02T03:04:05.123Z", 456), at("2024-01-02T03:04:05Z"), at("2024-01-02T03:04:05.000Z", 1), at("2023-12-31T23:59:59.999Z", 999)];
    await classes.Clock.objects.using(db).insertMany(times.map((t, id) => ({ id, m: { at: t } })));
    const rows = await db.fetchText("SELECT id, m FROM clock");
    const texts = new Map(rows.map(([id, doc]) => [Number(id), (JSON.parse(doc!) as { at: string }).at]));
    for (const t of texts.values()) assert.match(t, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{6}Z$/);
    const byText = [...texts.keys()].sort((a, b) => (texts.get(a)! < texts.get(b)! ? -1 : 1));
    assert.deepEqual(byText, [3, 1, 2, 0]);
    assert.equal(texts.get(1), "2024-01-02T03:04:05.000000Z");
    assert.equal(texts.get(2), "2024-01-02T03:04:05.000001Z");
  });
}
