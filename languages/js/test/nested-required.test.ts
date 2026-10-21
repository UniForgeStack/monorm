/** A columns group keeps the nullability of its model field, whatever the levels below it hold. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, loads, Registry, type Database } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const SCHEMA = `
type Geo {
  lat Float
  lng Float?
}

type Wrap {
  inner Geo?
}

type Tagged {
  tag   String
  inner Geo?
}

model Plain {
  id Int @id
  g  Geo
}

model Wrapped {
  id Int  @id
  w  Wrap
}

model Opt {
  id Int     @id
  t  Tagged?
}
`;

const SQLITE = 'datasource db { provider = "sqlite" }\n';

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

for (const kind of ["postgres", "sqlite"] as const) {
  const scenario = (what: string, body: (w: World) => Promise<void>): void => {
    test(`nested required ${what} (${kind})`, async () => {
      const w = await world(kind);
      try {
        await body(w);
      } finally {
        await w.db.dropTables();
        await w.db.close();
      }
    });
  };

  scenario("a non-null columns group loads and round-trips", async ({ db, classes }) => {
    const { Geo } = classes.$types;
    const q = classes.Plain.objects.using(db);
    await q.insertMany([{ id: 1, g: new Geo({ lat: 1.5 }) }, { id: 2, g: new Geo({ lat: 2.5, lng: 3.5 }) }]);
    const rows = new Map<number, Any>((await q.orderBy(classes.Plain.id)).map((p: Any) => [p.id, p.g]));
    assert.ok(rows.get(1).equals(new Geo({ lat: 1.5, lng: null })));
    assert.ok(rows.get(2).equals(new Geo({ lat: 2.5, lng: 3.5 })));
  });

  scenario("a non-null group keeps a value whose middle level is null", async ({ db, classes }) => {
    const { Geo, Wrap, Tagged } = classes.$types;
    const wq = classes.Wrapped.objects.using(db);
    await wq.insertMany([{ id: 1, w: new Wrap({ inner: null }) }, { id: 2, w: new Wrap({ inner: new Geo({ lat: 1 }) }) }]);
    const oq = classes.Opt.objects.using(db);
    await oq.insertMany([{ id: 1, t: null }, { id: 2, t: new Tagged({ tag: "x", inner: null }) }]);
    const wrapped = new Map<number, Any>((await wq.orderBy(classes.Wrapped.id)).map((r: Any) => [r.id, r.w]));
    assert.ok(wrapped.get(1).equals(new Wrap({ inner: null })));
    assert.ok(wrapped.get(2).equals(new Wrap({ inner: new Geo({ lat: 1, lng: null }) })));
    const opt = new Map<number, Any>((await oq.orderBy(classes.Opt.id)).map((r: Any) => [r.id, r.t]));
    assert.equal(opt.get(1), null);
    assert.ok(opt.get(2).equals(new Tagged({ tag: "x", inner: null })));
  });

  scenario("a non-null group refuses null", async ({ db, classes }) => {
    const wq = classes.Wrapped.objects.using(db);
    const pq = classes.Plain.objects.using(db);
    await assert.rejects(wq.insert({ id: 1, w: null }), /not nullable/);
    await assert.rejects(pq.insert({ id: 1, g: null }), /not nullable/);
    await assert.rejects(pq.insert({ id: 1 }), /g/);
    assert.equal(await wq.count(), 0);
    assert.equal(await pq.count(), 0);
  });

  scenario("an omitted non-null group of optional subfields is an empty value", async ({ db, classes }) => {
    const { Wrap } = classes.$types;
    const wq = classes.Wrapped.objects.using(db);
    await wq.insert({ id: 1 });
    const row = await wq.filter(classes.Wrapped.id.eq(1)).first();
    assert.ok(row.w.equals(new Wrap({ inner: null })));
  });
}
