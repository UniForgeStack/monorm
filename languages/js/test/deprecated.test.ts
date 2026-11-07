import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { connect, loads, Migrations, Migrator, Registry } from "../src/index.js";

const source = `
model Item {
  id     Int    @id @default(autoincrement())
  title  String
  legacy String @default("old") @deprecated
  @@map("deprecated_node_items")
}
`;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Any = any;

test("a deprecated field is not in the class", () => {
  const Item = loads(source, { registry: new Registry() }).Item!;
  assert.deepEqual(Item._meta.fieldList.map((f) => f.name), ["id", "title"]);
  assert.ok(!("legacy" in Item));
});

for (const dialect of ["sqlite", "postgres"]) {
  test(`a deprecated not null default column does not stop writes and reads (${dialect})`, async () => {
    const registry = new Registry();
    const Item = loads((dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + source, { registry }).Item as Any;
    const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
    const db = await connect(url, { registry, default: false });
    await db.createTables();
    const statements: string[] = [];
    const remove = db.onQuery((e) => { statements.push(e.sql); });
    try {
      const one = await Item.objects.using(db).insert({ title: "a" });
      const many = await Item.objects.using(db).insertMany([{ title: "b" }, { title: "c" }]).returning();
      assert.deepEqual(many.map((r: Any) => r.title), ["b", "c"]);
      assert.equal((await Item.objects.using(db).get(Item.id.eq(one.id))).title, "a");
      await Item.objects.using(db).filter(Item.id.eq(one.id)).update({ title: "a2" });
      assert.deepEqual((await Item.objects.using(db).orderBy("id")).map((r: Any) => r.title), ["a2", "b", "c"]);
      await one.update({ title: "a3" });
      await Item.objects.using(db).filter(Item.id.eq(many[0].id)).delete();
      await many[1].delete();
      assert.deepEqual((await Item.objects.using(db).orderBy("id")).map((r: Any) => r.title), ["a3"]);
      remove();
      assert.ok(statements.length > 0 && !statements.some((sql) => sql.includes("legacy")));
      // The column is still in the table, filled by its database default.
      assert.deepEqual((await db.fetch("SELECT legacy FROM deprecated_node_items")).map((r) => r.legacy), ["old"]);
    } finally { await db.dropTables(); await db.close(); }
  });
}

/** A database of its own, so that drift sees no table of another test. */
async function ownDatabaseUrl(): Promise<string> {
  const base = process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
  const name = `${base.split("/").pop()}_deprecated`;
  const admin = await connect(base, { default: false, maxConnections: 1 });
  try {
    if ((await admin.fetchText(`SELECT 1 FROM pg_database WHERE datname = '${name}'`)).length === 0) {
      await admin.execute(`CREATE DATABASE "${name}"`);
    }
  } finally { await admin.close(); }
  return base.replace(/\/[^/]*$/, `/${name}`);
}

for (const dialect of ["sqlite", "postgres"]) {
  test(`a deprecated column that exists is no drift and no migration (${dialect})`, async () => {
    const prefix = dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "";
    const table = "deprecated_node_drift_items";
    const plain = prefix + source.replace(" @deprecated", "").replace("deprecated_node_items", table);
    const deprecated = prefix + source.replace("deprecated_node_items", table);
    const registry = (text: string) => { const r = new Registry(); loads(text, { registry: r }); return r; };
    const dir = mkdtempSync(join(tmpdir(), "monorm-deprecated-"));
    const url = dialect === "sqlite" ? "sqlite://:memory:" : await ownDatabaseUrl();
    const db = await connect(url, { registry: registry(plain), default: false });
    const cleanup = `DROP TABLE IF EXISTS ${table}; DROP TABLE IF EXISTS monorm_migrations`;
    try {
      await db.execute(cleanup);
      new Migrations(dir, registry(plain)).make();
      await new Migrator(db, new Migrations(dir, registry(plain))).upgrade();
      const later = new Migrations(dir, registry(deprecated));
      assert.equal(later.make().length, 0);
      assert.equal((await new Migrator(db, later).drift()).steps.length, 0);
      await db.fetch(`SELECT legacy FROM ${table}`); // the column still exists
    } finally { await db.execute(cleanup); await db.close(); }
  });
}
