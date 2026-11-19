/** Nested JSON and column storage through migrations: backfills, renames and storage changes that keep the rows. */

import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";

import { connect, loads, Migrations, Migrator, Registry, type Database } from "../src/index.js";
import { extraMicros, keepMicros } from "../src/date-micros.js";
import { DATABASE_URL } from "./helpers.js";

/* eslint-disable @typescript-eslint/no-explicit-any */
type Any = any;

const NM_DB = new URL(DATABASE_URL).pathname.slice(1) + "_nested_migrations_js";
const NM_URL = Object.assign(new URL(DATABASE_URL), { pathname: "/" + NM_DB }).toString();
const SQLITE = 'datasource db { provider = "sqlite" }\n';
const DIALECTS = ["postgres", "sqlite"] as const;
type Dialect = (typeof DIALECTS)[number];
const DROP = "DROP TABLE IF EXISTS shop; DROP TABLE IF EXISTS monorm_migrations";

before(async () => {
  const admin = await connect(DATABASE_URL, { maxConnections: 1, default: false, registry: new Registry() });
  try {
    if (!(await admin.fetchText(`SELECT 1 FROM pg_database WHERE datname = '${NM_DB}'`)).length) {
      await admin.execute(`CREATE DATABASE "${NM_DB}"`);
    }
  } finally {
    await admin.close();
  }
});

const tmp = (): string => mkdtempSync(join(tmpdir(), "monorm-nested-migrations-"));

function models(source: string): Registry {
  const registry = new Registry();
  loads(source, { registry });
  return registry;
}

/** A connection bound to the models of `source`; SQLite lives in a file so that a second connection sees the rows. */
async function connected(dialect: Dialect, source: string, dir: string): Promise<{ db: Database; Shop: Any }> {
  const registry = new Registry();
  const classes = loads(source, { registry }) as Any;
  const db = await connect(dialect === "sqlite" ? `sqlite://${join(dir, "live.db")}` : NM_URL, { registry, default: false, maxConnections: 2 });
  return { db, Shop: classes["Shop"] };
}

async function withDatabase(dialect: Dialect, source: string, body: (db: Database, dir: string) => Promise<void>): Promise<void> {
  const dir = tmp();
  const { db } = await connected(dialect, source, dir);
  try {
    await db.execute(DROP);
    await body(db, dir);
  } finally {
    await db.execute(DROP);
    await db.close();
  }
}

async function migrate(db: Database, dir: string, source: string): Promise<void> {
  const migs = new Migrations(dir, models(source));
  assert.ok(migs.make().length);
  await new Migrator(db, migs).upgrade(undefined, { allowDestructive: true });
}

async function downgrade(db: Database, dir: string, source: string): Promise<void> {
  await new Migrator(db, new Migrations(dir, models(source))).downgrade({ allowDestructive: true });
}

async function insertAddresses(db: Database, rows: [number, unknown][]): Promise<void> {
  const values = rows.map(([id, value]) => `(${id}, '${JSON.stringify(value)}')`).join(", ");
  await db.execute(`INSERT INTO shop (id, address) VALUES ${values}`);
}

async function addresses(db: Database): Promise<Record<number, Any>> {
  return Object.fromEntries((await db.fetchText("SELECT id, address FROM shop")).map(([id, doc]) => [Number(id), JSON.parse(doc!)]));
}

const prefixOf = (dialect: Dialect): string => (dialect === "sqlite" ? SQLITE : "");

// -- renames of JSON subfields ---------------------------------------------------------------------

const KEYS_V1 = `
type Meta {
  size Int
}

type Item {
  flag  Boolean
  n     Int
  tags  String[]
  meta  Meta
  maybe String?
}

model Shop {
  id      Int  @id
  address Item @storage(json)
}
`;
const KEYS_V2 = KEYS_V1.replace("flag  Boolean", 'on Boolean @renamed_from("flag")')
  .replace("  n     Int", '  count Int @renamed_from("n")')
  .replace("  tags  String[]", '  labels String[] @renamed_from("tags")')
  .replace("  meta  Meta", '  info Meta @renamed_from("meta")')
  .replace("  size Int", '  size Int @renamed_from("size")')
  .replace("  maybe String?", '  perhaps String? @renamed_from("maybe")');
const OLD_ITEM = { flag: true, n: 3, tags: ["a", "b"], meta: { size: 7 }, maybe: null };
const NEW_ITEM = { on: true, count: 3, labels: ["a", "b"], info: { size: 7 }, perhaps: null };

const GROUP_V1 = `
type Geo {
  y Float
}

type Place {
  city String
  geo  Geo
}

model Shop {
  id      Int   @id
  address Place @storage(json)
}
`;
const GROUP_V2 = GROUP_V1.replace("  y Float", '  lat Float @renamed_from("y")').replace("  geo  Geo", '  pos Geo @renamed_from("geo")');

const LIST_V1 = `
type Geo {
  y Float
}

type Place {
  city String
  geos Geo[]
}

model Shop {
  id      Int     @id
  address Place[] @storage(json)
}
`;
const LIST_V2 = LIST_V1.replace("  y Float", '  lat Float @renamed_from("y")');

const FILL_V1 = `
type Geo {
  lat Float
}

type Place {
  city String
  geos Geo[]
}

model Shop {
  id      Int     @id
  address Place[] @storage(json)
}
`;
const FILL_V2 = FILL_V1.replace("  lat Float", "  lat Float\n  alt Float @default(0)").replace("  city String", '  city String\n  zip  String @default("0")');

for (const dialect of DIALECTS) {
  const prefix = prefixOf(dialect);

  // case: nested_migrations::a_renamed_json_subfield_moves_its_value_and_down_moves_it_back
  test(`a renamed JSON subfield moves its value and down moves it back (${dialect})`, async () => {
    await withDatabase(dialect, prefix + KEYS_V1, async (db, dir) => {
      await migrate(db, dir, prefix + KEYS_V1);
      await insertAddresses(db, [[1, OLD_ITEM], [2, { flag: false, n: 0, tags: [], meta: { size: 1 } }]]);
      await migrate(db, dir, prefix + KEYS_V2);
      const got = await addresses(db);
      assert.deepEqual(got[1], NEW_ITEM);
      assert.equal(got[1].on, true);
      assert.ok("perhaps" in got[1]);
      assert.deepEqual(got[2], { on: false, count: 0, labels: [], info: { size: 1 } });
      assert.equal(got[2].on, false);
      await downgrade(db, dir, prefix + KEYS_V2);
      const back = await addresses(db);
      assert.deepEqual(back[1], OLD_ITEM);
      assert.equal(back[1].flag, true);
    });
  });

  // case: nested_migrations::a_renamed_json_group_and_its_renamed_child_move_together
  test(`a renamed JSON group and its renamed child move together (${dialect})`, async () => {
    await withDatabase(dialect, prefix + GROUP_V1, async (db, dir) => {
      await migrate(db, dir, prefix + GROUP_V1);
      await insertAddresses(db, [
        [1, { city: "A", geo: { y: 1.5 } }],
        [2, { city: "B", pos: { lat: 2 } }], // already the new shape: left alone
        [3, { city: "C", geo: null }],
      ]);
      await migrate(db, dir, prefix + GROUP_V2);
      assert.deepEqual(await addresses(db), {
        1: { city: "A", pos: { lat: 1.5 } },
        2: { city: "B", pos: { lat: 2 } },
        3: { city: "C", pos: null },
      });
      await downgrade(db, dir, prefix + GROUP_V2);
      assert.deepEqual((await addresses(db))[1], { city: "A", geo: { y: 1.5 } });
    });
  });

  // case: nested_migrations::a_renamed_json_subfield_inside_lists_renames_every_element_in_order
  test(`a renamed JSON subfield inside lists renames every element in order (${dialect})`, async () => {
    await withDatabase(dialect, prefix + LIST_V1, async (db, dir) => {
      await migrate(db, dir, prefix + LIST_V1);
      const old = [
        { city: "A", geos: [{ y: 1 }, { y: 2 }, { y: 3 }] },
        { city: "B", geos: [] },
        { city: "C", geos: [{ y: 4 }] },
      ];
      await insertAddresses(db, [[1, old], [2, []], [3, [{ city: "D", geos: null }]]]);
      await migrate(db, dir, prefix + LIST_V2);
      const got = await addresses(db);
      assert.deepEqual(got[1][0].geos.map((g: Any) => g.lat), [1, 2, 3]);
      assert.deepEqual(got[1][1].geos, []);
      assert.deepEqual(got[1][2].geos, [{ lat: 4 }]);
      assert.deepEqual(got[2], []);
      assert.deepEqual(got[3], [{ city: "D", geos: null }]);
      await downgrade(db, dir, prefix + LIST_V2);
      assert.deepEqual((await addresses(db))[1], old);
    });
  });

  // case: nested_migrations::applying_a_fill_inside_lists_reaches_every_element_and_keeps_the_order
  test(`applying a fill inside lists reaches every element and keeps the order (${dialect})`, async () => {
    await withDatabase(dialect, prefix + FILL_V1, async (db, dir) => {
      await migrate(db, dir, prefix + FILL_V1);
      await insertAddresses(db, [
        [1, [{ city: "A", geos: [{ lat: 1 }, { lat: 2, alt: 9 }] }, { city: "B", zip: "q", geos: [] }]],
        [2, []],
        [3, [{ city: "C", geos: null }, null]],
      ]);
      await migrate(db, dir, prefix + FILL_V2);
      const got = await addresses(db);
      assert.deepEqual(got[1], [
        { city: "A", zip: "0", geos: [{ lat: 1, alt: 0 }, { lat: 2, alt: 9 }] },
        { city: "B", zip: "q", geos: [] },
      ]);
      assert.deepEqual(got[2], []);
      assert.deepEqual(got[3], [{ city: "C", zip: "0", geos: null }, null]);
    });
  });

  // case: nested_migrations::a_list_element_that_is_not_an_object_keeps_its_exact_number_through_a_rename
  test(`a list element that is not an object keeps its exact number through a rename (${dialect})`, async () => {
    await withDatabase(dialect, prefix + LIST_V1, async (db, dir) => {
      await migrate(db, dir, prefix + LIST_V1);
      // JSON text by hand: a JavaScript number cannot hold these two
      await db.execute(`INSERT INTO shop (id, address) VALUES (1, '[{"city":"A","geos":[{"y":1}]},18446744073709551615,9007199254740993,1.5]')`);
      await migrate(db, dir, prefix + LIST_V2);
      const text = (await db.fetchText("SELECT address FROM shop"))[0]![0]!;
      const got = JSON.parse(text);
      assert.deepEqual(got[0].geos, [{ lat: 1 }]);
      assert.ok(text.includes("18446744073709551615") && text.includes("9007199254740993"), text);
      assert.equal(got[3], 1.5);
    });
  });
}

// -- storage changes through the ORM ---------------------------------------------------------------

const ORM_SOURCE = (storage: string, extra: string): string => `
type Geo {
  lat Float
  lng Float?
}

type Stamp {
${extra}  label String
  at    DateTime
  flag  Json?
  n     BigInt
  geo   Geo?
}

model Shop {
  id   Int    @id
  data Stamp? ${storage}
}
`;

const moment = (iso: string, micros = 0): Date => {
  const d = new Date(iso);
  if (micros) keepMicros(d, micros);
  return d;
};
const STAMPS: [number, string, Date, unknown, bigint, { lat: number; lng: number | null } | null][] = [
  [1, "a", moment("2026-01-02T03:04:05.123Z", 456), true, 2n ** 60n, { lat: 1.5, lng: null }],
  [2, "b", moment("2026-01-02T03:04:05.500Z"), "text", -5n, null],
  [3, "c", moment("2026-12-31T23:59:59.000Z"), false, 0n, { lat: 0.5, lng: 2.5 }],
  [4, "d", moment("2026-12-31T23:59:59.120Z"), { k: [1, 2] }, 1n, null],
];

const ormSource = (dialect: Dialect, storage = "", extra = ""): string => prefixOf(dialect) + ORM_SOURCE(storage, extra);

function stampRows(): Any[] {
  const rows: Any[] = STAMPS.map(([id, label, at, flag, n, geo]) => ({ id, data: { label, at, flag, n, geo } }));
  return [...rows, { id: 5, data: null }];
}

/** What an ORM read of the rows shows, as plain values. */
function seen(rows: Any[]): Record<number, Any> {
  const out: Record<number, Any> = {};
  for (const r of rows) {
    const d = r.data;
    out[r.id] =
      d === null
        ? null
        : {
            label: d.label,
            at: `${d.at.toISOString()}+${extraMicros(d.at)}`,
            flag: d.flag,
            n: d.n,
            geo: d.geo === null ? null : { lat: d.geo.lat, lng: d.geo.lng },
            ...("code" in d ? { code: d.code } : {}),
          };
  }
  return out;
}

/** Reads and filters through a fresh connection bound to `source`; the filter is `eq` on a DateTime leaf. */
async function read(dialect: Dialect, source: string, dir: string): Promise<{ rows: Record<number, Any>; hits: Record<number, number[]> }> {
  const { db, Shop } = await connected(dialect, source, dir);
  try {
    const q = Shop.objects.using(db);
    const rows = seen(await q.orderBy(Shop.id));
    const hits: Record<number, number[]> = {};
    for (const [id, , at] of STAMPS) hits[id] = (await q.filter(Shop.data.at.eq(at)).orderBy(Shop.id)).map((r: Any) => r.id);
    return { rows, hits };
  } finally {
    await db.close();
  }
}

const EVERY_HIT = Object.fromEntries(STAMPS.map(([id]) => [id, [id]]));

for (const dialect of DIALECTS) {
  for (const columns of ["", '@storage(columns, prefix: "d_")']) {
    for (const first of ["columns", "json"] as const) {
      // case: nested_migrations::rows_written_through_the_orm_read_and_filter_after_a_storage_change
      test(`rows written through the ORM read and filter after a storage change (${dialect}, ${columns || "plain"}, ${first})`, async () => {
        const where = { columns, json: "@storage(json)" };
        const other = first === "columns" ? "json" : "columns";
        const sources = { columns: ormSource(dialect, where.columns), json: ormSource(dialect, where.json) };
        const dir = tmp();
        const migrations = join(dir, "m");
        const { db, Shop } = await connected(dialect, sources[first], dir);
        try {
          await db.execute(DROP);
          await migrate(db, migrations, sources[first]);
          await Shop.objects.using(db).insertMany(stampRows());
          const firstSeen = seen(await Shop.objects.using(db).orderBy(Shop.id));
          assert.equal(firstSeen[1].at, `${STAMPS[0]![2].toISOString()}+456`);
          assert.equal(firstSeen[1].flag, true);
          assert.equal(firstSeen[3].flag, false);
          await migrate(db, migrations, sources[other]);
          let got = await read(dialect, sources[other], dir);
          assert.deepEqual(got.rows, firstSeen);
          assert.deepEqual(got.hits, EVERY_HIT);
          await downgrade(db, migrations, sources[other]);
          got = await read(dialect, sources[first], dir);
          assert.deepEqual(got.rows, firstSeen);
          assert.deepEqual(got.hits, EVERY_HIT);
        } finally {
          await db.execute(DROP);
          await db.close();
        }
      });
    }
  }

  // case: nested_migrations::a_new_subfield_with_a_default_is_filled_by_a_storage_change_through_the_orm
  test(`a new subfield with a default is filled by a storage change through the ORM (${dialect})`, async () => {
    const extra = '  code  String @default("z")\n';
    for (const [first, other] of [["json", "columns"], ["columns", "json"]] as const) {
      const where = { columns: "", json: "@storage(json)" };
      const dir = tmp();
      const migrations = join(dir, `from_${first}`);
      const before = ormSource(dialect, where[first]);
      const after = ormSource(dialect, where[other], extra);
      const { db, Shop } = await connected(dialect, before, dir);
      try {
        await db.execute(DROP);
        await migrate(db, migrations, before);
        await Shop.objects.using(db).insertMany(stampRows());
        await migrate(db, migrations, after);
        const second = await connected(dialect, after, dir);
        let rows: Record<number, Any>;
        try {
          rows = seen(await second.Shop.objects.using(second.db).orderBy(second.Shop.id));
        } finally {
          await second.db.close();
        }
        assert.equal(rows[5], null);
        assert.deepEqual(
          Object.fromEntries(Object.entries(rows).filter(([, r]) => r !== null).map(([i, r]) => [i, r.code])),
          Object.fromEntries(STAMPS.map(([id]) => [id, "z"])),
        );
      } finally {
        await db.execute(DROP);
        await db.close();
      }
    }
  });
}

after(() => undefined);
