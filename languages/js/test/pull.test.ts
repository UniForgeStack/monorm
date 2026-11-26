/** `pull()`, baseline and drift against a live database. The Postgres tests use a
 * database of their own (`<test database>_pull_js`) and recreate its `public` schema. */

import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, afterEach, before, test } from "node:test";

import { MigrationError, Migrations, Migrator, Registry, connect, load, loads, pull, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";
import { DATABASE_URL } from "./helpers.js";

// the repository root: from test/ (Bun) or dist/test/ (Node)
const ROOT = [join(import.meta.dirname, "..", "..", ".."), join(import.meta.dirname, "..", "..", "..", "..")].find((d) => existsSync(join(d, "examples")))!;
const FIXTURE = readFileSync(join(ROOT, "fixtures", "pull_live.sql"), "utf8");

const PULL_DB = new URL(DATABASE_URL).pathname.slice(1) + "_pull_js";
const PULL_URL = Object.assign(new URL(DATABASE_URL), { pathname: "/" + PULL_DB }).toString();

// What the fixture holds that the schema language can't say (see the `gap:` lines).
// the view comes back around the table step: the diff cannot know what a view body reads
const EXPECTED = [
  "drop view live_bundles",
  "drop column bundles_slot.during",
  "create view live_bundles",
];

before(async () => {
  const admin = await connect(DATABASE_URL, { maxConnections: 1, default: false, registry: new Registry() });
  try {
    if (!(await admin.fetchText(`SELECT 1 FROM pg_database WHERE datname = '${PULL_DB}'`)).length) {
      await admin.execute(`CREATE DATABASE "${PULL_DB}"`);
    }
  } finally {
    await admin.close();
  }
});

const opened: Database[] = [];
after(async () => {
  for (const db of opened) await db.close();
});

test("pull, baseline and drift of a Django-style database", async () => {
  const db = await connect(PULL_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public;" + FIXTURE);

  const pulled = await pull(db);
  assert.ok(pulled.schema.includes('map: "bundles_bundle_shop_id_8f3e_fk_account_shop_id"'));
  // a trigger with REFERENCING transition tables reads back with them
  assert.ok(pulled.schema.includes("@@trigger(account_shop_updated_at, referencing(new: new_rows), when: after, operation: [update], level: statement"));
  assert.ok(!pulled.gaps.some((g) => g.includes("account_shop_updated_at")));
  assert.ok(pulled.gaps.some((g) => g.startsWith("rule bundles_bundle_soft_delete")));
  assert.deepEqual(pulled.differences.map((s) => s.summary), EXPECTED);

  const dir = mkdtempSync(join(tmpdir(), "monorm-pull-"));
  pulled.write(join(dir, "schema.prisma"));
  const migrator = new Migrator(db, new Migrations(join(dir, "migrations"), join(dir, "schema.prisma")));
  assert.equal((await migrator.baseline()).name, "0001_initial");
  assert.deepEqual(await migrator.upgrade(), []);
  const drift = await migrator.drift();
  assert.equal(drift.migration, "0001_initial");
  assert.deepEqual(drift.steps.map((s) => s.summary), EXPECTED);

  await db.execute("ALTER TABLE account_shop ADD COLUMN extra integer");
  assert.ok((await migrator.drift()).steps.some((s) => s.summary === "drop column account_shop.extra"));
  await assert.rejects(migrator.baseline(), MigrationError);
});

// case: pull::owned_serials_pull_as_identity_and_shared_sequences_stay_defaults
test("owned serials read as identity; a shared sequence stays a raw default", async () => {
  const db = await connect(PULL_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute(`DROP SCHEMA public CASCADE; CREATE SCHEMA public;
    CREATE TABLE s_empty (id serial PRIMARY KEY);
    CREATE TABLE s_full (id serial PRIMARY KEY); INSERT INTO s_full (id) SELECT g FROM generate_series(1, 5) g;
    CREATE SEQUENCE shared_seq;
    CREATE TABLE c_empty (id integer PRIMARY KEY DEFAULT nextval('shared_seq'));
    CREATE TABLE c_full (id integer PRIMARY KEY DEFAULT nextval('shared_seq'));
    INSERT INTO c_full (id) SELECT g FROM generate_series(1, 5) g;`);
  const pulled = await pull(db);
  assert.ok(pulled.gaps.some((g) => g.startsWith("column s_empty.id uses a sequence")));
  assert.ok(pulled.gaps.some((g) => g.startsWith("column s_full.id uses a sequence")));
  assert.equal(pulled.schema.split("@default(autoincrement())").length - 1, 2);
  assert.ok(pulled.gaps.some((g) => g.startsWith("column c_empty.id defaults to nextval('shared_seq'::regclass), a sequence the column does not own")));
  assert.ok(pulled.schema.includes(`@default(dbgenerated("nextval('shared_seq'::regclass)"))`));
  assert.deepEqual(pulled.differences, []);
  const dir = mkdtempSync(join(tmpdir(), "monorm-serials-"));
  pulled.write(join(dir, "schema.prisma"));
  const migrator = new Migrator(db, new Migrations(join(dir, "migrations"), join(dir, "schema.prisma")));
  await migrator.baseline();
  assert.deepEqual((await migrator.drift()).steps, []);
  const rows = await db.fetchText(
    "SELECT attrelid::regclass::text, attidentity::text, pg_get_expr(d.adbin, d.adrelid) FROM pg_attribute a " +
      "JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum " +
      "WHERE attrelid IN ('c_empty'::regclass, 'c_full'::regclass) ORDER BY 1",
  );
  assert.deepEqual(rows, [
    ["c_empty", "", "nextval('shared_seq'::regclass)"],
    ["c_full", "", "nextval('shared_seq'::regclass)"],
  ]);
});

const START_DIR = process.cwd();
afterEach(() => process.chdir(START_DIR));

/** A pulled schema names no dialect: the `[database]` table of the pull goes in the settings file. */
function usePulledSettings(pulled: { readonly database: string }, dir: string): void {
  writeFileSync(join(dir, "monorm.toml"), pulled.database);
  process.chdir(dir);
}

async function assertGeneratedPullIsClean(db: Database, dir: string, wanted: string): Promise<void> {
  const pulled = await pull(db);
  usePulledSettings(pulled, dir);
  assert.ok(pulled.schema.includes(wanted), pulled.schema);
  assert.ok(!pulled.gaps.some((g) => g.includes("generated")));
  assert.deepEqual(pulled.differences, []);
  pulled.write(join(dir, "schema.prisma"));
  const migrator = new Migrator(db, new Migrations(join(dir, "migrations"), join(dir, "schema.prisma")));
  await migrator.baseline();
  assert.deepEqual((await migrator.drift()).steps, []);
}

test("generated columns pull back without drift", async () => {
  const db = await connect(PULL_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute(`DROP SCHEMA public CASCADE; CREATE SCHEMA public;
    CREATE TABLE gen_lines (id bigint PRIMARY KEY, quantity integer NOT NULL, price integer NOT NULL,
      total integer GENERATED ALWAYS AS (quantity * price) STORED)`);
  await assertGeneratedPullIsClean(db, mkdtempSync(join(tmpdir(), "monorm-generated-")), "@generated(((quantity * price)))");
});

test("a virtual generated column pulls back without drift on postgres", async (t) => {
  const db = await connect(PULL_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  if (Number((await db.fetchText("SHOW server_version_num"))[0]![0]) < 180000) return t.skip("VIRTUAL generated columns need PostgreSQL 18");
  await db.execute(`DROP SCHEMA public CASCADE; CREATE SCHEMA public;
    CREATE TABLE gen_lines (id bigint PRIMARY KEY, quantity integer NOT NULL, price integer NOT NULL,
      total integer GENERATED ALWAYS AS (quantity * price) VIRTUAL)`);
  await assertGeneratedPullIsClean(db, mkdtempSync(join(tmpdir(), "monorm-generated-")), "@generated(((quantity * price)), stored: false)");
});

test("sqlite generated columns pull back without drift", async () => {
  for (const kind of ["STORED", "VIRTUAL"]) {
    const dir = mkdtempSync(join(tmpdir(), "monorm-generated-sqlite-"));
    process.chdir(dir);
    const registry = new Registry();
    loads('datasource db { provider = "sqlite" }\nmodel Keep { id Int @id }', { registry });
    const db = await connect(`sqlite://${join(dir, "live.db")}`, { default: false, registry });
    opened.push(db);
    await db.execute(`CREATE TABLE gen_lines (id INTEGER PRIMARY KEY, quantity INTEGER NOT NULL, price INTEGER NOT NULL,
      total INTEGER GENERATED ALWAYS AS (quantity * price) ${kind})`);
    await assertGeneratedPullIsClean(db, dir, kind === "STORED" ? "@generated((quantity * price))" : "@generated((quantity * price), stored: false)");
  }
});

test("sqlite triggers pull back as trigger attributes without drift", async () => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-triggers-sqlite-"));
  const registry = new Registry();
  loads('datasource db { provider = "sqlite" }\nmodel Keep { id Int @id }', { registry });
  const db = await connect(`sqlite://${join(dir, "live.db")}`, { default: false, registry });
  opened.push(db);
  await db.execute("CREATE TABLE tr_posts (id INTEGER PRIMARY KEY, title TEXT NOT NULL, touched INTEGER NOT NULL DEFAULT 0)");
  await db.execute(
    'CREATE TRIGGER "tr_posts_touch_trigger" AFTER UPDATE OF "title" ON "tr_posts" FOR EACH ROW WHEN (old.title <> new.title) BEGIN\n' +
      'UPDATE "tr_posts" SET "touched" = "touched" + 1 WHERE "id" = NEW."id";\nEND',
  );
  // a trigger in another form is a gap, not an attribute
  await db.execute("CREATE TRIGGER other AFTER INSERT ON tr_posts BEGIN SELECT 1; END");
  const pulled = await pull(db);
  assert.ok(pulled.schema.includes('@@trigger(touch, when: after, operation: [update_of(title)], condition: "old.title <> new.title", func: '), pulled.schema);
  assert.ok(!pulled.schema.includes("@@trigger(other"));
  assert.ok(pulled.gaps.some((g) => g.startsWith("trigger other on tr_posts")));
  assert.ok(!pulled.gaps.some((g) => g.startsWith("trigger tr_posts_touch_trigger")));
  assert.deepEqual(pulled.differences, []);
});

test("sql views pull back without drift", async () => {
  const db = await connect(PULL_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute(`DROP SCHEMA public CASCADE; CREATE SCHEMA public;
    CREATE TABLE view_orders (id bigint PRIMARY KEY, shop text NOT NULL, total integer NOT NULL);
    CREATE VIEW view_big AS SELECT id, shop, total FROM view_orders WHERE total > 100;
    CREATE MATERIALIZED VIEW view_by_shop AS SELECT shop AS id, sum(total) AS spent FROM view_big GROUP BY shop;
    CREATE UNIQUE INDEX view_by_shop_id ON view_by_shop (id)`);
  const dir = mkdtempSync(join(tmpdir(), "monorm-views-"));
  const pulled = await pull(db);
  assert.ok(pulled.schema.includes("view ViewBig {") && pulled.schema.includes("@@materialized"));
  assert.ok(pulled.schema.includes("@@depends_on([ViewBig])"));
  assert.ok(!pulled.gaps.some((g) => g.includes("view")), pulled.gaps.join("\n"));
  assert.deepEqual(pulled.differences, []);
  const schema = join(dir, "schema.prisma");
  pulled.write(schema);
  assert.ok(readFileSync(join(dir, "views", "public", "ViewBig.sql"), "utf8").includes("total > 100"));
  const migrator = new Migrator(db, new Migrations(join(dir, "migrations"), schema));
  await migrator.baseline();
  assert.deepEqual((await migrator.drift()).steps, []);
});

test("rollback refuses a baselined migration unless forced", async () => {
  const db = await connect(PULL_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public; CREATE TABLE keep (id bigint PRIMARY KEY); INSERT INTO keep VALUES (1)");
  const dir = mkdtempSync(join(tmpdir(), "monorm-baseline-"));
  (await pull(db)).write(join(dir, "schema.prisma"));
  const migrator = new Migrator(db, new Migrations(join(dir, "migrations"), join(dir, "schema.prisma")));
  await migrator.baseline();
  await assert.rejects(migrator.downgrade(), /0001_initial was baselined/);
  assert.deepEqual(await db.fetchText("SELECT count(*) FROM keep"), [["1"]]);
  assert.deepEqual((await migrator.downgrade({ force: true, allowDestructive: true })).map((m) => m.name), ["0001_initial"]);
});

const SOFT_DELETE_RULES = `
-- the rules of django-guitars: the rule name is soft_delete on every table, the column is _deleted_at
CREATE TABLE sdm_t (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, name text NOT NULL, _deleted_at timestamptz);
-- guitars: a Django foreign key has no ON DELETE action; a rule cascades the soft delete
CREATE TABLE sdm_c (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, t_id integer NOT NULL REFERENCES sdm_t (id), _deleted_at timestamptz);
-- a foreign key with ON DELETE CASCADE: the monorm cascade trigger reaches this child
CREATE TABLE sdm_d (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, t_id integer NOT NULL REFERENCES sdm_t (id) ON DELETE CASCADE,
                    _deleted_at timestamptz);
CREATE TABLE sdm_o (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY);
CREATE OR REPLACE RULE soft_delete AS ON DELETE TO sdm_t
  WHERE COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD (UPDATE sdm_t SET _deleted_at = NOW() WHERE "id" = old."id" AND _deleted_at IS NULL);
CREATE OR REPLACE RULE soft_delete AS ON DELETE TO sdm_c
  WHERE COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD (UPDATE sdm_c SET _deleted_at = NOW() WHERE "id" = old."id" AND _deleted_at IS NULL);
CREATE OR REPLACE RULE soft_delete_related_sdm_c AS ON UPDATE TO "sdm_t"
  WHERE old._deleted_at IS NULL AND new._deleted_at IS NOT NULL AND
        COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO ALSO (UPDATE "sdm_c" SET _deleted_at = NOW() WHERE "t_id" = old."id");
-- the plain shape, with no guard
CREATE RULE sdm_d_soft_delete AS ON DELETE TO sdm_d
  DO INSTEAD UPDATE sdm_d SET _deleted_at = now() WHERE sdm_d.id = old.id;
CREATE RULE sdm_o_keep AS ON DELETE TO sdm_o DO INSTEAD NOTHING;
`;

async function scalar(db: Database, sql: string): Promise<string | null> {
  return (await db.fetchText(sql))[0]![0]!;
}

test("pull moves a soft delete rule to triggers", async () => {
  const db = await connect(PULL_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public;" + SOFT_DELETE_RULES);

  const pulled = await pull(db);
  assert.ok(pulled.schema.includes('@@soft_delete.deleted_at("deleted_at"'));
  assert.equal(pulled.schema.split('rule: "soft_delete"').length - 1, 2);
  assert.ok(pulled.schema.includes('rule: "sdm_d_soft_delete"'));
  // a rule of another shape stays a gap and is never dropped
  assert.ok(pulled.gaps.some((g) => g.includes("rule sdm_o_keep")));
  assert.ok(pulled.gaps.some((g) => g.includes("rule soft_delete_related_sdm_c")));
  assert.ok(!pulled.gaps.some((g) => g.includes("rule soft_delete ") || g.includes("rule sdm_d_soft_delete")));
  // the legacy database has no guard for a child under a deleted parent: the move adds it for the cascade child sdm_d
  assert.deepEqual(pulled.differences.map((s) => s.summary), ["create or replace function sdm_d_soft_delete_guard_t()", "create trigger soft_delete_guard_t on sdm_d"]);
  // a baseline records the schema as the database, so the guard goes in by hand
  for (const step of pulled.differences) await db.execute(step.sql);

  const dir = mkdtempSync(join(tmpdir(), "monorm-soft-delete-move-"));
  pulled.write(join(dir, "schema.prisma"));
  const migrations = new Migrations(join(dir, "migrations"), join(dir, "schema.prisma"));
  const migrator = new Migrator(db, migrations);
  const baselineName = (await migrator.baseline()).name;
  // the baseline keeps the database as it is: the rule still runs
  await db.execute("INSERT INTO sdm_t (name) VALUES ('a'); DELETE FROM sdm_t");
  assert.equal(await scalar(db, "SELECT count(*) FROM sdm_t WHERE _deleted_at IS NOT NULL"), "1");
  await db.execute("TRUNCATE sdm_t CASCADE");

  const move = migrations.make("move_soft_delete");
  const up = move!.upSql;
  assert.ok(up.indexOf('DROP RULE IF EXISTS "soft_delete" ON "sdm_t"') >= 0);
  assert.ok(up.indexOf('DROP RULE IF EXISTS "soft_delete" ON "sdm_t"') < up.indexOf("CREATE TRIGGER"));
  assert.ok(up.includes('DROP RULE IF EXISTS "soft_delete" ON "sdm_c"') && up.includes('DROP RULE IF EXISTS "sdm_d_soft_delete" ON "sdm_d"'));
  assert.ok(!up.includes("sdm_o_keep") && !up.includes("soft_delete_related"));
  assert.deepEqual((await migrator.upgrade()).map((m) => m.name), [move!.name]);

  assert.equal(
    await scalar(db, "SELECT string_agg(rulename, ',' ORDER BY rulename) FROM pg_rules WHERE schemaname = 'public'"),
    "sdm_o_keep,soft_delete_related_sdm_c",
  );
  await db.execute(
    "INSERT INTO sdm_t (name) VALUES ('a'); INSERT INTO sdm_c (t_id) SELECT id FROM sdm_t;" +
      " INSERT INTO sdm_d (t_id) SELECT id FROM sdm_t",
  );
  await db.execute("DELETE FROM sdm_t");
  assert.equal(await scalar(db, "SELECT count(*) FROM sdm_t WHERE _deleted_at IS NOT NULL"), "1");
  assert.equal(await scalar(db, "SELECT count(*) FROM sdm_c WHERE _deleted_at IS NOT NULL"), "1");
  // no guitars rule reaches sdm_d: only the monorm cascade trigger soft-deletes it
  assert.equal(await scalar(db, "SELECT count(*) FROM sdm_d WHERE _deleted_at IS NOT NULL"), "1");

  const drift = await migrator.drift();
  assert.equal(drift.steps.length, 0);
  assert.deepEqual(
    drift.gaps.filter((g) => g.startsWith("rule ")).map((g) => g.split(" ")[1]).sort(),
    ["sdm_o_keep", "soft_delete_related_sdm_c"],
  );
  // with only recognized rules left, a new pull reproduces the database: no step and no gap line, as `pull` prints
  // "The schema reproduces the database." only for that
  await db.execute("DROP RULE sdm_o_keep ON sdm_o; DROP RULE soft_delete_related_sdm_c ON sdm_t");
  const again = await pull(db);
  assert.equal(again.differences.length, 0);
  assert.deepEqual(again.gaps, []);
  // the move is undone without the rule: the down migration warns and does not bring it back
  assert.deepEqual((await migrator.downgrade()).map((m) => m.name), [move!.name]);
  assert.equal(await scalar(db, "SELECT count(*) FROM pg_rules WHERE rulename = 'soft_delete'"), "0");
  // a down and an up again, and a new database, run the move: there is no rule to drop
  assert.deepEqual((await migrator.upgrade()).map((m) => m.name), [move!.name]);
  await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public");
  assert.deepEqual((await migrator.upgrade()).map((m) => m.name), [baselineName, move!.name]);
  await db.execute("INSERT INTO sdm_t (name) VALUES ('b'); DELETE FROM sdm_t");
  assert.equal(await scalar(db, "SELECT count(*) FROM sdm_t WHERE _deleted_at IS NOT NULL"), "1");
});

test("pull keeps a soft delete rule as a gap when its field name is taken", async () => {
  const db = await connect(PULL_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute(
    `DROP SCHEMA public CASCADE; CREATE SCHEMA public;
CREATE TABLE sdm_x (id serial PRIMARY KEY, deleted_at timestamptz, _deleted_at timestamptz);
CREATE RULE soft_delete AS ON DELETE TO sdm_x
  WHERE COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD (UPDATE sdm_x SET _deleted_at = NOW() WHERE "id" = old."id" AND _deleted_at IS NULL);`,
  );
  const pulled = await pull(db);
  assert.ok(!pulled.schema.includes("@@soft_delete"));
  assert.ok(pulled.gaps.some((g) => g.startsWith("rule soft_delete on sdm_x")));
  assert.ok(pulled.gaps.some((g) => g.startsWith("rule claims skipped")));
});

test("pull keeps a soft delete rule as a gap when a cascade loses its child", async () => {
  const db = await connect(PULL_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute(
    `DROP SCHEMA public CASCADE; CREATE SCHEMA public;
CREATE TABLE sdm_p (id serial PRIMARY KEY, _deleted_at timestamptz);
CREATE TABLE sdm_k (id serial PRIMARY KEY, p_id integer NOT NULL REFERENCES sdm_p (id) ON DELETE CASCADE);
CREATE RULE soft_delete AS ON DELETE TO sdm_p
  WHERE COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD (UPDATE sdm_p SET _deleted_at = NOW() WHERE "id" = old."id" AND _deleted_at IS NULL);`,
  );
  const pulled = await pull(db);
  assert.ok(!pulled.schema.includes("@@soft_delete"));
  assert.ok(pulled.gaps.some((g) => g.startsWith("rule soft_delete on sdm_p")));
  assert.ok(pulled.gaps.some((g) => g.startsWith("rule claims skipped") && g.includes("SdmK")));
});

test("pull keeps a disabled soft delete rule as a gap", async () => {
  const db = await connect(PULL_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute(
    `DROP SCHEMA public CASCADE; CREATE SCHEMA public;
CREATE TABLE sdm_q (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, _deleted_at timestamptz);
CREATE RULE soft_delete AS ON DELETE TO sdm_q
  WHERE COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD (UPDATE sdm_q SET _deleted_at = NOW() WHERE "id" = old."id" AND _deleted_at IS NULL);
ALTER TABLE sdm_q DISABLE RULE soft_delete;`,
  );
  const pulled = await pull(db);
  assert.ok(!pulled.schema.includes("@@soft_delete"));
  assert.ok(pulled.gaps.some((g) => g.startsWith("rule soft_delete on sdm_q") && g.includes("the rule is disabled")));
});

test("SQLite pull, baseline and drift", async () => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-pull-sqlite-"));
  const source = `
datasource db {
  provider = "sqlite"
}

model Owner {
  id    BigInt @id @default(autoincrement())
  email String @unique
  pets  Pet[]
}

model Pet {
  id       BigInt @id @default(autoincrement())
  owner_id BigInt
  age      Int    @default(0)
  owner    Owner  @relation(fields: [owner_id], references: [id], onDelete: Cascade)

  @@index([owner_id, -age])
  @@check("age >= 0", name: "pet_age")
}
`;
  writeFileSync(join(dir, "schema.prisma"), source);
  const registry = new Registry();
  loads(source, { registry });
  const db = await connect(`sqlite://${join(dir, "live.db")}`, { default: false, registry });
  opened.push(db);
  const made = new Migrations(join(dir, "made"), join(dir, "schema.prisma"));
  made.make();
  await new Migrator(db, made).upgrade();

  const pulled = await pull(db);
  assert.deepEqual(pulled.differences, []);
  assert.ok(pulled.database.includes('provider = "sqlite"') && !pulled.schema.includes("datasource"));
  assert.ok(pulled.schema.includes('@@check((age >= 0), name: "pet_age")'));
  pulled.write(join(dir, "pulled.prisma"));
  usePulledSettings(pulled, dir);
  await db.execute("DROP TABLE monorm_migrations");
  const migrator = new Migrator(db, new Migrations(join(dir, "migrations"), join(dir, "pulled.prisma")));
  await migrator.baseline();
  assert.deepEqual((await migrator.drift()).steps, []);
  await db.execute("CREATE INDEX extra_idx ON pet (age)");
  assert.deepEqual((await migrator.drift()).steps.map((s) => s.summary), ["drop index extra_idx"]);
});

const EXTENSION_MODELS = `
model Post {
 id Int @id @default(autoincrement())
 title String
 @@history.track
 @@timestamps.updated_at("touched_at", mode: "database")
 @@map("pe_posts")
}
model Comment {
 id Int @id @default(autoincrement())
 body String
 @@history.track(model: "CommentAudit")
 @@map("pe_comments")
}
model BundleVersion {
 id Int @id @default(autoincrement())
 bundle_id Int
 @@history.versions(of: bundle_id)
 @@map("pe_bundle_versions")
}
`;

for (const provider of ["sqlite", "postgresql"] as const) {
  test(`history and timestamps pull back as attributes without drift (${provider})`, { skip: !hasExtension("monorm.history", "monorm.timestamps") }, async () => {
    const registry = new Registry();
    loads(`datasource db { provider = "${provider}" }\n` + EXTENSION_MODELS, { registry });
    const url = provider === "sqlite" ? `sqlite://${join(mkdtempSync(join(tmpdir(), "monorm-ext-sqlite-")), "live.db")}` : PULL_URL;
    const db = await connect(url, { maxConnections: 2, default: false, registry });
    opened.push(db);
    if (provider === "postgresql") await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public");
    await db.createTables();
    const pulled = await pull(db);
    const text = pulled.schema;
    assert.ok(text.includes("@@history.track"));
    assert.ok(text.includes('@@history.track(model: "CommentAudit")'), text);
    assert.ok(text.includes("@@history.versions(of: bundle_id)"), text);
    assert.ok(text.includes('@@timestamps.updated_at("touched_at", mode: "database")'), text);
    // what the extensions generate is not a model, a trigger or a function of its own
    for (const generated of ["pe_posts_events", "comment_audit", "h_events_", "h_comment_audit_", "updated_at_touched_at", "version_number", "@@trigger("]) {
      assert.ok(!text.includes(generated), generated);
    }
    assert.deepEqual(pulled.differences.map((s) => s.summary), []);
  });
}

/** Creates the tables of `models` in an empty database and pulls them back. */
async function pulledFrom(provider: "sqlite" | "postgresql", models: string) {
  const registry = new Registry();
  loads(`datasource db { provider = "${provider}" }\n${models}`, { registry });
  const url = provider === "sqlite" ? `sqlite://${join(mkdtempSync(join(tmpdir(), "monorm-pulled-sqlite-")), "live.db")}` : PULL_URL;
  const db = await connect(url, { maxConnections: 2, default: false, registry });
  opened.push(db);
  if (provider === "postgresql") await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public");
  await db.createTables();
  return pull(db);
}

// a tracker argument that the database cannot show, and whether pull writes a gap line for the skipped claim;
// a partial events list leaves no claim to skip, because the claim needs all three triggers
const UNSHOWN_TRACKER_ARGUMENTS: [string, boolean][] = [
  ['fields: [title], label: "t"', true],
  ["row: old", true],
  ['when: "NEW.title <> OLD.title"', true],
  ["exclude: [body]", true],
  ["events: [insert, update]", false],
];

for (const provider of ["sqlite", "postgresql"] as const) {
  for (const [args, gap] of UNSHOWN_TRACKER_ARGUMENTS) {
    test(`a tracker with arguments the database cannot show pulls back as plain objects (${provider}, ${args})`, { skip: !hasExtension("monorm.history") }, async () => {
      const pulled = await pulledFrom(provider, `model Post {\n id Int @id @default(autoincrement())\n title String\n body String\n @@history.track(${args})\n @@map("pf_posts")\n}`);
      assert.ok(!pulled.schema.includes("@@history.track"), pulled.schema);
      assert.equal(pulled.gaps.some((g) => g.startsWith("object claims skipped, the schema with them differs from the database")), gap);
      assert.deepEqual(pulled.differences.map((s) => s.summary), []);
    });
  }

  test(`two trackers of one table pull back as two attributes (${provider})`, { skip: !hasExtension("monorm.history") }, async () => {
    const pulled = await pulledFrom(provider, 'model Post {\n id Int @id @default(autoincrement())\n title String\n @@history.track\n @@history.track(model: "PostAudit")\n @@map("p2_posts")\n}');
    const text = pulled.schema;
    assert.equal(text.split("@@history.track(").length - 1, 2, text);
    assert.ok(text.includes("@@history.track()"), text);
    assert.ok(text.includes('@@history.track(model: "PostAudit")'), text);
    for (const generated of ["p2_posts_events", "post_audit", "@@trigger("]) assert.ok(!text.includes(generated), generated);
    assert.deepEqual(pulled.differences.map((s) => s.summary), []);
  });

  test(`a timestamps column with another field name pulls back with map (${provider})`, { skip: !hasExtension("monorm.timestamps") }, async () => {
    const pulled = await pulledFrom(provider, 'model Post {\n id Int @id @default(autoincrement())\n title String\n @@timestamps.updated_at("touched_at", map: "_touched_at", mode: "database")\n @@map("pm_posts")\n}');
    const text = pulled.schema;
    assert.ok(text.includes('@@timestamps.updated_at("touched_at", map: "_touched_at", mode: "database")'), text);
    for (const generated of ["updated_at__touched_at", "@@trigger("]) assert.ok(!text.includes(generated), generated);
    assert.deepEqual(pulled.differences.map((s) => s.summary), []);
  });
}

test("a version model with scope pulls back with the scope column as a field", { skip: !hasExtension("monorm.history") }, async () => {
  const pulled = await pulledFrom("postgresql", 'model BundleVersion {\n id Int @id @default(autoincrement())\n bundle_id Int\n @@history.versions(of: bundle_id, scope: [user])\n @@map("ps_bundle_versions")\n}');
  // the scope column sits in the version table, so the database shows it as a plain column and no difference is left to see
  assert.ok(pulled.schema.includes("@@history.versions(of: bundle_id)") && !pulled.schema.includes("scope:"), pulled.schema);
  assert.ok(pulled.schema.includes("user String?"), pulled.schema);
  assert.deepEqual(pulled.gaps, []);
  assert.deepEqual(pulled.differences.map((s) => s.summary), []);
});

test("a failing rule claim is named when object claims were tried with it", { skip: !hasExtension("monorm.history", "monorm.soft-delete") }, async () => {
  const registry = new Registry();
  loads('datasource db { provider = "postgresql" }\nmodel Post {\n id Int @id @default(autoincrement())\n title String\n @@history.track\n @@map("pl_posts")\n}', { registry });
  const db = await connect(PULL_URL, { maxConnections: 2, default: false, registry });
  opened.push(db);
  await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public");
  await db.createTables();
  // the rule claim fails to compile: its field name `_deleted_at` is taken
  await db.execute(
    `CREATE TABLE sdm_x (id serial PRIMARY KEY, deleted_at timestamptz, _deleted_at timestamptz);
CREATE RULE soft_delete AS ON DELETE TO sdm_x
  WHERE COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD (UPDATE sdm_x SET _deleted_at = NOW() WHERE "id" = old."id" AND _deleted_at IS NULL);`,
  );
  const pulled = await pull(db);
  // the first attempt holds both kinds of claim; the gap names both, and the rule claims are tried again alone
  assert.ok(pulled.gaps.some((g) => g.startsWith("object claims skipped, the schema with the rule and object claims does not compile")));
  assert.ok(pulled.gaps.some((g) => g.startsWith("rule claims skipped, the schema with them does not compile")));
  assert.ok(!pulled.schema.includes("@@history.track"), pulled.schema);
  assert.deepEqual(pulled.differences.map((s) => s.summary), []);
});

const DJANGO_SQLITE = `
CREATE TABLE "auth_user" ("id" integer NOT NULL PRIMARY KEY AUTOINCREMENT, "is_staff" bool NOT NULL,
    "joined" datetime NOT NULL, "age" integer unsigned NOT NULL CHECK ("age" >= 0), "price" decimal NOT NULL,
    CHECK (price > 0));
CREATE TABLE "app_item" ("id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
    "user_id" integer NOT NULL REFERENCES "auth_user" ("id") DEFERRABLE INITIALLY DEFERRED,
    "owner_id" integer NULL REFERENCES auth_user);
INSERT INTO auth_user (is_staff, joined, age, price) VALUES (1, '2024-01-02 03:04:05', 3, 1.5);
`;

test("sqlite pull reads column clauses", async () => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-pull-django-"));
  const url = `sqlite://${join(dir, "django.db")}`;
  const bare = new Registry();
  loads('datasource db { provider = "sqlite" }\nmodel Keep { id Int @id }', { registry: bare });
  let db = await connect(url, { default: false, registry: bare });
  let text: string;
  try {
    await db.execute(DJANGO_SQLITE);
    const pulled = await pull(db);
    text = pulled.schema;
    usePulledSettings(pulled, dir);
  } finally {
    await db.close();
  }
  assert.ok(text.includes('@@check((age >= 0), name: "auth_user_age_check")'), text);
  assert.ok(text.includes('@@check((price > 0), name: "auth_user_check")'));
  assert.ok(text.includes("is_staff Boolean") && text.includes("joined DateTime") && text.includes("price Float"));
  assert.ok(text.includes('user AuthUser @relation("AppItem_user", fields: [user_id], references: [id], deferrable: deferred)'));
  assert.ok(text.includes('owner AuthUser? @relation("AppItem_owner", fields: [owner_id], references: [id])'));

  // the pulled models read the rows another tool wrote
  const registry = new Registry();
  const User = loads(text, { registry })["AuthUser"] as any;
  db = await connect(url, { default: false, registry });
  try {
    const user = await User.objects.using(db).get();
    assert.equal(user.isStaff, true);
    assert.equal(user.price, 1.5);
  } finally {
    await db.close();
  }
});

test("services share unmanaged models", async () => {
  for (const dialect of ["postgresql", "sqlite"]) {
    const dir = mkdtempSync(join(tmpdir(), "monorm-unmanaged-"));
    const postgres = dialect === "postgresql";
    const url = postgres ? PULL_URL : `sqlite://${join(dir, "shared.db")}`;
    const datasource = `datasource db { provider = "${dialect}" }\n`;
    writeFileSync(join(dir, "shared.prisma"), "model Account {\n id Int @id\n label String\n}");
    const owner = join(dir, "owner.prisma");
    writeFileSync(owner, datasource + 'import "shared.prisma" (prefix: "shared_")\n');
    const consumer = join(dir, "consumer.prisma");
    const consumerSource = datasource + `import "shared.prisma" (prefix: "shared_", managed: false)
model Task {
  id Int @id
  account_id Int
  account Account @relation(fields: [account_id], references: [id])
}
`;
    writeFileSync(consumer, consumerSource);
    const ownerMigrations = new Migrations(join(dir, "owner"), owner);
    const initial = ownerMigrations.make();
    const registry = new Registry();
    const Account = load(consumer, { registry })["Account"]!;
    const db = await connect(url, { maxConnections: 1, default: false, registry });
    try {
      if (postgres) {
        await db.execute("DROP SCHEMA IF EXISTS managed_owner CASCADE; DROP SCHEMA IF EXISTS managed_consumer CASCADE; " +
          "CREATE SCHEMA managed_owner; CREATE SCHEMA managed_consumer; SET search_path TO managed_owner");
        await new Migrator(db, ownerMigrations).upgrade();
        await db.execute("SET search_path TO managed_consumer, managed_owner");
      } else {
        // SQLite has one namespace. Apply the owner's generated DDL before starting the consumer ledger.
        await db.execute(initial!.upSql);
      }
      await db.execute("CREATE INDEX outside_index ON shared_account (label)");
      const consumerMigrations = new Migrations(join(dir, "consumer"), consumer);
      const migration = consumerMigrations.make();
      assert.ok(migration!.upSql.includes('REFERENCES "shared_account"'));
      assert.ok(!migration!.upSql.includes('CREATE TABLE "shared_account"'));
      await new Migrator(db, consumerMigrations).upgrade();

      const accounts = Account.objects.using(db);
      await accounts.insert({ id: 1, label: "shared" });
      assert.equal(await accounts.all().update({ label: "updated" }), 1);
      await db.execute("INSERT INTO task (id, account_id) VALUES (1, 1)");
      await assert.rejects(() => db.execute("INSERT INTO task (id, account_id) VALUES (2, 99)"));
      writeFileSync(consumer, consumerSource.replace("  id Int @id", "  id Int @id\n  note String?"));
      consumerMigrations.make();
      const migrator = new Migrator(db, consumerMigrations);
      await migrator.upgrade();
      const drift = await migrator.drift();
      assert.deepEqual(drift.steps, []);
      assert.deepEqual(drift.gaps, []);
      const indexQuery = postgres
        ? "SELECT indexname FROM pg_indexes WHERE schemaname = 'managed_owner' AND indexname = 'outside_index'"
        : "SELECT name FROM sqlite_schema WHERE name = 'outside_index'";
      assert.deepEqual(await db.fetchText(indexQuery), [["outside_index"]]);
      const account = await accounts.get();
      assert.ok(account && "label" in account);
      assert.equal(account.label, "updated");
    } finally {
      await db.close();
    }
  }
});
