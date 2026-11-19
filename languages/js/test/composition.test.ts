/** Model composition through the public API; needs the monorm/extensions/model-composition package. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, IntegrityError, TransactionAborted, loads, Registry, scope, statementInspection, type InspectedStatement } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

const enabled = hasExtension("monorm.composition");
const source = `
model Person {
 id Int @id @default(autoincrement())
 name String @check("name <> 'BADROOT'")
 @@map("composition06_node_people")
}
model Employee {
 salary Int @check("salary > 0")
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("composition06_node_employees")
}
model Manager {
 level Int @check("level > 0")
 @@composition.model(parent: "Employee", parentRef: "employee", childRef: "manager")
 @@map("composition06_node_managers")
}
model Customer {
 points Int
 @@composition.model(parent: "Person", parentRef: "person", childRef: "customer")
 @@map("composition06_node_customers")
}
`;
type Row = Record<string, unknown>;
type Objects = {
  insert(values: Row): InspectedStatement<Row>;
  attach(parentId: unknown, values: Row): Promise<Row>;
  count(): Promise<number>;
  get(...filters: unknown[]): Promise<Row>;
  filter(...filters: unknown[]): Objects;
  load(...items: unknown[]): Objects;
  update(values: Row): InspectedStatement<number>;
  delete(): InspectedStatement<number> & { returning(): Promise<Row[]> };
  insertMany(rows: Row[]): Promise<number>;
};

type Db = Awaited<ReturnType<typeof connect>>;
const name = (model: unknown) => (model as Record<string, { eq(value: unknown): unknown }>)["name"]!.eq("Alice");
/** The composition06 tables, empty, with `managerKey` as the manager level key. */
async function handMadeTables(db: Db, managerKey: string): Promise<void> {
  await db.dropTables();
  for (const statement of [
    "CREATE TABLE composition06_node_people (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
    "CREATE TABLE composition06_node_employees (id INTEGER PRIMARY KEY REFERENCES composition06_node_people (id) ON DELETE CASCADE, salary INTEGER NOT NULL)",
    `CREATE TABLE composition06_node_managers (${managerKey}, level INTEGER NOT NULL)`,
    "CREATE TABLE composition06_node_customers (id INTEGER PRIMARY KEY REFERENCES composition06_node_people (id) ON DELETE CASCADE, points INTEGER NOT NULL)",
  ]) await db.execute(statement);
}
async function dropHandMadeTables(db: Db): Promise<void> {
  for (const table of ["managers", "employees", "customers", "people"]) await db.execute(`DROP TABLE IF EXISTS composition06_node_${table}`);
}

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : (process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test");
  test(`composed create, attach, update and delete (${provider})`, { skip: !enabled || !url }, async () => {
    const registry = new Registry();
    const models = loads(`datasource db {\n provider = "${provider}"\n}\n${source}`, { registry });
    const [Person, Employee, Manager, Customer] = ["Person", "Employee", "Manager", "Customer"].map((name) => models[name]!);
    const field = (model: unknown, name: string) => (model as Record<string, { eq(value: unknown): unknown }>)[name]!;
    const db = await connect(url!, { registry, default: false });
    const objects = (model: unknown) => (model as { objects: { using(db: unknown): Objects } }).objects.using(db);
    const counts = (...list: unknown[]) => Promise.all(list.map((model) => objects(model).count()));
    await db.dropTables();
    await db.createTables();
    try {
      const insert = objects(Manager).insert({ name: "Alice", salary: 20, level: 1 });
      assert.equal(insert.inspect().statementCount, 4); // three owners and the logical identity read
      assert.equal(insert.inspect().hasBatching, false);
      assert.deepStrictEqual(insert.inspect().statements[3], statementInspection({ joinsCount: 1, scalarCorrelated: 2 }));
      const alice = await insert;
      assert.deepEqual([alice["name"], alice["salary"], alice["level"]], ["Alice", 20, 1]);
      assert.deepEqual(await counts(Person, Employee, Manager), [1, 1, 1]);

      // One person is an Employee and a Customer at the same time.
      const customer = await objects(Customer).attach(alice["id"], { points: 5 });
      assert.deepEqual([customer["id"], customer["name"], customer["points"]], [alice["id"], "Alice", 5]);
      await assert.rejects(objects(Customer).attach(alice["id"], { points: 6 }), IntegrityError);
      await assert.rejects(objects(Customer).attach((alice["id"] as number) + 100, { points: 6 }), IntegrityError);
      assert.equal(await objects(Customer).count(), 1);

      // A failed leaf or root insert rolls back the whole chain.
      await assert.rejects(objects(Manager).insert({ name: "Bob", salary: 10, level: 0 }), IntegrityError);
      await assert.rejects(objects(Manager).insert({ name: "BADROOT", salary: 10, level: 1 }), IntegrityError);
      assert.equal(await objects(Person).count(), 1);

      // An inherited-field filter selects the composed rows to update.
      const update = objects(Manager).filter(field(Manager, "salary").eq(20)).update({ level: 4 });
      assert.equal(update.inspect().statementCount, 2);
      assert.equal(update.inspect().hasBatching, true);
      assert.equal(await update, 1);
      assert.equal(await objects(Manager).filter(field(Manager, "salary").eq(99)).update({ level: 5 }), 0);
      assert.equal((await objects(Manager).get(field(Manager, "id").eq(alice["id"])))["level"], 4);
      assert.equal((await objects(Employee).filter(field(Employee, "name").eq("Alice")).get())["salary"], 20);

      const person = await objects(Person).load(field(Person, "employee"), field(Person, "customer")).get();
      assert.deepEqual([(person["employee"] as Row)["salary"], (person["customer"] as Row)["points"]], [20, 5]);

      // Deleting through a child removes every level, the sibling child too.
      const deletion = objects(Manager).filter(field(Manager, "id").eq(alice["id"])).delete();
      assert.equal(deletion.inspect().statementCount, 2);
      assert.equal(deletion.inspect().hasBatching, true);
      await deletion;
      assert.deepEqual(await counts(Person, Employee, Manager, Customer), [0, 0, 0, 0]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
  test(`a delete through a child model removes every level and counts like the root (${provider})`, { skip: !enabled || !url }, async () => {
    const registry = new Registry();
    const models = loads(`datasource db {\n provider = "${provider}"\n}\n${source}`, { registry });
    const [Person, Employee, Manager] = ["Person", "Employee", "Manager"].map((name) => models[name]!);
    const field = (model: unknown, name: string) => (model as Record<string, { eq(value: unknown): unknown }>)[name]!;
    const db = await connect(url!, { registry, default: false });
    const objects = (model: unknown) => (model as { objects: { using(db: unknown): Objects } }).objects.using(db);
    await db.dropTables();
    await db.createTables();
    try {
      await objects(Manager).insertMany([{ name: "Alice", salary: 1, level: 1 }, { name: "Bob", salary: 2, level: 2 }, { name: "Cy", salary: 3, level: 3 }, { name: "Dee", salary: 4, level: 4 }]);
      assert.equal(await objects(Manager).filter(field(Manager, "name").eq("Alice")).delete(), 1);
      const returned = await objects(Manager).filter(field(Manager, "name").eq("Bob")).delete().returning();
      assert.deepEqual(returned.map((row) => row["name"]), ["Bob"]);
      assert.equal(await objects(Person).filter(field(Person, "name").eq("Cy")).delete(), 1);
      const rootReturned = await objects(Person).filter(field(Person, "name").eq("Dee")).delete().returning();
      assert.deepEqual(rootReturned.map((row) => row["name"]), ["Dee"]);
      assert.deepEqual(await Promise.all([Person, Employee, Manager].map((model) => objects(model).count())), [0, 0, 0]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
  test(`a delete through a child model over tables without a cascade fails and deletes nothing (${provider})`, { skip: !enabled || !url }, async () => {
    const registry = new Registry();
    const models = loads(`datasource db {\n provider = "${provider}"\n}\n${source}`, { registry });
    const [Person, Employee, Manager] = ["Person", "Employee", "Manager"].map((name) => models[name]!);
    const field = (model: unknown, name: string) => (model as Record<string, { eq(value: unknown): unknown }>)[name]!;
    const db = await connect(url!, { registry, default: false });
    const objects = (model: unknown) => (model as { objects: { using(db: unknown): Objects } }).objects.using(db);
    await db.dropTables();
    // Tables made by hand: the manager key has no ON DELETE CASCADE.
    for (const statement of [
      "CREATE TABLE composition06_node_people (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
      "CREATE TABLE composition06_node_employees (id INTEGER PRIMARY KEY REFERENCES composition06_node_people (id) ON DELETE CASCADE, salary INTEGER NOT NULL)",
      "CREATE TABLE composition06_node_managers (id INTEGER PRIMARY KEY REFERENCES composition06_node_employees (id), level INTEGER NOT NULL)",
      "CREATE TABLE composition06_node_customers (id INTEGER PRIMARY KEY, points INTEGER NOT NULL)",
      "INSERT INTO composition06_node_people (id, name) VALUES (1, 'Alice')",
      "INSERT INTO composition06_node_employees (id, salary) VALUES (1, 5)",
      "INSERT INTO composition06_node_managers (id, level) VALUES (1, 1)",
    ]) await db.execute(statement);
    try {
      await assert.rejects(async () => { await objects(Manager).filter(field(Manager, "name").eq("Alice")).delete(); }, /ON DELETE CASCADE.*composition06_node_managers\.id/);
      assert.deepEqual(await Promise.all([Person, Employee, Manager].map((model) => objects(model).count())), [1, 1, 1]);
    } finally {
      for (const table of ["managers", "employees", "customers", "people"]) await db.execute(`DROP TABLE composition06_node_${table}`);
      await db.close();
    }
  });
  test(`a composed delete checks the cascades again after a script changes them (${provider})`, { skip: !enabled || !url }, async () => {
    const registry = new Registry();
    const models = loads(`datasource db {\n provider = "${provider}"\n}\n${source}`, { registry });
    const Manager = models["Manager"]!;
    const db = await connect(url!, { registry, default: false });
    const remove = () => (Manager as unknown as { objects: { using(db: unknown): Objects } }).objects.using(db).filter(name(Manager)).delete();
    const plain = "id INTEGER PRIMARY KEY REFERENCES composition06_node_employees (id)";
    try {
      await handMadeTables(db, plain);
      await assert.rejects(async () => { await remove(); }, /composition06_node_managers\.id/);
      // The failure was not cached: the next delete sees the cascade that was added.
      await handMadeTables(db, `${plain} ON DELETE CASCADE`);
      await remove();
      // A script that drops the cascade clears the passed check, so the next delete fails again.
      await handMadeTables(db, plain);
      await assert.rejects(async () => { await remove(); }, /composition06_node_managers\.id/);
      await db.transaction(async () => {
        await assert.rejects(async () => { await remove(); }, /composition06_node_managers\.id/);
      });
    } finally {
      await dropHandMadeTables(db);
      await db.close();
    }
  });
  test(`a composed delete compares key names without case and needs foreign_keys on sqlite (${provider})`, { skip: !enabled || provider !== "sqlite" }, async () => {
    const registry = new Registry();
    const models = loads(`datasource db {\n provider = "sqlite"\n}\n${source}`, { registry });
    const Manager = models["Manager"]!;
    const remove = (db: unknown) => (Manager as unknown as { objects: { using(db: unknown): Objects } }).objects.using(db).filter(name(Manager)).delete();
    const db = await connect("sqlite://:memory:", { registry, default: false });
    try {
      await handMadeTables(db, "ID INTEGER PRIMARY KEY REFERENCES COMPOSITION06_NODE_EMPLOYEES (ID) ON DELETE CASCADE");
      await remove(db);
    } finally {
      await dropHandMadeTables(db);
      await db.close();
    }
    const off = await connect("sqlite://:memory:", { registry, default: false });
    try {
      await handMadeTables(off, "id INTEGER PRIMARY KEY REFERENCES composition06_node_employees (id) ON DELETE CASCADE");
      await off.execute("PRAGMA foreign_keys = OFF");
      await assert.rejects(async () => { await remove(off); }, /PRAGMA foreign_keys = ON/);
    } finally {
      await dropHandMadeTables(off);
      await off.close();
    }
  });
  test(`a failed composed insert marks the enclosing transaction (${provider})`, { skip: !enabled || !url }, async () => {
    const registry = new Registry();
    const models = loads(`datasource db {\n provider = "${provider}"\n}\n${source}`, { registry });
    const [Person, Manager] = ["Person", "Manager"].map((name) => models[name]!);
    const db = await connect(url!, { registry, default: false });
    const objects = (model: unknown) => (model as { objects: { using(db: unknown): Objects } }).objects.using(db);
    await db.dropTables();
    await db.createTables();
    try {
      const bad = { name: "Bob", salary: 10, level: 0 };
      // Outside a transaction the insert is atomic: its root row rolls back.
      await assert.rejects(objects(Manager).insert(bad), IntegrityError);
      assert.equal(await objects(Person).count(), 0);
      // Inside one it opens no savepoint: the failure marks the transaction.
      await assert.rejects(
        db.transaction(async () => {
          await assert.rejects(objects(Manager).insert(bad), IntegrityError);
        }),
        (e: unknown) => e instanceof TransactionAborted && e.cause instanceof IntegrityError,
      );
      assert.equal(await objects(Person).count(), 0);
      // A nested transaction() recovers.
      await db.transaction(async () => {
        await assert.rejects(db.transaction(async () => { await objects(Manager).insert(bad); }), IntegrityError);
        await objects(Manager).insert({ name: "Ok", salary: 1, level: 1 });
      });
      assert.equal(await objects(Person).count(), 1);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}

for (const [attribute, extension] of [
  ['@@locking.version', "monorm.locking"],
  ['@@soft_delete.deleted_at("deleted")', "monorm.soft-delete"],
  ['@@timestamps.updated_at("changed")', "monorm.timestamps"],
] as const) {
  test(`a tracking attribute below the root of a composed model is rejected (${extension}) (${extension})`, { skip: !enabled || !hasExtension(extension) }, () => {
    const roleSource = `datasource db {\n provider = "sqlite"\n}
model Person {
 id Int @id
 name String
 @@map("composition06_node_role_people")
}
model Employee {
 salary Int
 ${attribute}
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("composition06_node_role_employees")
}
`;
    assert.throws(() => loads(roleSource, { registry: new Registry() }), /below the root of its ownership chain; declare it on Person/);
  });
}

const computedSource = `
model Person {
 id Int @id @default(autoincrement())
 name String
 @@map("composition_computed_node_people")
}
model Staff {
 text String
 start Int
 end Int
 shop_id Int
 updated_by String?
 first String
 last String
 @@composition.model(parent: "Person", parentRef: "person", childRef: "staff")
 @@outside.guard
 @@map("composition_computed_node_staff")
}
`;

test("a composed write returns the computed field", { skip: !enabled || !hasExtension("acme.outside") }, async () => {
  const registry = new Registry();
  const models = loads(`datasource db {\n provider = "sqlite"\n}\n${computedSource}`, { registry });
  const Staff = models["Staff"] as unknown as { objects: { using(db: unknown): Objects }; id: { eq(value: unknown): unknown } };
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  try {
    await scope({ shop: 1, user: "ann" }, async () => {
      const row = await Staff.objects.using(db).insert({ name: "Ada", text: "t", start: 1, end: 2, first: "grace", last: "hopper" });
      assert.equal(row["initials"], "GH");
      const staff = Staff.objects.using(db).filter(Staff.id.eq(row["id"])) as unknown as { update(values: Row): { returning(): Promise<Row[]> } };
      const updated = await staff.update({ first: "mary", last: "shelley" }).returning();
      assert.deepEqual(updated.map((r) => r["initials"]), ["MS"]);
    });
  } finally { await db.close(); }
});

const returnedSource = `
model Person {
 id Int @id @default(autoincrement())
 name String
 bio String?
 @@map("composition_returned_node_people")
}
model Staff {
 salary Int
 badge String?
 @@composition.model(parent: "Person", parentRef: "person", childRef: "staff")
 @@query.defaults(fields: ["name", "salary"])
 @@map("composition_returned_node_staff")
}
`;

test("a multi-table write returns the default selection or the named columns", { skip: !enabled || !hasExtension("monorm.query-defaults") }, async () => {
  const registry = new Registry();
  const models = loads(`datasource db {\n provider = "sqlite"\n}\n${returnedSource}`, { registry });
  type Returned<T> = { returning(...columns: unknown[]): Promise<T> };
  type StaffObjects = {
    withoutDefaults(): StaffObjects;
    filter(...filters: unknown[]): StaffObjects;
    count(): Promise<number>;
    insert(values: Row): PromiseLike<Row> & Returned<Row>;
    insertMany(rows: Row[]): Returned<Row[]>;
    update(values: Row): Returned<Row[]>;
    delete(): Returned<Row[]>;
  };
  const Staff = models["Staff"] as unknown as { objects: { using(db: unknown): StaffObjects } } & Record<string, { eq(value: unknown): unknown }>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  const sql: string[] = [];
  db.onQuery((e: { sql: string }) => { sql.push(e.sql); });
  const json = (rows: Row[]) => rows.map((r) => (r as unknown as { toJSON(): Row }).toJSON());
  try {
    const staff = Staff.objects.using(db);
    const row = await staff.insert({ name: "Ann", salary: 1, badge: "b", bio: "x" });
    assert.deepEqual(json([row]), [{ salary: 1, name: "Ann" }]);
    assert.ok(!sql.at(-1)!.includes('"badge"') && !sql.at(-1)!.includes('"bio"'));
    const named = await staff.insert({ name: "Bob", salary: 2 }).returning(Staff["badge"]);
    assert.deepEqual(json([named]), [{ badge: null }]);
    assert.ok(sql.at(-1)!.includes('"badge"') && !sql.at(-1)!.includes('"salary"'));
    const everything = await staff.withoutDefaults().insert({ name: "Cid", salary: 3, bio: "z" });
    assert.deepEqual([everything["salary"], everything["badge"], everything["name"], everything["bio"]], [3, null, "Cid", "z"]);
    const many = await staff.insertMany([{ name: "Dee", salary: 4 }, { name: "Eve", salary: 5 }]).returning();
    assert.deepEqual(json(many), [{ salary: 4, name: "Dee" }, { salary: 5, name: "Eve" }]);
    const ann = () => staff.filter(Staff["name"]!.eq("Ann"));
    assert.deepEqual(json(await ann().update({ salary: 9 }).returning()), [{ salary: 9, name: "Ann" }]);
    assert.deepEqual(json(await ann().update({ salary: 10 }).returning(Staff["badge"])), [{ badge: "b" }]);
    assert.deepEqual(json(await staff.filter(Staff["name"]!.eq("Dee")).delete().returning()), [{ salary: 4, name: "Dee" }]);
    assert.equal(await staff.withoutDefaults().count(), 4);
  } finally { await db.close(); }
});
