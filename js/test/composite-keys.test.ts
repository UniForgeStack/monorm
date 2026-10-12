/** Composite primary keys (`@@id`) and composite foreign keys on Postgres and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, describe as info, loads, Registry } from "../src/index.js";

const source = `
model Item {
  shop  Int
  id    Int
  title String
  lines Line[]
  @@id([shop, id])
  @@map("ck_node_item")
}

model Line {
  id     Int  @id @default(autoincrement())
  shop   Int
  itemId Int
  qty    Int
  item   Item @relation(fields: [shop, itemId], references: [shop, id])
  @@map("ck_node_line")
}
`;

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"];

  async function setup(): Promise<{ db: any; Item: any; Line: any }> {
    const registry = new Registry();
    const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${source}`, { registry });
    const db = await connect(url!, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    return { db, Item: models["Item"], Line: models["Line"] };
  }

  test(`crud by the whole key (${provider})`, { skip: !url }, async () => {
    const { db, Item } = await setup();
    try {
      const items = Item.objects.using(db);
      const rows = await items.insertMany([{ shop: 1, id: 1, title: "a" }, { shop: 2, id: 1, title: "b" }, { shop: 1, id: 2, title: "c" }]).returning();
      assert.deepEqual(rows.map((i: any) => i.pk), [{ shop: 1, id: 1 }, { shop: 2, id: 1 }, { shop: 1, id: 2 }]);
      assert.deepEqual(info(Item).primaryKey, ["shop", "id"]);

      const b = await items.require().get(Item.shop.eq(2), Item.id.eq(1));
      assert.equal(b.title, "b");
      assert.equal(await items.get(Item.shop.eq(3), Item.id.eq(1)), null);

      await b.update({ title: "B" });
      assert.deepEqual((await items.orderBy(Item.shop, Item.id).all()).map((i: any) => i.title), ["a", "c", "B"]);
      await db.execute("UPDATE ck_node_item SET title = 'bb' WHERE shop = 2 AND id = 1");
      assert.equal(await b.refresh(), true);
      assert.equal(b.title, "bb");

      await items.updateMany([{ shop: 1, id: 1, title: "x" }, { shop: 1, id: 2, title: "y" }]);
      assert.deepEqual((await items.orderBy(Item.shop, Item.id).all()).map((i: any) => i.title), ["x", "y", "bb"]);
      await assert.rejects(items.inBulk([1]), /several fields/);

      await b.delete();
      assert.equal(await items.count(), 2);
      assert.equal(await items.get(Item.shop.eq(2), Item.id.eq(1)), null);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`pagination and batches walk the whole key (${provider})`, { skip: !url }, async () => {
    const { db, Item } = await setup();
    try {
      const items = Item.objects.using(db);
      await items.insertMany([1, 2].flatMap((shop) => [1, 2, 3].map((id) => ({ shop, id, title: `${shop}.${id}` }))));
      const titles: string[] = [];
      let cursor: string | null = null;
      for (;;) {
        const page: any = await items.paginate({ first: 4, after: cursor });
        titles.push(...page.items.map((i: any) => i.title));
        if (!page.hasNext) break;
        cursor = page.nextCursor;
      }
      assert.deepEqual(titles, ["1.1", "1.2", "1.3", "2.1", "2.2", "2.3"]);
      assert.deepEqual((await items.paginate({ last: 2 })).items.map((i: any) => i.title), ["2.2", "2.3"]);
      const batches: string[][] = [];
      for await (const batch of items.batches(4)) batches.push(batch.map((i: any) => i.title));
      assert.deepEqual(batches, [["1.1", "1.2", "1.3", "2.1"], ["2.2", "2.3"]]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`relations through a composite foreign key (${provider})`, { skip: !url }, async () => {
    const { db, Item, Line } = await setup();
    try {
      const [a, b] = await Item.objects.using(db).insertMany([{ shop: 1, id: 1, title: "a" }, { shop: 2, id: 1, title: "b" }]).returning();
      const lines = Line.objects.using(db);
      await lines.insertMany([{ item: a, qty: 1 }, { item: a, qty: 2 }, { shop: 2, itemId: 1, qty: 3 }]);
      const joined = await lines.load(Line.item).orderBy(Line.id).all();
      assert.deepEqual(joined.map((l: any) => [l.item.title, l.qty]), [["a", 1], ["a", 2], ["b", 3]]);
      const prefetched = await lines.load(Line.item.objects.filter(Item.title.gte("a"))).orderBy(Line.id).all();
      assert.deepEqual(prefetched.map((l: any) => l.item.title), ["a", "a", "b"]);

      const loaded = await Item.objects.using(db).load(Item.lines).orderBy(Item.shop).all();
      assert.deepEqual(loaded.map((i: any) => i.lines.cached.map((l: any) => l.qty)), [[1, 2], [3]]);
      assert.deepEqual((await a.lines.using(db).orderBy(Line.qty).all()).map((l: any) => l.qty), [1, 2]);
      await b.lines.using(db).insert({ qty: 4 });
      assert.deepEqual((await b.lines.using(db).orderBy(Line.qty).all()).map((l: any) => l.qty), [3, 4]);

      assert.equal(await lines.filter(Line.item.eq(a)).count(), 2);
      assert.equal(await lines.filter(Line.item.title.eq("b")).count(), 2);
      assert.equal(await Item.objects.using(db).filter(Item.lines.qty.gt(3)).count(), 1);

      const line = await lines.require().get(Line.id.eq(joined[0].id));
      await line.update({ item: b });
      assert.deepEqual([line.shop, line.itemId], [2, 1]);
      await line.load(Line.item);
      assert.equal(line.item.title, "b");
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}
