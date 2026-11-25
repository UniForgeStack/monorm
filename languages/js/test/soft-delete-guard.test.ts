/** The guard of a soft-delete cascade child: a row under, or moved to, a soft-deleted parent takes the stamp of the parent. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

const SOFT = '@@soft_delete.deleted_at("deleted")';

const SCHEMA = `
model Shelf {
  id    Int    @id
  books Book[]
  memos Memo[]
  ${SOFT}
  @@map("sg_shelf")
}

model Book {
  id      Int   @id
  shelfId Int   @map("shelf_id")
  shelf   Shelf @relation(fields: [shelfId], references: [id], onDelete: Cascade)
  pages   Page[]
  ${SOFT}
  @@map("sg_book")
}

model Page {
  id     Int  @id
  bookId Int  @map("book_id")
  book   Book @relation(fields: [bookId], references: [id], onDelete: Cascade)
  ${SOFT}
  @@map("sg_page")
}

model Memo {
  id      Int   @id
  shelfId Int   @map("shelf_id")
  shelf   Shelf @relation(fields: [shelfId], references: [id], onDelete: Cascade) @soft_delete.keep_child_rows
  ${SOFT}
  @@map("sg_memo")
}
`;

const PG = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const T1 = "'2020-01-01 00:00:00.000'";
const T2 = "'2020-01-02 00:00:00.000'";
const skip = !hasExtension("monorm.soft-delete");

type Env = { db: any; m: any };

async function open(provider: string): Promise<Env> {
  const registry = new Registry();
  const head = provider === "sqlite" ? 'datasource db {\n  provider = "sqlite"\n}\n' : "";
  const m: any = loads(head + SCHEMA, { registry });
  const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : PG, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  await m.Shelf.objects.using(db).insertMany([{ id: 1 }, { id: 2 }]);
  await db.execute(`UPDATE sg_shelf SET deleted = ${T1} WHERE id = 1`);
  return { db, m };
}

const stamp = async (db: any, table: string, key: number): Promise<unknown> =>
  (await db.fetch(`SELECT deleted FROM ${table} WHERE id = ${key}`))[0].deleted;

for (const provider of ["sqlite", "postgresql"]) {
  async function run(name: string, body: (e: Env) => Promise<void>): Promise<void> {
    test(`${name} (${provider})`, { skip }, async () => {
      const e = await open(provider);
      try {
        await body(e);
      } finally {
        await e.db.dropTables();
        await e.db.close();
      }
    });
  }

  await run("a row inserted under a deleted parent takes the stamp and under a live parent none", async ({ db, m }) => {
    await m.Book.objects.using(db).insertMany([{ id: 1, shelfId: 1 }, { id: 2, shelfId: 2 }]);
    assert.deepEqual(await stamp(db, "sg_book", 1), await stamp(db, "sg_shelf", 1));
    assert.notEqual(await stamp(db, "sg_book", 1), null);
    assert.equal(await stamp(db, "sg_book", 2), null);
  });

  await run("a row with its own stamp keeps it", async ({ db }) => {
    await db.execute(`INSERT INTO sg_book (id, shelf_id, deleted) VALUES (1, 1, ${T2})`);
    assert.notEqual(await stamp(db, "sg_book", 1), null);
    assert.notDeepEqual(await stamp(db, "sg_book", 1), await stamp(db, "sg_shelf", 1));
  });

  await run("a live row moved to a deleted parent takes the stamp and to a live parent keeps none", async ({ db, m }) => {
    await m.Book.objects.using(db).insertMany([{ id: 1, shelfId: 2 }, { id: 2, shelfId: 2 }]);
    await db.execute("UPDATE sg_book SET shelf_id = 1 WHERE id = 1");
    assert.deepEqual(await stamp(db, "sg_book", 1), await stamp(db, "sg_shelf", 1));
    await db.execute("UPDATE sg_shelf SET deleted = NULL WHERE id = 2");
    await db.execute("UPDATE sg_book SET shelf_id = 2 WHERE id = 2");
    assert.equal(await stamp(db, "sg_book", 2), null);
  });

  await run("the revive of the parent brings back a row that the guard stamped", async ({ db, m }) => {
    await m.Book.objects.using(db).insertMany([{ id: 1, shelfId: 1 }]);
    await db.execute("UPDATE sg_shelf SET deleted = NULL WHERE id = 1");
    assert.equal(await stamp(db, "sg_book", 1), null);
  });

  await run("a relation with keep_child_rows has no guard", async ({ db, m }) => {
    await m.Memo.objects.using(db).insertMany([{ id: 1, shelfId: 1 }]);
    assert.equal(await stamp(db, "sg_memo", 1), null);
  });

  await run("a live row moved to a deleted parent stamps its own children", async ({ db, m }) => {
    await m.Book.objects.using(db).insertMany([{ id: 1, shelfId: 2 }]);
    await m.Page.objects.using(db).insertMany([{ id: 1, bookId: 1 }]);
    await db.execute("UPDATE sg_book SET shelf_id = 1 WHERE id = 1");
    assert.deepEqual(await stamp(db, "sg_page", 1), await stamp(db, "sg_shelf", 1));
    assert.notEqual(await stamp(db, "sg_page", 1), null);
    await db.execute("UPDATE sg_shelf SET deleted = NULL WHERE id = 1");
    assert.equal(await stamp(db, "sg_book", 1), null);
    assert.equal(await stamp(db, "sg_page", 1), null);
  });
}
