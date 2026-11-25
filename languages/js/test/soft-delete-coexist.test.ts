/** `hard_delete_setting`: soft delete next to another ORM that keeps a rule and its own triggers.
 * The fixture table `sd_pick` uses the SQL of the other ORM: a rule that turns a `DELETE` into an `UPDATE`
 * unless `rules.hard_deletion` is `'on'`, and a trigger that cascades the stamp to its child table. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, loads, param, Registry, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const SCHEMA = (key: string, pick: string) => `
datasource db {
 provider = "postgresql"
${key}}
model Spec {
 id Int @id @default(autoincrement())
 name String
 guitars Guitar[]
 wires Wire[]
 @@soft_delete.deleted_at
 @@map("sd_spec")
}
model Guitar {
 id Int @id @default(autoincrement())
 spec_id Int
 spec Spec @relation(fields: [spec_id], references: [id], onDelete: Cascade)
 straps Strap[]
 @@soft_delete.deleted_at
 @@map("sd_guitar")
}
model Strap {
 id Int @id @default(autoincrement())
 guitar_id Int
 guitar Guitar @relation(fields: [guitar_id], references: [id], onDelete: Cascade)
 @@soft_delete.deleted_at
 @@map("sd_strap")
}
model Wire {
 id Int @id @default(autoincrement())
 spec_id Int
 spec Spec @relation(fields: [spec_id], references: [id], onDelete: Cascade) @soft_delete.keep_child_rows
 @@map("sd_wire")
}
model PickItem {
 id Int @id
 pick_id Int
 @@managed(false)
 @@map("sd_pick_item")
}
${pick}`;

const KEY = ' hard_delete_setting = "rules.hard_deletion"\n';

// Trusted only with the key: it adds no object to a table that another service owns.
const PICK = `model Pick {
 id Int @id
 name String
 @@managed(false)
 @@soft_delete.deleted_at
 @@map("sd_pick")
}
`;

// What the other ORM keeps in its own migrations: a table, a rule and a trigger that cascades the stamp.
const PICKS = [
  "CREATE TABLE sd_pick (id integer PRIMARY KEY, name text NOT NULL, deleted_at timestamptz)",
  "CREATE TABLE sd_pick_item (id serial PRIMARY KEY, pick_id integer NOT NULL REFERENCES sd_pick(id), deleted_at timestamptz)",
  `CREATE RULE sd_pick_soft AS ON DELETE TO sd_pick
    WHERE coalesce(current_setting('rules.hard_deletion', true), '') <> 'on'
    DO INSTEAD UPDATE sd_pick SET deleted_at = now() WHERE id = OLD.id`,
  `CREATE FUNCTION sd_pick_cascade() RETURNS trigger LANGUAGE plpgsql AS $$
   BEGIN
     UPDATE sd_pick_item SET deleted_at = NEW.deleted_at WHERE pick_id = NEW.id AND deleted_at IS NULL;
     RETURN NULL;
   END $$`,
  `CREATE TRIGGER sd_pick_cascade AFTER UPDATE OF deleted_at ON sd_pick FOR EACH ROW
    WHEN (OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL) EXECUTE FUNCTION sd_pick_cascade()`,
];
const DROP_PICKS = ["DROP TABLE IF EXISTS sd_pick_item", "DROP TABLE IF EXISTS sd_pick CASCADE", "DROP FUNCTION IF EXISTS sd_pick_cascade()"];

// The same rule on a MonOrm table (P4), and a rule with no condition (P5).
const SPEC_RULE = `CREATE RULE sd_spec_soft AS ON DELETE TO sd_spec
  WHERE coalesce(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD UPDATE sd_spec SET deleted_at = now() WHERE id = OLD.id AND deleted_at IS NULL`;
const ALWAYS_RULE = "CREATE RULE sd_pick_always AS ON DELETE TO sd_pick DO INSTEAD UPDATE sd_pick SET deleted_at = now() WHERE id = OLD.id";

const base = process.env["ORM_TEST_DATABASE_URL"];
const skip = !hasExtension("monorm.soft-delete") || !base;

interface World {
  db: Database;
  m: Record<string, Any>;
}

async function world(key: string, body: (w: World) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const m = loads(SCHEMA(key, key ? PICK : ""), { registry }) as Record<string, Any>;
  const db = await connect(base!, { registry, default: false });
  try {
    for (const sql of DROP_PICKS) await db.execute(sql);
    await db.dropTables();
    await db.createTables();
    for (const sql of PICKS) await db.execute(sql);
    await body({ db, m });
  } finally {
    for (const sql of DROP_PICKS) await db.execute(sql);
    await db.dropTables();
    await db.close();
  }
}

const alive = async (db: Database, table: string): Promise<number[]> =>
  ((await db.fetch(`SELECT id FROM ${table} ORDER BY id`)) as Any[]).map((r) => r.id);
const stamped = async (db: Database, table: string): Promise<number[]> =>
  ((await db.fetch(`SELECT id FROM ${table} WHERE deleted_at IS NOT NULL ORDER BY id`)) as Any[]).map((r) => r.id);

async function seed({ db, m }: World): Promise<void> {
  await m["Spec"].objects.using(db).insertMany([1, 2, 3].map((id) => ({ id, name: `s${id}` })));
  await m["Guitar"].objects.using(db).insertMany([{ specId: 1 }, { specId: 1 }, { specId: 2 }]);
  await m["Wire"].objects.using(db).insertMany([{ specId: 1 }, { specId: 2 }]);
  await db.execute("INSERT INTO sd_pick (id, name) VALUES (1, 'strat'), (2, 'tele')");
  await db.execute("INSERT INTO sd_pick_item (pick_id) VALUES (1), (2)");
}

const scenario = (what: string, body: (w: World) => Promise<void>, key = KEY) => test(what, { skip }, () => world(key, body));

scenario("hard delete removes a live row and the next delete in the transaction soft deletes", async (w) => {
  const { db, m } = w;
  await seed(w);
  const specs = m["Spec"].objects.using(db);
  const events: Any[] = [];
  db.onQuery((e) => void events.push(e));
  await db.transaction(async () => {
    assert.equal(await specs.filter(m["Spec"].id.eq(1)).hardDelete(), 1);
    assert.deepEqual(await alive(db, "sd_spec"), [2, 3]);
    assert.equal(await specs.filter(m["Spec"].id.eq(2)).delete(), 1);
    await db.execute("DELETE FROM sd_spec WHERE id = 3");
    assert.deepEqual(await alive(db, "sd_spec"), [2, 3]);
    assert.deepEqual(await stamped(db, "sd_spec"), [2, 3]);
  });
  assert.equal(events[0].settings["rules.hard_deletion"], "on");
  assert.ok(events.slice(1).every((e) => e.settings["rules.hard_deletion"] !== "on"));
  // a plain delete outside a transaction never carries the setting
  const [row] = (await db.fetch("SELECT current_setting('rules.hard_deletion', true) AS s")) as Any[];
  assert.ok(row.s === null || row.s === "");
});

scenario("a hard delete removes the live rows of an instance and of a held statement", async (w) => {
  const { db, m } = w;
  await seed(w);
  const Spec = m["Spec"];
  const first = await Spec.objects.using(db).filter(Spec.id.eq(1)).first();
  await first.hardDelete();
  assert.deepEqual(await alive(db, "sd_spec"), [2, 3]);
  const held = Spec.objects.using(db).filter(Spec.id.eq(param("id"))).hardDelete().prepare();
  assert.equal(await held.run({ id: 2 }), 1);
  assert.deepEqual(await alive(db, "sd_spec"), [3]);
  await Spec.objects.using(db).filter(Spec.id.eq(3)).delete();
  assert.deepEqual(await stamped(db, "sd_spec"), [3]);
});

scenario("a raw hard deletion setting removes a live parent and its cascade children", async (w) => {
  const { db } = w;
  await seed(w);
  await db.transaction(async () => {
    await db.execute("SELECT set_config('rules.hard_deletion', 'on', true)");
    await db.execute("DELETE FROM sd_spec WHERE id = 1");
  });
  assert.deepEqual(await alive(db, "sd_spec"), [2, 3]);
  assert.deepEqual(await alive(db, "sd_wire"), [2]);
  assert.deepEqual((((await db.fetch("SELECT spec_id FROM sd_guitar")) as Any[]).map((r) => r.spec_id)), [2]);
  assert.deepEqual(await stamped(db, "sd_guitar"), []);
});

scenario("without the key a hard delete of a live row soft deletes it", async (w) => {
  const { db, m } = w;
  await seed(w);
  assert.equal(await m["Spec"].objects.using(db).filter(m["Spec"].id.eq(1)).hardDelete(), 0);
  assert.deepEqual(await alive(db, "sd_spec"), [1, 2, 3]);
  assert.deepEqual(await stamped(db, "sd_spec"), [1]);
  assert.deepEqual(await stamped(db, "sd_guitar"), [1, 2]);
  const defs = ((await db.fetch("SELECT pg_get_triggerdef(oid) AS d FROM pg_trigger WHERE tgrelid = 'sd_spec'::regclass AND NOT tgisinternal")) as Any[]).map((r) => r.d).join(" ");
  assert.ok(!defs.includes("hard_deletion") && defs.includes("soft_delete"));
}, "");

scenario("with the key the delete and cascade triggers read the setting", async ({ db }) => {
  const names: Record<string, Record<string, string>> = {};
  for (const table of ["sd_spec", "sd_guitar"]) {
    const defs = (await db.fetch(`SELECT tgname AS n, pg_get_triggerdef(oid) AS d FROM pg_trigger WHERE tgrelid = '${table}'::regclass AND NOT tgisinternal`)) as Any[];
    names[table] = Object.fromEntries(defs.map((r) => [r.n, r.d]));
  }
  for (const [table, triggers] of Object.entries(names)) {
    assert.ok(Object.values(triggers).some((d) => d.includes("IS DISTINCT FROM 'on'")), table);
  }
  const guitar = Object.entries(names["sd_guitar"]!);
  assert.ok(guitar.some(([n]) => n.includes("soft_delete_guard")));
  const guards = guitar.filter(([n]) => n.includes("guard"));
  assert.ok(guards.some(([n]) => n === "soft_delete_guard_spec_cascade"));
  assert.ok(guards.every(([, d]) => d.includes("IS DISTINCT FROM 'on'")));
});

scenario("a moved and deleted row archives its children only while the setting is off", async (w) => {
  const { db, m } = w;
  await seed(w);
  await m["Strap"].objects.using(db).insertMany([{ guitarId: 1 }, { guitarId: 2 }, { guitarId: 3 }]);
  const move = (id: number) => `UPDATE sd_guitar SET spec_id = 3, deleted_at = now() WHERE id = ${id}`;
  await db.transaction(async () => {
    await db.execute("SELECT set_config('rules.hard_deletion', 'on', true)");
    await db.execute(move(1));
  });
  // The cascade and its copy for a moved row both skip, so the strap stays live.
  assert.deepEqual(await stamped(db, "sd_guitar"), [1]);
  assert.deepEqual(await stamped(db, "sd_strap"), []);
  await db.execute(move(2));
  assert.deepEqual(await stamped(db, "sd_strap"), [2]);
});

scenario("a rule of the other ORM on a monorm table lets a hard delete remove the row", async (w) => {
  const { db, m } = w;
  await seed(w);
  await db.execute(SPEC_RULE);
  assert.ok((await m["Spec"].objects.using(db).filter(m["Spec"].id.eq(1)).hardDelete()) >= 0);
  assert.deepEqual(await alive(db, "sd_spec"), [2, 3]);
  await m["Spec"].objects.using(db).filter(m["Spec"].id.eq(2)).delete();
  assert.deepEqual(await alive(db, "sd_spec"), [2, 3]);
  assert.deepEqual(await stamped(db, "sd_spec"), [2]);
});

scenario("the trigger of monorm breaks the hard delete path of the rule when there is no key", async (w) => {
  const { db } = w;
  await seed(w);
  await db.execute(SPEC_RULE);
  await db.transaction(async () => {
    await db.execute("SELECT set_config('rules.hard_deletion', 'on', true)");
    await db.execute("DELETE FROM sd_spec WHERE id = 1");
  });
  assert.deepEqual(await alive(db, "sd_spec"), [1, 2, 3]);
  assert.deepEqual(await stamped(db, "sd_spec"), [1]);
}, "");

scenario("an unmanaged table with a rule is soft deleted by the rule and hard deleted with the setting", async (w) => {
  const { db, m } = w;
  await seed(w);
  const Pick = m["Pick"];
  const picks = Pick.objects.using(db);
  // a model with no trigger of MonOrm soft-deletes in the engine, and the trigger of the other ORM cascades the stamp
  assert.equal(await picks.filter(Pick.id.eq(1)).delete(), 1);
  assert.deepEqual(await alive(db, "sd_pick"), [1, 2]);
  assert.deepEqual(await stamped(db, "sd_pick"), [1]);
  assert.deepEqual(await stamped(db, "sd_pick_item"), [1]);
  await db.execute("DELETE FROM sd_pick_item");
  assert.equal(await picks.filter(Pick.id.eq(1)).hardDelete(), 1);
  assert.deepEqual(await alive(db, "sd_pick"), [2]);
  // a live row is removed too: the setting turns the rule off for this statement
  assert.equal(await picks.filter(Pick.id.eq(2)).hardDelete(), 1);
  assert.deepEqual(await alive(db, "sd_pick"), []);
});

scenario("a raw delete under the rule reports no row and soft deletes the row", async (w) => {
  const { db } = w;
  await seed(w);
  assert.equal(await db.execute("DELETE FROM sd_pick WHERE id = 2"), 0);
  assert.deepEqual(await stamped(db, "sd_pick"), [2]);
  assert.deepEqual(await stamped(db, "sd_pick_item"), [2]);
  await db.execute("DROP RULE sd_pick_soft ON sd_pick");
  await db.execute(ALWAYS_RULE);
  assert.equal(await db.execute("DELETE FROM sd_pick WHERE id = 1"), 0);
  assert.deepEqual(await stamped(db, "sd_pick"), [1, 2]);
});

scenario("the guard of a child skips when the setting is on", async (w) => {
  const { db } = w;
  await seed(w);
  await db.execute("UPDATE sd_spec SET deleted_at = now() WHERE id = 3");
  await db.execute("INSERT INTO sd_guitar (spec_id) VALUES (3)");
  assert.deepEqual(await stamped(db, "sd_guitar"), [4]);
  await db.transaction(async () => {
    await db.execute("SELECT set_config('rules.hard_deletion', 'on', true)");
    await db.execute("INSERT INTO sd_guitar (spec_id) VALUES (3)");
    await db.execute("UPDATE sd_guitar SET spec_id = 3 WHERE id = 1");
  });
  assert.deepEqual(await stamped(db, "sd_guitar"), [4]);
});

test("an unmanaged soft delete model needs the key in the datasource", { skip: !hasExtension("monorm.soft-delete") }, () => {
  assert.throws(() => loads(SCHEMA("", PICK), { registry: new Registry() }), /Pick.*unmanaged.*`hard_delete_setting`/s);
  loads(SCHEMA(KEY, PICK), { registry: new Registry() });
});
