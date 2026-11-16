/** Column roles (updated_at, soft delete, version) on the root of a composed model, and its bulk writes. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry, VersionConflict, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

const skip = !hasExtension("monorm.composition", "monorm.timestamps", "monorm.soft-delete", "monorm.locking");
const OLD = new Date("2000-01-01T00:00:00Z");
const DATABASES = ["sqlite", "postgresql"] as const;
const PG = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";

const roles = (mode: string) => `
model Person {
 id Int @id @default(autoincrement())
 name String
 @@timestamps.updated_at("changed", mode: "${mode}")
 @@soft_delete.deleted_at("deleted", mode: "${mode}")
 @@locking.version("version")
 @@map("comproles_js_people")
}
model Employee {
 salary Int
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("comproles_js_employees")
}
model Manager {
 level Int
 @@composition.model(parent: "Employee", parentRef: "employee", childRef: "manager")
 @@map("comproles_js_managers")
}
`;

type Row = Record<string, any>; // eslint-disable-line @typescript-eslint/no-explicit-any
type Objects = {
  insert(values: Row): Promise<Row>;
  insertMany(rows: Row[], options?: Row): { returning(): Promise<Row[]> } & Promise<number>;
  attachMany(items: [unknown, Row][]): Promise<Row[]>;
  count(): Promise<number>;
  get(...filters: unknown[]): Promise<Row>;
  filter(...filters: unknown[]): Objects;
  update(values: Row): Promise<number>;
  delete(): Promise<number>;
  hardDelete(): Promise<number>;
  undelete(): Promise<number>;
  deletedOnly(): Objects;
  then: PromiseLike<Row[]>["then"];
};

async function open(dialect: string, source: string) {
  const registry = new Registry();
  const models = loads(`${dialect === "sqlite" ? 'datasource db {\n provider = "sqlite"\n}\n' : ""}${source}`, { registry });
  const db = await connect(dialect === "sqlite" ? "sqlite://:memory:" : PG, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  const objects = (name: string) => (models[name] as unknown as { objects: { using(db: Database): Objects } }).objects.using(db);
  const column = (name: string, field: string) => (models[name] as unknown as Record<string, { eq(value: unknown): unknown }>)[field]!;
  return { db, objects, column };
}
const rows = async (db: Database, table: string) => (await db.fetch(`SELECT * FROM ${table}`)).length;
const TABLES = ["comproles_js_people", "comproles_js_employees", "comproles_js_managers"];
const watch = (db: Database) => {
  const events: { sql: string }[] = [];
  const remove = db.onQuery((e) => { events.push(e); });
  return { events, remove };
};
const when = (value: unknown) => new Date(value as string | Date).getTime();

for (const dialect of DATABASES) {
  for (const mode of ["database", "application"]) {
    test(`a change on any level bumps the root (${dialect}, ${mode})`, { skip }, async () => {
      const { db, objects, column } = await open(dialect, roles(mode));
      try {
        const managers = objects("Manager");
        let alice = await managers.insert({ name: "Alice", salary: 1, level: 1, changed: OLD });
        assert.equal(when(alice["changed"]), OLD.getTime());
        const before = alice["version"] as number;
        assert.equal(await managers.filter(column("Manager", "id").eq(alice["id"])).update({ level: 2 }), 1);
        alice = await managers.get(column("Manager", "id").eq(alice["id"]));
        assert.equal(alice["level"], 2);
        assert.ok(when(alice["changed"]) > OLD.getTime());
        assert.equal(alice["version"], before + 1);
        await db.execute("UPDATE comproles_js_people SET changed = '2000-01-01 00:00:00' WHERE id = 1");
        await db.execute("UPDATE comproles_js_employees SET salary = 9 WHERE id = 1");
        alice = await managers.get(column("Manager", "id").eq(alice["id"]));
        assert.equal(when(alice["changed"]) > OLD.getTime(), mode === "database");
      } finally {
        await db.dropTables();
        await db.close();
      }
    });

    test(`a soft delete of a composed row updates only the root (${dialect}, ${mode})`, { skip }, async () => {
      const { db, objects, column } = await open(dialect, roles(mode));
      try {
        const managers = objects("Manager");
        await managers.insertMany([{ name: "Alice", salary: 1, level: 1 }, { name: "Bob", salary: 2, level: 2 }]);
        const name = (value: string) => column("Manager", "name").eq(value);
        assert.equal(await managers.filter(name("Alice")).delete(), 1);
        assert.deepEqual(await Promise.all(TABLES.map((t) => rows(db, t))), [2, 2, 2]);
        assert.deepEqual((await managers as unknown as Row[]).map((m) => m["name"]), ["Bob"]);
        assert.deepEqual((await objects("Person") as unknown as Row[]).map((p) => p["name"]), ["Bob"]);
        assert.deepEqual((await managers.deletedOnly() as unknown as Row[]).map((m) => m["name"]), ["Alice"]);
        assert.equal(await managers.filter(name("Alice")).delete(), 0);
        assert.equal(await managers.filter(name("Alice")).undelete(), 1);
        assert.deepEqual((await objects("Manager") as unknown as Row[]).map((m) => m["name"]).sort(), ["Alice", "Bob"]);
        // A hard delete of a soft-deleted row removes it from every level; the root cascades to the levels.
        assert.equal(await managers.filter(name("Bob")).delete(), 1);
        await objects("Person").filter(column("Person", "name").eq("Bob")).hardDelete();
        assert.deepEqual(await Promise.all(TABLES.map((t) => rows(db, t))), [1, 1, 1]);
      } finally {
        await db.dropTables();
        await db.close();
      }
    });

    test(`a hard delete of a live composed row removes every level (${dialect}, ${mode})`, { skip }, async () => {
      const { db, objects, column } = await open(dialect, roles(mode));
      try {
        const managers = objects("Manager");
        await managers.insertMany([{ name: "Alice", salary: 1, level: 1 }, { name: "Bob", salary: 2, level: 2 }]);
        await objects("Person").filter(column("Person", "name").eq("Alice")).hardDelete();
        // In database mode the root trigger soft-deletes a live row; a second hard delete removes it.
        assert.deepEqual(await Promise.all(TABLES.map((t) => rows(db, t))), mode === "database" ? [2, 2, 2] : [1, 1, 1]);
        await objects("Person").filter(column("Person", "name").eq("Alice")).hardDelete();
        assert.deepEqual(await Promise.all(TABLES.map((t) => rows(db, t))), [1, 1, 1]);
      } finally {
        await db.dropTables();
        await db.close();
      }
    });

    test(`a stale write through a child model raises version conflict (${dialect}, ${mode})`, { skip }, async () => {
      const { db, objects, column } = await open(dialect, roles(mode));
      try {
        const managers = objects("Manager");
        const first = await managers.insert({ name: "Alice", salary: 1, level: 1 });
        const second = await managers.get(column("Manager", "id").eq(first["id"]));
        await (first as unknown as { update(v: Row): Promise<void> }).update({ level: 2 });
        assert.equal(first["version"], second["version"] + 1);
        await assert.rejects((second as unknown as { update(v: Row): Promise<void> }).update({ level: 3 }), VersionConflict);
        assert.equal((await managers.get(column("Manager", "id").eq(first["id"])))["level"], 2);
      } finally {
        await db.dropTables();
        await db.close();
      }
    });

    test(`a stale delete through a child model raises version conflict (${dialect}, ${mode})`, { skip }, async () => {
      const { db, objects, column } = await open(dialect, roles(mode));
      try {
        const managers = objects("Manager");
        const first = await managers.insert({ name: "Alice", salary: 1, level: 1 });
        const second = await managers.get(column("Manager", "id").eq(first["id"]));
        await (first as unknown as { update(v: Row): Promise<void> }).update({ level: 2 });
        await assert.rejects((second as unknown as { delete(): Promise<void> }).delete(), VersionConflict);
        assert.equal(await managers.count(), 1);
      } finally {
        await db.dropTables();
        await db.close();
      }
    });
  }

  test(`a raw delete on the root table soft deletes in database mode (${dialect})`, { skip }, async () => {
    const { db, objects } = await open(dialect, roles("database"));
    try {
      await objects("Manager").insert({ name: "Alice", salary: 1, level: 1 });
      await db.execute("DELETE FROM comproles_js_people WHERE id = 1");
      assert.deepEqual(await Promise.all(TABLES.map((t) => rows(db, t))), [1, 1, 1]);
      assert.equal(await objects("Manager").count(), 0);
      assert.equal(await objects("Manager").deletedOnly().count(), 1);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`a bulk attach adds the child levels to existing parents in input order (${dialect})`, { skip }, async () => {
    const { db, objects } = await open(dialect, roles("database"));
    try {
      const people = await objects("Person").insertMany([0, 1, 2].map((i) => ({ name: `p${i}` }))).returning();
      const ids = people.map((p) => p["id"]);
      const { events, remove } = watch(db);
      let made: Row[];
      try {
        made = await objects("Employee").attachMany([[ids[2], { salary: 30 }], [ids[0], { salary: 10 }], [ids[1], { salary: 20 }]]);
      } finally { remove(); }
      assert.deepEqual(made.map((e) => [e["id"], e["salary"], e["name"]]), [[ids[2], 30, "p2"], [ids[0], 10, "p0"], [ids[1], 20, "p1"]]);
      assert.equal(events.length, 2); // one insert for the level and one read
      assert.equal(await rows(db, "comproles_js_employees"), 3);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}

for (const attribute of ['@@timestamps.updated_at("changed")', '@@soft_delete.deleted_at("deleted")', "@@locking.version"]) {
  test(`a role below the root is an error that names the root (${attribute})`, { skip }, () => {
    const source = `datasource db {\n provider = "sqlite"\n}
model Person {
 id Int @id
 name String
 @@map("comproles_js_below_people")
}
model Employee {
 salary Int
 ${attribute}
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("comproles_js_below_employees")
}
`;
    assert.throws(() => loads(source, { registry: new Registry() }), /declare it on Person/);
  });
}

for (const mode of ["database", "application"]) {
  test(`a bulk insert on postgres costs a key statement, one insert per level and a read (${mode})`, { skip }, async () => {
    const { db, objects } = await open("postgresql", roles(mode));
    try {
      const { events, remove } = watch(db);
      let made: Row[];
      try {
        made = await objects("Manager").insertMany([0, 1, 2, 3, 4].map((i) => ({ name: `m${i}`, salary: i, level: i * 10 }))).returning();
      } finally { remove(); }
      assert.deepEqual(made.map((m) => m["name"]), ["m0", "m1", "m2", "m3", "m4"]);
      assert.deepEqual(made.map((m) => m["level"]), [0, 10, 20, 30, 40]);
      assert.equal(new Set(made.map((m) => m["id"])).size, 5);
      assert.ok(made.every((m) => m["version"] === 0 && m["deleted"] === null));
      const sqls = events.map((e) => e.sql.toLowerCase());
      assert.equal(sqls.length, 5, sqls.join("\n")); // key, one insert for each of three levels, one read
      assert.ok(sqls[0]!.includes("generate_series") && sqls[0]!.includes("nextval"));
      assert.ok(sqls.slice(1, 4).every((s) => s.startsWith("insert into")), sqls.join("\n"));
      assert.ok(sqls[4]!.startsWith("select"));
      assert.deepEqual(await Promise.all(TABLES.map((t) => rows(db, t))), [5, 5, 5]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}

test("a bulk insert with caller keys skips the key statement and keeps input order", { skip }, async () => {
  const { db, objects } = await open("postgresql", roles("database"));
  try {
    const managers = objects("Manager");
    let { events, remove } = watch(db);
    let made: Row[];
    try {
      made = await managers.insertMany([30, 10, 20].map((k) => ({ id: k, name: `m${k}`, salary: 1, level: 1 }))).returning();
    } finally { remove(); }
    assert.deepEqual(made.map((m) => m["id"]), [30, 10, 20]);
    assert.equal(events.length, 4); // three inserts and one read
    ({ events, remove } = watch(db));
    let mixed: Row[];
    try {
      mixed = await managers.insertMany([{ name: "a", salary: 1, level: 1 }, { id: 99, name: "b", salary: 1, level: 1 }, { name: "c", salary: 1, level: 1 }]).returning();
    } finally { remove(); }
    assert.deepEqual(mixed.map((m) => m["name"]), ["a", "b", "c"]);
    assert.ok(mixed[1]!["id"] === 99 && mixed[0]!["id"] !== mixed[2]!["id"]);
    assert.equal(events.length, 5);
  } finally {
    await db.dropTables();
    await db.close();
  }
});

test("a bulk insert on sqlite stays row by row", { skip }, async () => {
  const { db, objects } = await open("sqlite", roles("database"));
  try {
    const { events, remove } = watch(db);
    let made: Row[];
    try {
      made = await objects("Manager").insertMany([0, 1, 2].map((i) => ({ name: `m${i}`, salary: i, level: i }))).returning();
    } finally { remove(); }
    assert.deepEqual(made.map((m) => m["name"]), ["m0", "m1", "m2"]);
    assert.ok(events.length >= 9); // at least one insert for each level and row
  } finally {
    await db.dropTables();
    await db.close();
  }
});

test("a bulk insert with copy keeps its error", { skip }, async () => {
  const { db, objects } = await open("postgresql", roles("database"));
  try {
    await assert.rejects(objects("Manager").insertMany([{ name: "a", salary: 1, level: 1 }], { copy: true }), /copy/i);
  } finally {
    await db.dropTables();
    await db.close();
  }
});

const CASCADE = `
model Shop {
 id Int @id @default(autoincrement())
 @@soft_delete.deleted_at("deleted", mode: "database")
 @@map("comcas_js_shops")
}
model Person {
 id Int @id @default(autoincrement())
 name String
 @@soft_delete.deleted_at("deleted", mode: "database")
 @@map("comcas_js_people")
}
model Employee {
 shop_id Int
 shop Shop @relation(fields: [shop_id], references: [id], onDelete: Cascade)
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("comcas_js_employees")
}
`;

for (const dialect of DATABASES) {
  test(`a soft delete cascades to a composed child through its root (${dialect})`, { skip }, async () => {
    const { db, objects, column } = await open(dialect, CASCADE);
    try {
      const shops = await objects("Shop").insertMany([{}, {}]).returning();
      for (const [shop, names] of [[shops[0]!, ["a", "b"]], [shops[1]!, ["c"]]] as const)
        for (const name of names) await objects("Employee").insert({ name, shopId: shop["id"] });
      assert.equal(await objects("Shop").filter(column("Shop", "id").eq(shops[0]!["id"])).delete(), 1);
      assert.deepEqual((await objects("Employee") as unknown as Row[]).map((e) => e["name"]).sort(), ["c"]);
      assert.equal(await rows(db, "comcas_js_people"), 3);
      assert.equal(await rows(db, "comcas_js_employees"), 3);
      const flagged = await db.fetch("SELECT name FROM comcas_js_people WHERE deleted IS NOT NULL ORDER BY name");
      assert.deepEqual(flagged.map((r) => r["name"]), ["a", "b"]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}
