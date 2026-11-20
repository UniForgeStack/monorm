/** `@@trigger` with the pgtrigger names: deferred checks, transition tables, change helpers and skippable triggers. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, DatabaseError, loads, QueryError, Registry, skipTriggers, type Database } from "../src/index.js";

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const MODEL = (bump: string, when: string, shout: string) => `
model TgItem {
  id Int @id @default(autoincrement())
  title String
  stock Int
  updated_at DateTime @default(now())
  version Int @default(0)
  @@map("tg_js_items")
  @@trigger(bump, when: after, operation: [update], condition: [any_change(title, stock)], func: "${bump}")
  @@trigger(shout, when: ${when}, operation: [insert], skippable: true, func: "${shout}")
}
`;

const BODIES: Record<string, string> = {
  postgresql: MODEL(
    "BEGIN UPDATE tg_js_items SET version = version + 1 WHERE id = NEW.id; RETURN NULL; END;",
    "before",
    "BEGIN NEW.title := upper(NEW.title); RETURN NEW; END;",
  ),
  sqlite: MODEL("UPDATE tg_js_items SET version = version + 1 WHERE id = NEW.id", "after", "UPDATE tg_js_items SET title = upper(title) WHERE id = NEW.id"),
};

const POSTGRES_ONLY = `
model TgLog {
  id Int @id @default(autoincrement())
  note String
  @@map("tg_js_log")
}
model TgStock {
  id Int @id @default(autoincrement())
  stock Int
  @@map("tg_js_stock")
  @@trigger(nonnegative, when: after, operation: [insert, update], deferrable: deferred, func: "BEGIN IF (SELECT stock FROM tg_js_stock WHERE id = NEW.id) < 0 THEN RAISE EXCEPTION 'negative stock'; END IF; RETURN NULL; END;")
  @@trigger(batch, referencing(new: n), when: after, operation: [insert], level: statement, func: "BEGIN INSERT INTO tg_js_log (note) SELECT 'inserted ' || count(*) FROM n; RETURN NULL; END;")
}
`;

const postgresUrl = process.env["ORM_TEST_DATABASE_URL"];
const urlOf = (provider: string) => (provider === "sqlite" ? "sqlite://:memory:" : postgresUrl!);

async function withDatabase(provider: string, body: (db: Database, models: Record<string, Any>) => Promise<void>, extra = "", datasource = ""): Promise<void> {
  const registry = new Registry();
  const models = loads(`datasource db {\n  provider = "${provider}"\n${datasource}}\n${BODIES[provider]}${extra}`, { registry }) as Record<string, Any>;
  const db = await connect(urlOf(provider), { registry, default: false });
  await db.dropTables();
  await db.createTables();
  try {
    await body(db, models);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

const version = async (db: Database) => Number((await db.fetch("SELECT version FROM tg_js_items"))[0]!["version"]);

for (const provider of ["sqlite", "postgresql"]) {
  test(`any_change fires and an updated_at only update does not (${provider})`, { skip: provider === "postgresql" && !postgresUrl }, () =>
    withDatabase(provider, async (db, m) => {
      await m["TgItem"].objects.using(db).insert({ title: "A", stock: 1 });
      await db.execute("UPDATE tg_js_items SET updated_at = '2001-02-03T00:00:00'");
      assert.equal(await version(db), 0);
      await db.execute("UPDATE tg_js_items SET title = 'b'");
      assert.equal(await version(db), 1);
      await db.execute("UPDATE tg_js_items SET stock = stock");
      assert.equal(await version(db), 1);
    }));
}

test("a deferred check fails at commit", { skip: !postgresUrl }, () =>
  withDatabase("postgresql", async (db, m) => {
    const Stock = m["TgStock"];
    await db.transaction(async () => {
      const row: Any = await Stock.objects.using(db).insert({ stock: -1 });
      await Stock.objects.using(db).filter(Stock.id.eq(row.id)).update({ stock: 5 });
    });
    assert.equal(Number((await db.fetch("SELECT stock FROM tg_js_stock"))[0]!["stock"]), 5);
    await assert.rejects(
      db.transaction(async () => {
        await Stock.objects.using(db).insert({ stock: -1 });
      }),
      (e: Any) => e instanceof DatabaseError && /negative stock/.test(e.message),
    );
    assert.equal((await db.fetch("SELECT id FROM tg_js_stock")).length, 1);
  }, POSTGRES_ONLY));

test("a statement trigger reads the transition table", { skip: !postgresUrl }, () =>
  withDatabase("postgresql", async (db, m) => {
    await m["TgStock"].objects.using(db).insertMany([{ stock: 1 }, { stock: 2 }, { stock: 3 }]);
    assert.deepEqual((await db.fetch("SELECT note FROM tg_js_log")).map((r) => r["note"]), ["inserted 3"]);
  }, POSTGRES_ONLY));

test("a skippable trigger skips under a bypass role and not outside it", { skip: !postgresUrl }, () =>
  withDatabase("postgresql", async (db, m) => {
    const Item = m["TgItem"];
    await Item.objects.using(db).insert({ title: "plain", stock: 1 });
    await skipTriggers(["shout"], () => Item.objects.using(db).insert({ title: "skipped", stock: 1 }));
    await db.execute("DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'monorm_tg_plain') THEN CREATE ROLE monorm_tg_plain; END IF; END $$");
    await db.execute("GRANT ALL ON ALL TABLES IN SCHEMA public TO monorm_tg_plain; GRANT ALL ON ALL SEQUENCES IN SCHEMA public TO monorm_tg_plain");
    await db.transaction(async () => {
      await db.execute("SET LOCAL SESSION AUTHORIZATION monorm_tg_plain");
      await skipTriggers(["shout"], () => Item.objects.using(db).insert({ title: "refused", stock: 1 }));
    });
    const titles = (await db.fetch("SELECT title FROM tg_js_items ORDER BY id")).map((r) => r["title"]);
    assert.deepEqual(titles, ["PLAIN", "skipped", "REFUSED"]);
  }, "", '  bypass_roles = ["postgres"]\n'));

test("a skippable trigger without bypass roles cannot be skipped", { skip: !postgresUrl }, () =>
  withDatabase("postgresql", async (db, m) => {
    await skipTriggers(["shout"], () => m["TgItem"].objects.using(db).insert({ title: "held", stock: 1 }));
    assert.deepEqual((await db.fetch("SELECT title FROM tg_js_items")).map((r) => r["title"]), ["HELD"]);
  }));

test("a skip block raises on sqlite and referencing is refused", async () => {
  await withDatabase("sqlite", async (db, m) => {
    await assert.rejects(skipTriggers(["shout"], () => m["TgItem"].objects.using(db).insert({ title: "x", stock: 1 })), (e: Any) => e instanceof QueryError && /sqlite has none/.test(e.message));
  });
  const source =
    'datasource db { provider = "sqlite" }\nmodel T { id Int @id\n @@trigger(t, referencing(new: n), when: after, operation: [insert], func: "SELECT 1") }';
  assert.throws(() => loads(source, { registry: new Registry() }), /referencing/);
});
