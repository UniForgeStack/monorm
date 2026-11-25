/** updateMany() takes expressions over the model's own columns, and updateValues takes a fixed expression map. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, excluded, func, loads, or, and, param, Registry } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const schema = `
model Author {
  id Int @id
  name String @default("")
  items Item[]
  @@map("ume_author")
}
model Item {
  id Int @id
  author_id Int
  author Author @relation(fields: [author_id], references: [id])
  title String @default("")
  views Int @default(0)
  revenue Int @default(0)
  at_end Int?
  read_at DateTime?
  frozen Boolean @default(false)
  @@map("ume_item")
}
`;
const AT = new Date("2026-01-01T00:00:00Z");

async function setup(provider: string, disable: string[] = []) {
  const registry = new Registry();
  const models = loads(`datasource db {\n provider = "${provider}"\n}\n${schema}`, { registry });
  const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false, disable });
  await db.dropTables();
  await db.createTables();
  const Item: any = models["Item"];
  const items: any = Item.objects.using(db);
  await (models["Author"] as any).objects.using(db).insertMany([{ id: 1, name: "A" }, { id: 2, name: "B" }, { id: 9 }]);
  await items.insertMany([
    { id: 1, authorId: 1, title: "a", views: 5, revenue: 10 },
    { id: 2, authorId: 1, title: "b", views: 7, revenue: 20 },
    { id: 3, authorId: 2, title: "c", views: 1, revenue: 30, frozen: true },
  ]);
  const sqlOf = async (statement: any): Promise<[any, string[]]> => {
    const sql: string[] = [];
    const remove = db.onQuery((e: any) => { if (e.sql.trimStart().toUpperCase().startsWith("UPDATE")) sql.push(e.sql); });
    try {
      return [await statement, sql];
    } finally {
      remove();
    }
  };
  const views = async () => Object.fromEntries((await items.orderBy(Item.id).all()).map((i: any) => [i.id, i.views]));
  return { db, Item, items, sqlOf, views };
}

for (const provider of ["sqlite", "postgresql"]) {
  test(`update many adds to a column in one statement (${provider})`, async () => {
    const { db, Item, items, sqlOf, views } = await setup(provider);
    try {
      const rows = [{ id: 1, views: Item.views.add(1) }, { id: 2, views: Item.views.add(5) }];
      const [count, sql] = await sqlOf(items.updateMany(rows));
      assert.equal(count, 2);
      assert.ok(sql.length === 1 && sql[0]!.includes("VALUES") && sql[0]!.includes("views"), sql.join("\n"));
      assert.deepEqual(await views(), { 1: 6, 2: 12, 3: 1 });
    } finally {
      await db.close();
    }
  });

  test(`update many runs one statement per shape (${provider})`, async () => {
    const { db, Item, items, sqlOf, views } = await setup(provider);
    try {
      const rows = [{ id: 1, views: Item.views.add(1) }, { id: 2, views: Item.views.mul(2) }, { id: 3, views: Item.views.add(10) }];
      const [count, sql] = await sqlOf(items.updateMany(rows));
      assert.equal(count, 3);
      assert.equal(sql.length, 2);
      assert.deepEqual(await views(), { 1: 6, 2: 14, 3: 11 });
    } finally {
      await db.close();
    }
  });

  test(`update many mixes plain and expression cells in a column (${provider})`, async () => {
    const { db, Item, items, sqlOf } = await setup(provider);
    try {
      const rows = [{ id: 1, title: "x", views: Item.views.add(1) }, { id: 2, title: "y", views: 100 }];
      const [count, sql] = await sqlOf(items.updateMany(rows));
      assert.equal(count, 2);
      assert.equal(sql.length, 2);
      const got = Object.fromEntries((await items.orderBy(Item.id).all()).map((i: any) => [i.id, [i.title, i.views]]));
      assert.deepEqual(got, { 1: ["x", 6], 2: ["y", 100], 3: ["c", 1] });
    } finally {
      await db.close();
    }
  });

  test(`update many reads case and coalesce in a row (${provider})`, async () => {
    const { db, Item, items } = await setup(provider);
    try {
      await items.updateMany([
        { id: 1, atEnd: func.coalesce(Item.atEnd, Item.views) },
        { id: 2, atEnd: func.coalesce(Item.atEnd, Item.views) },
      ]);
      await items.updateMany([
        { id: 1, views: func.case([Item.views.gt(100), 1], { default: Item.views.add(1) }) },
        { id: 3, views: 9 },
      ]);
      const got = Object.fromEntries((await items.orderBy(Item.id).all()).map((i: any) => [i.id, [i.views, i.atEnd]]));
      assert.deepEqual(got, { 1: [6, 5], 2: [7, 7], 3: [9, null] });
    } finally {
      await db.close();
    }
  });

  test(`update many applies the filters of the query set (${provider})`, async () => {
    const { db, Item, items, views } = await setup(provider);
    try {
      const count = await items.filter(Item.frozen.eq(false)).updateMany([
        { id: 1, views: Item.views.add(1) },
        { id: 3, views: Item.views.add(1) },
      ]);
      assert.equal(count, 1);
      assert.deepEqual(await views(), { 1: 6, 2: 7, 3: 1 });
    } finally {
      await db.close();
    }
  });

  test(`update many falls back to case with an expression (${provider})`, async () => {
    const { db, Item, items, sqlOf } = await setup(provider, ["update_from_values"]);
    try {
      const [count, sql] = await sqlOf(items.updateMany([{ id: 1, views: Item.views.add(1) }, { id: 2, views: Item.views.add(2) }]));
      assert.equal(count, 2);
      assert.ok(sql[0]!.includes("CASE") && !sql[0]!.includes("VALUES"), sql[0]);
      assert.deepEqual((await items.orderBy(Item.id).all()).map((i: any) => i.views), [6, 9, 1]);
    } finally {
      await db.close();
    }
  });

  test(`update many update values read excluded (${provider})`, async () => {
    const { db, Item, items, sqlOf, views } = await setup(provider);
    try {
      const rows = [{ id: 1, views: 3 }, { id: 2, views: 4 }];
      const [count, sql] = await sqlOf(items.updateMany(rows, { updateValues: { views: Item.views.add(excluded(Item.views)) } }));
      assert.equal(count, 2);
      assert.ok(sql.length === 1 && sql[0]!.includes("VALUES"), sql.join("\n"));
      assert.deepEqual(await views(), { 1: 8, 2: 11, 3: 1 });
      // A field in the rows and in updateValues: updateValues wins, and the rows still name the VALUES column.
      await items.updateMany([{ id: 1, views: 100 }, { id: 2, views: 200 }], { updateValues: { views: excluded(Item.views).add(1) } });
      assert.deepEqual(await views(), { 1: 101, 2: 201, 3: 1 });
      // A field only in updateValues gets a fixed value for every row.
      await items.updateMany([{ id: 1, views: 0 }, { id: 2, views: 0 }], { updateValues: { title: "same" } });
      assert.deepEqual((await items.orderBy(Item.id).all()).map((i: any) => i.title), ["same", "same", "c"]);
    } finally {
      await db.close();
    }
  });

  test(`a prepared update many reads excluded through unnest (${provider})`, async () => {
    const { db, Item, items, views } = await setup(provider);
    try {
      await items.insertMany(Array.from({ length: 40 }, (_, k) => ({ id: k + 10, authorId: 9, views: k + 10 })));
      const prepared = items.updateMany(param("rows", { fields: [Item.id, Item.views] }), { updateValues: { views: Item.views.add(excluded(Item.views)) } }).prepare();
      assert.equal(await prepared.run({ rows: [{ id: 1, views: 3 }, { id: 2, views: 4 }] }), 2);
      assert.equal(await prepared.run({ rows: Array.from({ length: 40 }, (_, k) => ({ id: k + 10, views: 1000 })) }), 40);
      const got: any = await views();
      assert.ok(got[1] === 8 && got[2] === 11 && got[10] === 1010 && got[49] === 1049, JSON.stringify(got));
      await assert.rejects(async () => { await prepared.run({ rows: [{ id: 1, views: Item.views.add(1) }] }); }, /a prepared updateMany takes plain values/);
    } finally {
      await db.close();
    }
  });

  test(`update many returns the rows of the revenue case (${provider})`, async () => {
    const { db, Item, items } = await setup(provider);
    try {
      const revenue = excluded(Item.revenue);
      const readFirst = or(Item.readAt.isNull(), Item.readAt.gt(AT));
      const atEnd = func.case(
        [and(Item.atEnd.isNull(), Item.frozen.eq(false)), func.case([readFirst, revenue], { default: Item.revenue })],
        { default: Item.atEnd },
      );
      const rows = await items.filter(Item.frozen.eq(false))
        .updateMany([{ id: 1, revenue: 111 }, { id: 2, revenue: 222 }, { id: 3, revenue: 333 }], { updateValues: { atEnd, readAt: AT } })
        .returning(Item.id, Item.atEnd);
      assert.deepEqual(Object.fromEntries(rows.map((r: any) => [r.id, r.atEnd])), { 1: 111, 2: 222 });
      // Second call: atEnd is set, so the CASE keeps it.
      const again = await items.updateMany([{ id: 1, revenue: 5 }], { updateValues: { atEnd } }).returning(Item.id, Item.atEnd);
      assert.deepEqual(Object.fromEntries(again.map((r: any) => [r.id, r.atEnd])), { 1: 111 });
    } finally {
      await db.close();
    }
  });

  test(`update many rolls back every group when one fails (${provider})`, async () => {
    const { db, Item, items, views } = await setup(provider);
    try {
      // Two shapes, two statements: the second sets NULL in a NOT NULL column.
      await assert.rejects(async () => { await items.updateMany([{ id: 1, views: Item.views.add(1) }, { id: 2, views: Item.atEnd }]); });
      assert.deepEqual(await views(), { 1: 5, 2: 7, 3: 1 });
    } finally {
      await db.close();
    }
  });

  test(`update many expression errors (${provider})`, async () => {
    const { db, Item, items, views } = await setup(provider);
    try {
      const fails = async (run: () => unknown, pattern: RegExp) => assert.rejects(async () => { await run(); }, pattern);
      await fails(() => items.updateMany([{ id: Item.views.add(1), views: 1 }]), /primary key must be a plain value/);
      await fails(() => items.updateMany([{ id: 1, views: Item.author.id }]), /relation path/);
      await fails(() => items.updateMany([{ id: 1, views: excluded(Item.views) }]), /excluded/);
      await fails(() => items.updateMany([{ id: 1, views: param("x") }]), /param/);
      await fails(() => items.updateMany([{ id: 1, title: "x" }], { updateValues: { views: excluded(Item.views) } }), /needs views in the rows/);
      await fails(() => items.updateMany([{ id: 1, views: Item.views.add(1) }], { updateValues: { views: 3 } }), /expression in a row and also in update_values/);
      await fails(() => items.updateMany([{ id: 1, views: Item.views.add(1) }, { id: 1, views: Item.views.mul(2) }]), /appears twice/);
      await fails(() => items.updateMany([{ id: 1, views: 3 }], { updateValues: { id: 5 } }), /update_values cannot set id/);
      assert.deepEqual(await views(), { 1: 5, 2: 7, 3: 1 });
    } finally {
      await db.close();
    }
  });
}

const pairs = `
model Pair {
  a Int
  b Int
  views Int @default(0)
  @@id([a, b])
  @@map("ume_pair")
}
`;

for (const provider of ["sqlite", "postgresql"]) {
  test(`update many takes expressions on a composite key (${provider})`, async () => {
    const registry = new Registry();
    const Pair: any = loads(`datasource db {\n provider = "${provider}"\n}\n${pairs}`, { registry })["Pair"];
    const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    try {
      const rows: any = Pair.objects.using(db);
      await rows.insertMany([{ a: 1, b: 1, views: 1 }, { a: 1, b: 2, views: 2 }, { a: 2, b: 1, views: 3 }]);
      assert.equal(await rows.updateMany([{ a: 1, b: 2, views: Pair.views.add(10) }, { a: 2, b: 1, views: Pair.views.mul(2) }]), 2);
      assert.equal(await rows.updateMany([{ a: 1, b: 1, views: 7 }], { updateValues: { views: Pair.views.add(excluded(Pair.views)) } }), 1);
      const got = Object.fromEntries((await rows.all()).map((p: any) => [`${p.a},${p.b}`, p.views]));
      assert.deepEqual(got, { "1,1": 8, "1,2": 12, "2,1": 6 });
    } finally {
      await db.close();
    }
  });
}
