/** A default filter is a schema expression: methods, a to-one path and a typed scope on both databases; a string filter still works. */
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { connect, load, loads, QueryError, Registry, scope } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

const source = `
model FxOwner {
  id     Int        @id
  region String
  items  FxNative[]
  @@map("fx_owner")
}

model FxNative {
  id      Int     @id
  ownerId Int
  title   String
  rank    Int
  owner   FxOwner @relation(fields: [ownerId], references: [id])
  @@map("fx_native")
  @@query.filter("window", rank.between(2, 9) & title.startswith("t"))
  @@query.filter("north", owner.region == "north")
}

model FxScoped {
  id    Int    @id
  title String
  shop  Int
  @@map("fx_scoped")
  @@query.filter("tenant", shop == scope.shop)
}

model FxLegacy {
  id    Int    @id
  title String
  rank  Int
  shop  Int
  @@map("fx_legacy")
  @@query.filter("legacy", "rank > 1 and shop == 1")
}
`;

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any
const skip = !hasExtension("monorm.query-defaults");
const ROWS = [
  { id: 1, title: "tea", rank: 1, shop: 1 },
  { id: 2, title: "toast", rank: 3, shop: 1 },
  { id: 3, title: "tart", rank: 4, shop: 2 },
  { id: 4, title: "bun", rank: 5, shop: 1 },
  { id: 5, title: "tin", rank: 12, shop: 1 },
];
const OWNERS: Record<number, number> = { 1: 1, 2: 1, 3: 2, 4: 1, 5: 1 };
const ids = (rows: Any[]) => rows.map((r) => r.id);

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : (process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test");

  async function setup() {
    const registry = new Registry();
    const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${source}`, { registry }) as Record<string, Any>;
    const db = await connect(url, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    await models["FxOwner"].objects.using(db).insertMany([{ id: 1, region: "north" }, { id: 2, region: "south" }]);
    await models["FxNative"].objects.using(db).insertMany(ROWS.map(({ shop: _shop, ...r }) => ({ ...r, ownerId: OWNERS[r.id] })));
    await models["FxScoped"].objects.using(db).insertMany(ROWS.map(({ rank: _rank, ...r }) => r));
    await models["FxLegacy"].objects.using(db).insertMany(ROWS);
    return { db, models };
  }

  test(`method and to-one path filters run on both databases (${provider})`, { skip }, async () => {
    const { db, models } = await setup();
    try {
      const Native = models["FxNative"];
      assert.deepEqual(ids(await Native.objects.using(db).orderBy("id")), [2]);
      assert.deepEqual(ids(await Native.objects.using(db).withoutDefaults("north").orderBy("id")), [2, 3]);
      assert.deepEqual(ids(await Native.objects.using(db).withoutDefaults("window").orderBy("id")), [1, 2, 4, 5]);
      assert.equal(await Native.objects.using(db).count(), 1);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`a scope value in a filter takes the type of its field (${provider})`, { skip }, async () => {
    const { db, models } = await setup();
    try {
      const Scoped = models["FxScoped"];
      assert.deepEqual(await scope({ shop: 1 }, async () => ids(await Scoped.objects.using(db).orderBy("id"))), [1, 2, 4, 5]);
      assert.deepEqual(await scope({ shop: 2 }, async () => ids(await Scoped.objects.using(db).orderBy("id"))), [3]);
      await assert.rejects(async () => Scoped.objects.using(db).count(), (e: Error) => e instanceof QueryError && /scope\.shop/.test(e.message));
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`a filter written as a string still filters (${provider})`, { skip }, async () => {
    const { db, models } = await setup();
    try {
      const Legacy = models["FxLegacy"];
      assert.deepEqual(ids(await Legacy.objects.using(db).orderBy("id")), [2, 4, 5]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}

const warned = `datasource db {
  provider = "sqlite"
}

model FxWarned {
  id   Int @id
  rank Int
  @@query.filter("big", "rank > 1")
}
`;

test("a filter written as a string warns with its position and the native form", { skip }, async () => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-filter-warning-"));
  const seen: Error[] = [];
  const listener = (warning: Error) => {
    if (warning.name === "SchemaWarning") seen.push(warning);
  };
  process.on("warning", listener);
  try {
    const file = join(dir, "schema.prisma");
    writeFileSync(file, warned);
    load(file, { registry: new Registry() });
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(seen.length, 1);
    assert.match(seen[0]?.message ?? "", /schema\.prisma:8:\d+: warning: this filter is a string; write the expression: rank > 1/);
  } finally {
    process.off("warning", listener);
    rmSync(dir, { recursive: true });
  }
});

const shapes = `datasource db {
  provider = "sqlite"
}

model FxBase {
  id     Int     @id
  active Boolean
  @@inherit.abstract
  @@query.filter("on", active == true)
}

model FxOne {
  @@inherit.extends(["FxBase"])
  @@map("fx_one")
}

model FxTwo {
  note String
  @@inherit.extends(["FxBase"])
  @@map("fx_two")
}

model FxStamped {
  id   Int    @id
  name String
  @@timestamps.updated_at("edited", mode: "application")
  @@query.filter("touched", edited != null)
  @@map("fx_stamped")
}

model FxMember {
  id     Int     @id
  active Boolean
  legacy Int?
  @@map("fx_member")
}

model FxProxy {
  @@proxy.of(FxMember)
  @@proxy.fields(exclude: ["legacy"])
  @@query.filter("on", active == true)
}

model FxPerson {
  id     Int     @id
  active Boolean
  @@map("fx_person")
}

model FxStaff {
  salary Int
  @@composition.model(parent: "FxPerson", parentRef: "person", childRef: "staff")
  @@query.filter("on", active == true & salary > 1)
  @@map("fx_staff")
}
`;
const needsShapes = !hasExtension("monorm.query-defaults", "monorm.inherit", "monorm.timestamps", "monorm.proxy", "monorm.composition");

test("a filter resolves on the final shape of every kind of model", { skip: needsShapes }, async () => {
  const registry = new Registry();
  const models = loads(shapes, { registry }) as Record<string, Any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  try {
    await models["FxOne"].objects.using(db).insertMany([{ id: 1, active: true }, { id: 2, active: false }]);
    await models["FxTwo"].objects.using(db).insertMany([{ id: 1, active: false, note: "n" }, { id: 2, active: true, note: "n" }, { id: 3, active: true, note: "m" }]);
    assert.deepEqual(ids(await models["FxOne"].objects.using(db).orderBy("id")), [1]);
    assert.deepEqual(ids(await models["FxTwo"].objects.using(db).orderBy("id")), [2, 3]);
    const stamped = models["FxStamped"].objects.using(db);
    await stamped.insertMany([{ id: 1, name: "a" }, { id: 2, name: "b" }]);
    assert.deepEqual(ids(await stamped.orderBy("id")), [1, 2]);
    await models["FxMember"].objects.using(db).insertMany([{ id: 1, active: true, legacy: 0 }, { id: 2, active: false, legacy: 0 }]);
    assert.deepEqual(ids(await models["FxProxy"].objects.using(db).orderBy("id")), [1]);
    await models["FxStaff"].objects.using(db).insertMany([{ id: 1, active: true, salary: 5 }, { id: 2, active: false, salary: 5 }, { id: 3, active: true, salary: 0 }]);
    assert.deepEqual(ids(await models["FxStaff"].objects.using(db).orderBy("id")), [1]);
  } finally {
    await db.close();
  }
});

test("a proxy filter that names an omitted field fails with its position", { skip: needsShapes }, () => {
  const source = shapes.replace('@@query.filter("on", active == true)\n}\n\nmodel FxPerson', '@@query.filter("on", legacy == 0)\n}\n\nmodel FxPerson');
  assert.notEqual(source, shapes);
  assert.throws(() => loads(source, { registry: new Registry() }), /\d+:\d+: @@query\.filter: unknown field `legacy` on model `FxProxy`/);
});

const parent = `datasource db {
  provider = "sqlite"
}

model FxParent {
  id     Int     @id
  active Boolean
  rank   Int
  @@query.filter("live", active == true)
}

model FxChild {
  id     Int     @id
  active Boolean
  rank   Int
  @@query.parent("FxParent")
  @@query.filter("live", parent.default_filter & rank > 1)
}
`;

test("a child filter extends the filter of its parent", { skip }, async () => {
  const registry = new Registry();
  const models = loads(parent, { registry }) as Record<string, Any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  try {
    const Child = models["FxChild"];
    await Child.objects.using(db).insertMany([{ id: 1, active: true, rank: 1 }, { id: 2, active: true, rank: 2 }, { id: 3, active: false, rank: 3 }]);
    assert.deepEqual(ids(await Child.objects.using(db).orderBy("id")), [2]);
    assert.deepEqual(ids(await Child.objects.using(db).withoutDefaults("live").orderBy("id")), [1, 2, 3]);
    assert.throws(() => loads(parent.replace('@@query.parent("FxParent")', ""), { registry: new Registry() }), /parent\.default_filter has no inherited filter/);
  } finally {
    await db.close();
  }
});
