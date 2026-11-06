/** A SQL view model reads, refuses every write with `ReadOnlyModel` before SQL, and a materialized view refreshes. */
import assert from "node:assert/strict";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { connect, load, ReadOnlyModel, QueryError, Registry } from "../src/index.js";
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

async function open(dialect: Dialect) {
  const registry = new Registry();
  const models = load(schemaFile(dialect), { registry }) as unknown as Record<string, Open>;
  const db = await connect(dialect === "sqlite" ? "sqlite://:memory:" : url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  await models.User!.objects.using(db).insertMany([{ id: 1, name: "ada", active: true }, { id: 2, name: "bob", active: false }]);
  return { models, db };
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
