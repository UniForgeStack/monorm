/** Owning relations (`owns: true`): the database deletes an owned row with its last owner, on PostgreSQL and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry } from "../src/index.js";

const source = `
model Spec {
  id      Int      @id
  body    String
  guitars Guitar[]
  photos  Photo[]
  @@map("ow_node_spec")
}

model Guitar {
  id     Int   @id
  specId Int?  @map("spec_id")
  spec   Spec? @relation(fields: [specId], references: [id], onDelete: NoAction, owns: true)
  @@map("ow_node_guitar")
}

model Photo {
  id     Int  @id
  specId Int  @map("spec_id")
  spec   Spec @relation(fields: [specId], references: [id])
  @@map("ow_node_photo")
}

model Order {
  id       Int    @id
  basketId Int    @map("basket_id")
  basket   Basket @relation(fields: [basketId], references: [id], onDelete: NoAction, owns: true)
  @@map("ow_node_order")
}

model Basket {
  id       Int     @id
  orders   Order[]
  detailId Int     @map("detail_id")
  detail   Detail  @relation(fields: [detailId], references: [id], onDelete: NoAction, owns: true)
  @@map("ow_node_basket")
}

model Detail {
  id      Int      @id
  baskets Basket[]
  @@map("ow_node_detail")
}
`;

const cycle = `
model Left {
  id      Int     @id
  rightId Int?    @map("right_id")
  right   Right?  @relation("LeftOwnsRight", fields: [rightId], references: [id], onDelete: NoAction, owns: true)
  lefts   Right[] @relation("RightOwnsLeft")
  @@map("ow_node_left")
}

model Right {
  id     Int    @id
  leftId Int?   @map("left_id")
  left   Left?  @relation("RightOwnsLeft", fields: [leftId], references: [id], onDelete: NoAction, owns: true)
  rights Left[] @relation("LeftOwnsRight")
  @@map("ow_node_right")
}
`;

const longCycle = `
model Ring {
  id     Int     @id
  nextId Int?    @map("next_id")
  next   Ring2?  @relation("RingNext", fields: [nextId], references: [id], onDelete: NoAction, owns: true)
  prevs  Ring2[] @relation("Ring2Next")
  @@map("ow_node_ring")
}

model Ring2 {
  id     Int    @id
  nextId Int?   @map("next_id")
  next   Ring?  @relation("Ring2Next", fields: [nextId], references: [id], onDelete: NoAction, owns: true)
  prevs  Ring[] @relation("RingNext")
  @@map("ow_node_ring2")
}
`;

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : (process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test");

  async function setup(): Promise<{ db: any; m: any }> {
    const registry = new Registry();
    const m = loads(`datasource db {\n  provider = "${provider}"\n}\n${source}`, { registry });
    const db = await connect(url!, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    return { db, m };
  }

  const ids = async (db: any, table: string): Promise<number[]> =>
    (await db.fetch(`SELECT id FROM ${table} ORDER BY id`)).map((r: any) => Number(r.id));

  async function run(name: string, body: (db: any, m: any) => Promise<void>): Promise<void> {
    test(`${name} (${provider})`, { skip: !url }, async () => {
      const { db, m } = await setup();
      try {
        await body(db, m);
      } finally {
        await db.dropTables();
        await db.close();
      }
    });
  }

  await run("an owned row goes with its only owner", async (db, m) => {
    await m.Spec.objects.using(db).insertMany([{ id: 1, body: "a" }, { id: 2, body: "b" }]);
    await m.Guitar.objects.using(db).insertMany([{ id: 1, specId: 1 }, { id: 2, specId: 2 }]);
    await m.Guitar.objects.using(db).filter(m.Guitar.id.eq(1)).delete();
    assert.deepEqual(await ids(db, "ow_node_spec"), [2]);
    assert.deepEqual(await ids(db, "ow_node_guitar"), [2]);
  });

  await run("a shared owned row stays until its last owner goes", async (db, m) => {
    const Guitar = m.Guitar;
    await m.Spec.objects.using(db).insertMany([{ id: 1, body: "a" }]);
    await Guitar.objects.using(db).insertMany([{ id: 1, specId: 1 }, { id: 2, specId: 1 }]);
    await Guitar.objects.using(db).filter(Guitar.id.eq(1)).delete();
    assert.deepEqual(await ids(db, "ow_node_spec"), [1]);
    await Guitar.objects.using(db).filter(Guitar.id.eq(2)).delete();
    assert.deepEqual(await ids(db, "ow_node_spec"), []);
  });

  await run("all owners deleted in one statement delete the shared row", async (db, m) => {
    const Guitar = m.Guitar;
    await m.Spec.objects.using(db).insertMany([{ id: 1, body: "a" }, { id: 2, body: "b" }]);
    await Guitar.objects.using(db).insertMany([{ id: 1, specId: 1 }, { id: 2, specId: 1 }, { id: 3, specId: 2 }]);
    assert.equal(await Guitar.objects.using(db).filter(Guitar.id.lt(3)).delete(), 2);
    assert.deepEqual(await ids(db, "ow_node_spec"), [2]);
  });

  await run("another foreign key keeps the owned row", async (db, m) => {
    const Guitar = m.Guitar;
    await m.Spec.objects.using(db).insertMany([{ id: 1, body: "a" }]);
    await m.Photo.objects.using(db).insertMany([{ id: 1, specId: 1 }]);
    await Guitar.objects.using(db).insertMany([{ id: 1, specId: 1 }]);
    await Guitar.objects.using(db).filter(Guitar.id.eq(1)).delete();
    assert.deepEqual(await ids(db, "ow_node_spec"), [1]);
    await db.execute("DELETE FROM ow_node_photo");
    await Guitar.objects.using(db).insertMany([{ id: 2, specId: 1 }]);
    await Guitar.objects.using(db).filter(Guitar.id.eq(2)).delete();
    assert.deepEqual(await ids(db, "ow_node_spec"), []);
  });

  await run("a null owning key deletes nothing", async (db, m) => {
    await m.Spec.objects.using(db).insertMany([{ id: 1, body: "a" }]);
    await m.Guitar.objects.using(db).insertMany([{ id: 1, specId: null }]);
    await m.Guitar.objects.using(db).filter(m.Guitar.id.eq(1)).delete();
    assert.deepEqual(await ids(db, "ow_node_spec"), [1]);
  });

  await run("a two level chain deletes every level", async (db, m) => {
    await m.Detail.objects.using(db).insertMany([{ id: 1 }]);
    await m.Basket.objects.using(db).insertMany([{ id: 1, detailId: 1 }]);
    await m.Order.objects.using(db).insertMany([{ id: 1, basketId: 1 }]);
    await m.Order.objects.using(db).filter(m.Order.id.eq(1)).delete();
    assert.deepEqual(await ids(db, "ow_node_basket"), []);
    assert.deepEqual(await ids(db, "ow_node_detail"), []);
  });

  if (provider === "sqlite") {
    test("an owning cycle is refused on sqlite", () => {
      for (const schema of [cycle, longCycle]) {
        assert.throws(() => loads(`datasource db {\n  provider = "sqlite"\n}\n${schema}`, { registry: new Registry() }), /owning cycle/);
      }
    });
  } else {
    test(`a two model cycle deletes both rows (${provider})`, async () => {
      const registry = new Registry();
      const m: any = loads(cycle, { registry });
      const db = await connect(url!, { registry, default: false });
      try {
        await db.dropTables();
        await db.createTables();
        await m.Left.objects.using(db).insertMany([{ id: 1, rightId: null }]);
        await m.Right.objects.using(db).insertMany([{ id: 1, leftId: 1 }]);
        await db.execute("UPDATE ow_node_left SET right_id = 1 WHERE id = 1");
        await m.Left.objects.using(db).filter(m.Left.id.eq(1)).delete();
        assert.deepEqual(await ids(db, "ow_node_left"), []);
        assert.deepEqual(await ids(db, "ow_node_right"), []);
      } finally {
        await db.dropTables();
        await db.close();
      }
    });

    await run("two concurrent deletes of the owners delete the shared row", async (db, m) => {
      const other = await connect(url!, { default: false });
      try {
        await m.Spec.objects.using(db).insertMany([{ id: 1, body: "a" }]);
        await m.Guitar.objects.using(db).insertMany([{ id: 1, specId: 1 }, { id: 2, specId: 1 }]);
        let second: Promise<void> | undefined;
        let done = false;
        await db.transaction(async () => {
          await db.execute("DELETE FROM ow_node_guitar WHERE id = 1");
          second = other.transaction(async () => {
            await other.execute("DELETE FROM ow_node_guitar WHERE id = 2");
          }).then(() => { done = true; });
          await new Promise((r) => setTimeout(r, 500));
          assert.equal(done, false, "the second delete must wait for the lock on the shared row");
        });
        await second;
        assert.deepEqual(await ids(db, "ow_node_spec"), []);
      } finally {
        await other.close();
      }
    });
  }

  await run("a raw sql delete gives the same result as the orm call", async (db, m) => {
    const Guitar = m.Guitar;
    await m.Spec.objects.using(db).insertMany([{ id: 1, body: "a" }, { id: 2, body: "b" }]);
    await Guitar.objects.using(db).insertMany([{ id: 1, specId: 1 }, { id: 2, specId: 1 }, { id: 3, specId: 2 }]);
    await db.execute("DELETE FROM ow_node_guitar WHERE id = 1");
    assert.deepEqual(await ids(db, "ow_node_spec"), [1, 2]);
    await db.execute("DELETE FROM ow_node_guitar WHERE id IN (2, 3)");
    assert.deepEqual(await ids(db, "ow_node_spec"), []);
  });
}
