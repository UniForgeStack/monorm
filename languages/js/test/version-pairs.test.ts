/** Run with an artifact that selects the locking extension: bound writes check the loaded versions. */
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import ts from "typescript";
import { connect, NotLoaded, of, Registry, VersionConflict, type Database } from "../src/index.js";
import { native } from "../src/native.js";
import { loadsSoftDelete } from "./soft-delete-ext.js";
import { hasExtension } from "./artifact-caps.js";

const locking = { skip: !hasExtension("orm.locking") };
const both = { skip: !hasExtension("orm.locking") || !hasExtension("orm.soft-delete") };
type Models = Record<string, any>;
const bound = (b: unknown): any => b;

const LOCKED = `
model Doc {
  id      Int    @id
  title   String
  @@locking.version
  @@map("vp_node_docs")
}
model Plain {
  id      Int    @id
  title   String
  @@map("vp_node_plain")
}`;
const PAGES = `
model Page {
  id      Int    @id
  title   String
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@locking.version
  @@map("vp_node_pages")
}`;
const DOCS = LOCKED + PAGES;

async function open(dialect: string): Promise<[Database, Models, string[]]> {
  const registry = new Registry();
  const models = await loadsSoftDelete((dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + DOCS, { registry }) as unknown as Models;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  const statements: string[] = [];
  db.onQuery((event) => { statements.push(event.sql); });
  return [db, models, statements];
}

/** Load a schema through the `models.ts` that `orm generate` writes, as an application does, on SQLite. */
async function openGenerated(schema: string): Promise<[Database, Models, string[]]> {
  const root = mkdtempSync(join(tmpdir(), "orm-version-pairs-"));
  writeFileSync(join(root, "schema.prisma"), 'datasource db { provider = "sqlite" }\n' + schema);
  assert.equal(await native().cli(["--schema", join(root, "schema.prisma"), "generate", "typescript", "-o", join(root, "models.ts"), "--import", "orm"]), 0);
  const src = join(import.meta.dirname, "..", "src");
  const imports: Record<string, string> = { '"orm"': JSON.stringify(pathToFileURL(join(src, "index.js")).href), '"orm/ext"': JSON.stringify(pathToFileURL(join(src, "ext.js")).href) };
  const emit = (from: string, to: string) => {
    mkdirSync(to, { recursive: true });
    for (const name of readdirSync(from).filter((n) => n.endsWith(".ts") && !n.endsWith(".d.ts"))) {
      let { outputText } = ts.transpileModule(readFileSync(join(from, name), "utf8"), { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } });
      for (const [name, url] of Object.entries(imports)) outputText = outputText.replaceAll(` from ${name}`, ` from ${url}`);
      writeFileSync(join(to, name.replace(/\.ts$/, ".js")), outputText);
    }
  };
  const out = join(root, "js");
  emit(root, out);
  for (const dir of readdirSync(join(root, "orm-ext"))) emit(join(root, "orm-ext", dir), join(out, "orm-ext", dir));
  // Bun runs every test file in one process, so the default registry may already hold these models.
  const file = join(out, "models.js");
  const source = readFileSync(file, "utf8");
  const isolated = source.replace(/define\(SCHEMA(\)|, \{)/, (_, tail: string) => `define(SCHEMA, { registry: __registry${tail === ")" ? " })" : ","}`);
  assert.notEqual(isolated, source);
  writeFileSync(file, `import { Registry as __Registry } from ${imports['"orm"']};\nexport const __registry = new __Registry();\n${isolated}`);
  const { __registry: registry, ...models } = (await import(pathToFileURL(file).href)) as Models;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  const statements: string[] = [];
  db.onQuery((event) => { statements.push(event.sql); });
  return [db, models, statements];
}

const writes = (statements: string[]) => statements.filter((sql) => /^\s*(UPDATE|DELETE)/i.test(sql));

async function seed(models: Models, db: Database, name: string, count = 3): Promise<any[]> {
  const rows = Array.from({ length: count }, (_, i) => ({ id: i + 1, title: `t${i + 1}` }));
  return models[name].objects.using(db).insertMany(rows).returning();
}

async function titles(models: Models, db: Database, name = "Doc"): Promise<Record<number, [string, number]>> {
  const out: Record<number, [string, number]> = {};
  for (const r of await models[name].objects.using(db).orderBy(models[name].id)) out[r.id] = [r.title, r.version];
  return out;
}

for (const dialect of ["sqlite", "postgres"]) {
  test(`a bound update with a stale object throws and changes no row (${dialect})`, locking, async () => {
    const [db, models, statements] = await open(dialect);
    const { Doc } = models;
    try {
      const [a, b, c] = await seed(models, db, "Doc");
      await (await Doc.objects.using(db).get(Doc.id.eq(2))).update({ title: "newer" });
      statements.length = 0;
      await assert.rejects(of([a, b, c]).update({ title: "x" }), VersionConflict);
      assert.deepEqual(await titles(models, db), { 1: ["t1", 0], 2: ["newer", 1], 3: ["t3", 0] });
      assert.deepEqual([a.title, a.version, b.title, b.version], ["t1", 0, "t2", 0]);
      const [sql] = writes(statements);
      assert.equal(writes(statements).length, 1);
      assert.match(sql!, /version/);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a bound update of fresh objects writes one statement and bumps the versions (${dialect})`, locking, async () => {
    const [db, models, statements] = await open(dialect);
    try {
      const [a, , c] = await seed(models, db, "Doc");
      statements.length = 0;
      const result = await of([c, a]).update({ title: "x" });
      assert.deepEqual(result, [c, a]);
      assert.deepEqual([a, c].map((o) => [o.title, o.version]), [["x", 1], ["x", 1]]);
      assert.deepEqual(await titles(models, db), { 1: ["x", 1], 2: ["t2", 0], 3: ["x", 1] });
      assert.equal(writes(statements).length, 1);
      await of([a, c]).update({ title: "y" });
      assert.deepEqual([a, c].map((o) => [o.title, o.version]), [["y", 2], ["y", 2]]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a bound delete with a stale object throws and deletes no row (${dialect})`, locking, async () => {
    const [db, models] = await open(dialect);
    const { Doc } = models;
    try {
      const [a, b, c] = await seed(models, db, "Doc");
      await (await Doc.objects.using(db).get(Doc.id.eq(3))).update({ title: "newer" });
      await assert.rejects(of([a, b, c]).delete(), VersionConflict);
      assert.deepEqual(Object.keys(await titles(models, db)), ["1", "2", "3"]);
      assert.deepEqual(await of([a, b]).delete(), [a, b]);
      assert.deepEqual(Object.keys(await titles(models, db)), ["3"]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a conflict rolls back only the bound write inside a transaction (${dialect})`, locking, async () => {
    const [db, models] = await open(dialect);
    const { Doc } = models;
    try {
      const [a, b] = await seed(models, db, "Doc");
      await (await Doc.objects.using(db).get(Doc.id.eq(1))).update({ title: "newer" });
      await db.transaction(async () => {
        await Doc.objects.using(db).insert({ id: 9, title: "kept" });
        await assert.rejects(of([a, b]).update({ title: "x" }), VersionConflict);
        const rows = await Doc.objects.using(db).filter(Doc.id.in([2, 9])).orderBy(Doc.id);
        assert.deepEqual(rows.map((r: any) => r.title), ["t2", "kept"]);
      });
      const now = await titles(models, db);
      assert.deepEqual([now[9], now[2]], [["kept", 0], ["t2", 0]]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`an object hidden by a filter is not a conflict but a stale visible one is (${dialect})`, locking, async () => {
    const [db, models] = await open(dialect);
    const { Doc } = models;
    try {
      const [a, b] = await seed(models, db, "Doc");
      assert.deepEqual(await of([a, b]).filter(Doc.title.eq("t1")).update({ title: "x" }), [a]);
      await (await Doc.objects.using(db).get(Doc.id.eq(2))).update({ title: "newer" });
      assert.deepEqual(await of([a, b]).filter(Doc.title.eq("x")).update({ title: "y" }), [a]);
      await assert.rejects(of([b]).filter(Doc.title.eq("newer")).update({ title: "z" }), VersionConflict);
      assert.deepEqual(await of([b]).filter(Doc.title.eq("nothing")).update({ title: "z" }), []);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`two objects of one row with different versions conflict (${dialect})`, locking, async () => {
    const [db, models] = await open(dialect);
    const { Doc } = models;
    try {
      const [first] = await seed(models, db, "Doc");
      const second = await Doc.objects.using(db).get(Doc.id.eq(1));
      await second.update({ title: "newer" });
      await assert.rejects(of([first, second]).update({ title: "x" }), VersionConflict);
      assert.deepEqual((await titles(models, db))[1], ["newer", 1]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a partial object needs its version (${dialect})`, locking, async () => {
    const [db, models] = await open(dialect);
    const { Doc } = models;
    try {
      await seed(models, db, "Doc");
      const partial = await Doc.objects.using(db).load(Doc.title).get(Doc.id.eq(1));
      await assert.rejects(of([partial]).update({ title: "x" }), NotLoaded);
      const versioned = await Doc.objects.using(db).load(Doc.title, Doc.version).get(Doc.id.eq(1));
      assert.deepEqual(await of([versioned]).update({ title: "x" }), [versioned]);
      assert.equal(versioned.version, 1);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a bound write on a model without locking runs the same statement (${dialect})`, locking, async () => {
    const [db, models, statements] = await open(dialect);
    const { Plain } = models;
    try {
      const held = await seed(models, db, "Plain");
      statements.length = 0;
      await of(held).update({ title: "x" });
      assert.equal(statements.length, 1);
      assert.doesNotMatch(statements[0]!, /version/);
      assert.deepEqual((await Plain.objects.using(db).all()).map((r: any) => r.title), ["x", "x", "x"]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`an instance write of a locking model has one version predicate (${dialect})`, locking, async () => {
    const [db, models, statements] = await open(dialect);
    const { Doc } = models;
    try {
      const [a, b, c] = await seed(models, db, "Doc");
      statements.length = 0;
      await a.update({ title: "x" });
      await b.delete();
      for (const sql of writes(statements)) {
        const where = sql.split(" WHERE ")[1]!;
        assert.deepEqual([where.split('"version" =').length - 1, where.split('"id" =').length - 1], [1, 1], sql);
      }
      await (await Doc.objects.using(db).get(Doc.id.eq(3))).update({ title: "newer" });
      await assert.rejects(c.update({ title: "y" }), VersionConflict);
      await assert.rejects(c.delete(), VersionConflict);
      assert.deepEqual(await titles(models, db), { 1: ["x", 1], 3: ["newer", 1] });
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a bound write matches a key and version pair for each object (${dialect})`, locking, async () => {
    const [db, models, statements] = await open(dialect);
    try {
      const held = await seed(models, db, "Doc", 4);
      statements.length = 0;
      await of(held).update({ title: "x" });
      assert.equal(writes(statements).length, 1);
      const where = writes(statements)[0]!.split(" WHERE ")[1]!;
      assert.deepEqual([" IN (", '"version" =', " OR "].map((part) => where.split(part).length - 1), [1, 4, 3], where);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a conflict reads only the key and the version of the rows that the write left out (${dialect})`, locking, async () => {
    const [db, models, statements] = await open(dialect);
    const { Doc } = models;
    try {
      const [a, b, c] = await seed(models, db, "Doc");
      await (await Doc.objects.using(db).get(Doc.id.eq(2))).update({ title: "newer" });
      statements.length = 0;
      await assert.rejects(of([a, b, c]).update({ title: "x" }), (e: Error) => {
        assert.ok(e instanceof VersionConflict);
        assert.equal(e.message, "Doc 2 was changed since it was loaded");
        return true;
      });
      const [read, ...more] = statements.filter((sql) => /^\s*SELECT/i.test(sql));
      assert.equal(more.length, 0);
      assert.match(read!, /"version"/);
      assert.doesNotMatch(read!, /"title"| IN \(| OR /);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`undelete and hardDelete check the loaded versions (${dialect})`, both, async () => {
    const [db, models] = await open(dialect);
    const { Page } = models;
    try {
      const [a, b, c] = await seed(models, db, "Page");
      assert.deepEqual(await of([a, b]).delete(), [a, b]);
      assert.deepEqual([a.version, b.version], [1, 1]);
      await (await Page.objects.using(db).allWithDeleted().get(Page.id.eq(1))).undelete();
      await assert.rejects(bound(of([a, b])).undelete(), VersionConflict);
      const rows = await Page.objects.using(db).allWithDeleted().orderBy(Page.id);
      assert.deepEqual(rows.map((r: any) => [r.id, r.deleted === null, r.version]), [[1, true, 2], [2, false, 1], [3, true, 0]]);
      await a.load({ reload: true });
      assert.deepEqual(await bound(of([a, b])).undelete(), [b]);
      assert.deepEqual([b.deleted, b.version], [null, 2]);
      await (await Page.objects.using(db).get(Page.id.eq(1))).update({ title: "newer" });
      await assert.rejects(bound(of([a, c])).hardDelete(), VersionConflict);
      assert.equal(await Page.objects.using(db).allWithDeleted().count(), 3);
      assert.deepEqual((await bound(of([c, b])).hardDelete()).map((r: any) => r.id), [3, 2]);
      assert.equal(await Page.objects.using(db).allWithDeleted().count(), 1);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a row that another writer soft-deleted is a conflict (${dialect})`, both, async () => {
    const [db, models] = await open(dialect);
    const { Page } = models;
    try {
      const [a] = await seed(models, db, "Page");
      await (await Page.objects.using(db).get(Page.id.eq(1))).delete();
      await assert.rejects(of([a]).update({ title: "x" }), VersionConflict);
    } finally { await db.dropTables(); await db.close(); }
  });
}

test("a generated module checks the loaded versions of a bound write", locking, async () => {
  const [db, models, statements] = await openGenerated(LOCKED);
  const { Doc, Plain } = models;
  try {
    const [a, b, c] = await seed(models, db, "Doc");
    await (await Doc.objects.using(db).get(Doc.id.eq(2))).update({ title: "newer" });
    await assert.rejects(of([a, b, c]).update({ title: "x" }), VersionConflict);
    await assert.rejects(of([a, b, c]).delete(), VersionConflict);
    assert.deepEqual(await titles(models, db), { 1: ["t1", 0], 2: ["newer", 1], 3: ["t3", 0] });
    const held = await seed(models, db, "Plain");
    statements.length = 0;
    await of(held).update({ title: "x" });
    assert.deepEqual(statements.filter((sql) => /version/.test(sql)), []);
    assert.deepEqual((await Plain.objects.using(db).all()).map((r: any) => r.title), ["x", "x", "x"]);
  } finally { await db.close(); }
});

test("a generated module checks the versions of an undelete", both, async () => {
  const [db, models] = await openGenerated(PAGES);
  const { Page } = models;
  try {
    const [a, b] = await seed(models, db, "Page");
    assert.deepEqual(await of([a, b]).delete(), [a, b]);
    await (await Page.objects.using(db).allWithDeleted().get(Page.id.eq(1))).undelete();
    await assert.rejects(bound(of([a, b])).undelete(), VersionConflict);
  } finally { await db.close(); }
});
