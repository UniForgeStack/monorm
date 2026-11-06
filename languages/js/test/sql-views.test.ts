/** A SQL view model reads, refuses every write with `ReadOnlyModel` before SQL, and a materialized view refreshes. */
import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { connect, load, Migrations, Migrator, ReadOnlyModel, QueryError, Registry, RelatedSet, loads, pull } from "../src/index.js";
import { native } from "../src/native.js";
import { scratch } from "./scratch.js";

const url = process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
const bodies: Record<string, string> = {
  ActiveUser: 'SELECT id, name FROM "sv_node_users" WHERE active',
  UserNames: 'SELECT id, name FROM "sv_node_users"',
};
type Dialect = "sqlite" | "postgres";
// Models are open records here: the views have no write types, so the writes below are cast.
type Open = { objects: any; id: any } & Record<string, any>;

function schemaFile(dialect: Dialect): string {
  const dir = scratch("monorm-sql-views-");
  let text = dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "";
  text += `
model User {
  id     Int @id
  name   String
  active Boolean
  @@map("sv_node_users")
}
view ActiveUser {
  id   Int @id
  name String
  @@map("sv_node_active")
}
`;
  if (dialect === "postgres") {
    text += `
view UserNames {
  id   Int @id
  name String
  @@materialized
  @@index([id], unique: true)
  @@map("sv_node_names")
}
`;
  }
  const path = join(dir, "schema.prisma");
  writeFileSync(path, text);
  const sidecars = join(dir, "views", dialect === "sqlite" ? "main" : "public");
  mkdirSync(sidecars, { recursive: true });
  for (const [name, body] of Object.entries(bodies)) writeFileSync(join(sidecars, `${name}.sql`), body);
  return path;
}

/** A database of its own: drift reads the whole database, and the shared one holds other tests' tables. */
async function privateUrl(): Promise<string> {
  if (process.env.ORM_SQL_VIEWS_DATABASE_URL) return process.env.ORM_SQL_VIEWS_DATABASE_URL;
  const name = `${url.split("/").pop()}_views`;
  const admin = await connect(url, { default: false, maxConnections: 1, registry: new Registry() });
  try {
    if ((await admin.fetchText(`SELECT 1 FROM pg_database WHERE datname = '${name}'`)).length === 0) {
      await admin.execute(`CREATE DATABASE "${name}"`);
    }
  } finally {
    await admin.close();
  }
  return url.replace(/\/[^/]*$/, `/${name}`);
}

async function open(dialect: Dialect, at = url) {
  const registry = new Registry();
  const path = schemaFile(dialect);
  const models = load(path, { registry }) as unknown as Record<string, Open>;
  const db = await connect(dialect === "sqlite" ? "sqlite://:memory:" : at, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  await models.User!.objects.using(db).insertMany([{ id: 1, name: "ada", active: true }, { id: 2, name: "bob", active: false }]);
  return { models, db, path };
}

for (const dialect of ["sqlite", "postgres"] as const) {
  test(`a view model reads and every write is refused before sql (${dialect})`, async () => {
    const { models, db } = await open(dialect);
    const ActiveUser = models.ActiveUser!;
    try {
      const rows = await ActiveUser.objects.using(db).orderBy(ActiveUser.id).all();
      assert.deepEqual(rows.map((r: { id: number; name: string }) => [r.id, r.name]), [[1, "ada"]]);
      const row = rows[0];
      const objects = ActiveUser.objects.using(db);
      const wanted = { name: "ReadOnlyModel", message: /ActiveUser is a view/ };
      const writes: (() => unknown)[] = [
        () => objects.insert({ id: 9, name: "x" }),
        () => objects.insertMany([{ id: 9, name: "x" }]),
        () => objects.insertMany([{ id: 9, name: "x" }], { copy: true }),
        () => objects.getOrInsert({ id: 9, name: "x" }),
        () => objects.insert({ id: 9, name: "x" }).onConflict(ActiveUser.id, { update: true }),
        () => objects.filter(ActiveUser.id.eq(1)).update({ name: "x" }),
        () => objects.updateMany([{ id: 1, name: "x" }]),
        () => objects.filter(ActiveUser.id.eq(1)).delete(),
        () => objects.all().delete(),
        () => row.update({ name: "x" }),
        () => row.delete(),
      ];
      for (const write of writes) {
        await assert.rejects(async () => write(), (e: unknown) => e instanceof ReadOnlyModel && wanted.message.test((e as Error).message));
      }
      // PostgreSQL would take a write to a simple view; the base table shows that none arrived.
      const base = await models.User!.objects.using(db).all();
      assert.deepEqual(base.map((r: { id: number; name: string }) => [r.id, r.name]).sort(), [[1, "ada"], [2, "bob"]]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`refresh refuses a plain view and a non view (${dialect})`, async () => {
    const { models, db } = await open(dialect);
    try {
      for (const model of [models.ActiveUser!, models.User!]) {
        await assert.rejects(async () => db.refresh(model as never), (e: unknown) =>
          e instanceof QueryError && /only a materialized view can be refreshed|no materialized views/.test((e as Error).message));
      }
    } finally { await db.dropTables(); await db.close(); }
  });
}

test("refresh reloads a materialized view with and without concurrently", async () => {
  const { models, db } = await open("postgres");
  const UserNames = models.UserNames!, User = models.User!;
  try {
    assert.equal(await UserNames.objects.using(db).count(), 0);
    await db.refresh(UserNames as never);
    const names = await UserNames.objects.using(db).all();
    assert.deepEqual(names.map((r: { name: string }) => r.name).sort(), ["ada", "bob"]);
    await User.objects.using(db).insert({ id: 3, name: "cy", active: true });
    assert.equal(await UserNames.objects.using(db).count(), 2);
    await db.refresh(UserNames as never, { concurrently: true });
    assert.equal(await UserNames.objects.using(db).count(), 3);
  } finally { await db.dropTables(); await db.close(); }
});

test("the generated module has no write types for a view", () => {
  const text = native().generateTypescript(schemaFile("postgres"));
  for (const shape of ["ActiveUserInsert", "ActiveUserUpdate", "ActiveUserUpdateRow"]) assert.ok(!text.includes(shape), shape);
  assert.ok(text.includes("UserInsert"));
  const spec = /export interface ActiveUserSpec \{[^]*?\n}/.exec(text)?.[0];
  assert.ok(spec);
  for (const slot of ["insert", "update", "updateRow"]) assert.ok(new RegExp(`readonly ${slot}: never;`).test(spec), slot);
});

test("createTables runs twice and a hand written body has no drift", async () => {
  const empty = await emptyDatabase("postgres");
  await empty.close();
  const { models, db, path } = await open("postgres", await privateUrl());
  try {
    await db.createTables();
    assert.equal(await models.ActiveUser!.objects.using(db).count(), 1);
    const migrator = new Migrator(db, new Migrations(join(path, "..", "migrations"), path));
    await migrator.baseline();
    assert.deepEqual((await migrator.drift()).steps, []);
  } finally {
    await db.execute("DROP TABLE IF EXISTS monorm_migrations");
    await db.dropTables();
    await db.close();
  }
});

for (const dialect of ["sqlite", "postgres"] as const) {
  test(`related set insert into a view is refused before sql (${dialect})`, async () => {
    const { models, db } = await open(dialect);
    try {
      const owner = await models.User!.objects.using(db).get(models.User!.id.eq(1));
      // Back relation declarations to views are forbidden in v1. Construct the
      // runtime related set directly to exercise its independent insert path.
      const related = new RelatedSet(models.ActiveUser!._meta);
      Object.assign(related, {
        relation: { name: "activeRows", ir: "active_rows", kind: "many", target: "ActiveUser", from: ["id"], to: ["id"], through: undefined },
        instance: owner,
      });
      const bound = related.using(db);
      const events: unknown[] = [];
      const remove = db.onQuery((event) => events.push(event));
      try {
        await assert.rejects(async () => bound.insert({ name: "changed" }), (error: unknown) =>
          error instanceof ReadOnlyModel && /ActiveUser is a view/.test(error.message));
        assert.deepEqual(events, []);
      } finally { remove(); }
    } finally { await db.dropTables(); await db.close(); }
  });
}

async function emptyDatabase(dialect: Dialect, schema?: string) {
  const registry = new Registry();
  if (schema !== undefined) load(schema, { registry });
  else if (dialect === "sqlite") loads('datasource db { provider = "sqlite" }', { registry });
  const db = await connect(dialect === "sqlite" ? "sqlite://:memory:" : await privateUrl(),
    { registry, default: false, maxConnections: 1 });
  if (dialect === "postgres") await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public");
  return db;
}

function migrationSchema(dir: string, dialect: Dialect, field = "Int", body?: string, extra = "") {
  const path = join(dir, "schema.prisma");
  writeFileSync(path, (dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + `
model ReviewItem {
  id Int @id
  value ${field}
  ${extra}
  @@map("sv_review_items")
}
view ReviewView {
  id Int @id
  value ${body === undefined ? field : "String"}
  @@map("sv_review_view")
}
`);
  const sidecars = join(dir, "views", dialect === "sqlite" ? "main" : "public");
  mkdirSync(sidecars, { recursive: true });
  writeFileSync(join(sidecars, "ReviewView.sql"), body ?? "SELECT id, value FROM sv_review_items");
  return path;
}

for (const change of ["body", "column_type", "table"] as const) {
  test(`postgres recreates a view and runs the migration (${change})`, async () => {
    const dir = scratch("monorm-view-migrations-");
    const schema = migrationSchema(dir, "postgres");
    const db = await emptyDatabase("postgres", schema);
    try {
      const migrations = new Migrations(join(dir, "migrations"), schema);
      migrations.make();
      const migrator = new Migrator(db, migrations);
      await migrator.upgrade();
      const models = load(schema, { registry: new Registry() }) as unknown as Record<string, Open>;
      await models.ReviewItem!.objects.using(db).insertMany([{ id: 1, value: 7 }, { id: 2, value: 9 }]);
      if (change === "body") migrationSchema(dir, "postgres", "Int", "SELECT id, value::text AS value FROM sv_review_items");
      else if (change === "column_type") migrationSchema(dir, "postgres", "BigInt");
      else migrationSchema(dir, "postgres", "Int", undefined, "added Int?");
      const migration = migrations.make(change)[0]!;
      const sql = migration.upSql;
      assert.ok(!sql.includes("OR REPLACE"));
      assert.ok(sql.indexOf("DROP VIEW") >= 0 && sql.indexOf("CREATE VIEW") > sql.indexOf("DROP VIEW"));
      if (change !== "body") assert.ok(sql.indexOf("DROP VIEW") < sql.indexOf("ALTER TABLE") && sql.indexOf("ALTER TABLE") < sql.indexOf("CREATE VIEW"));
      assert.deepEqual((await migrator.upgrade(undefined, { allowDestructive: true })).map((m) => m.name), [migration.name]);
      assert.deepEqual(await db.fetchText("SELECT id, value FROM sv_review_view ORDER BY id"), [["1", "7"], ["2", "9"]]);
      const expected = { body: "text", column_type: "bigint", table: "integer" }[change];
      assert.deepEqual(await db.fetchText("SELECT pg_typeof(value)::text FROM sv_review_view LIMIT 1"), [[expected]]);
    } finally {
      await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public");
      await db.close();
    }
  });
}

test("sqlite rebuild under a view copies rows with legacy alter table off", async () => {
  const dir = scratch("monorm-view-rebuild-");
  const schema = migrationSchema(dir, "sqlite");
  const db = await emptyDatabase("sqlite", schema);
  try {
    await db.execute("PRAGMA legacy_alter_table=0");
    assert.deepEqual(await db.fetchText("PRAGMA legacy_alter_table"), [["0"]]);
    const migrations = new Migrations(join(dir, "migrations"), schema);
    migrations.make();
    const migrator = new Migrator(db, migrations);
    await migrator.upgrade();
    const models = load(schema, { registry: new Registry() }) as unknown as Record<string, Open>;
    await models.ReviewItem!.objects.using(db).insertMany([{ id: 1, value: 7 }, { id: 2, value: 9 }]);
    migrationSchema(dir, "sqlite", "Int", undefined, "@@check(value >= 0)");
    const migration = migrations.make("rebuild")[0]!;
    assert.ok(migration.upSql.includes("__orm_new_"));
    await migrator.upgrade(undefined, { allowDestructive: true });
    assert.deepEqual(await db.fetchText("PRAGMA legacy_alter_table"), [["0"]]);
    assert.deepEqual(await db.fetchText("SELECT id, value FROM sv_review_view ORDER BY id"), [["1", "7"], ["2", "9"]]);
    assert.deepEqual(await db.fetchText("SELECT id, value FROM sv_review_items ORDER BY id"), [["1", "7"], ["2", "9"]]);
  } finally { await db.close(); }
});

for (const dialect of ["sqlite", "postgres"] as const) {
  test(`views pull back as blocks and sidecars without drift (${dialect})`, async () => {
    const db = await emptyDatabase(dialect);
    try {
      await db.execute("CREATE TABLE sv_pull_items (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);" +
        "CREATE VIEW sv_pull_view AS SELECT id, value FROM sv_pull_items WHERE value > 0");
      if (dialect === "postgres") await db.execute("CREATE MATERIALIZED VIEW sv_pull_totals AS SELECT id, value FROM sv_pull_view;" +
        "CREATE UNIQUE INDEX sv_pull_totals_id ON sv_pull_totals(id)");
      const pulled = await pull(db);
      assert.ok(pulled.schema.includes("view SvPullView {"));
      assert.deepEqual(pulled.gaps.filter((gap) => gap.includes("view")), []);
      assert.deepEqual(pulled.differences, []);
      const dir = scratch("monorm-view-pull-");
      const schema = join(dir, "pulled.prisma");
      pulled.write(schema);
      const sidecars = join(dir, "views", dialect === "sqlite" ? "main" : "public");
      assert.ok(readFileSync(join(sidecars, "SvPullView.sql"), "utf8").includes("value > 0"));
      if (dialect === "postgres") {
        assert.ok(pulled.schema.includes("view SvPullTotals {"));
        assert.ok(pulled.schema.includes("@@materialized"));
        assert.ok(pulled.schema.includes("@@depends_on([SvPullView])"));
        assert.ok(readFileSync(join(sidecars, "SvPullTotals.sql"), "utf8").includes("sv_pull_view"));
      }
      load(schema, { registry: new Registry() });
      const migrator = new Migrator(db, new Migrations(join(dir, "migrations"), schema));
      await migrator.baseline();
      assert.deepEqual((await migrator.drift()).steps, []);
    } finally {
      if (dialect === "postgres") await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public");
      await db.close();
    }
  });
}
