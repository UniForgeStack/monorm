/** Owning relations with soft delete: the sweep, the owned revive and `sweepOwned()`, on PostgreSQL and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry, skipTriggers } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

const SOFT = '@@soft_delete.deleted_at("deleted")';

const SCHEMA = `
model Spec {
  id      Int      @id
  body    String
  guitars Guitar[]
  banjos  Banjo[]
  frames  Frame[]
  ${SOFT}
  @@map("os_spec")
}

model Guitar {
  id      Int   @id
  specId Int? @map("spec_id")
  spec    Spec? @relation(fields: [specId], references: [id], onDelete: NoAction, owns: true)
  ${SOFT}
  @@map("os_guitar")
}

model Banjo {
  id      Int   @id
  specId Int? @map("spec_id")
  spec    Spec? @relation(fields: [specId], references: [id], onDelete: NoAction, owns: true)
  ${SOFT}
  @@map("os_banjo")
}

model Frame {
  id      Int   @id
  specId Int? @map("spec_id")
  spec    Spec? @relation(fields: [specId], references: [id], onDelete: NoAction, owns: true)
  @@map("os_frame")
}
`;

// The target comes first, so one pass of `sweepOwned({ repair: true })` meets it before its owner.
const CHAIN = `
model Detail {
  id      Int      @id
  baskets Basket[]
  ${SOFT}
  @@map("os_detail")
}

model Basket {
  id        Int     @id
  orders    Order[]
  detailId Int @map("detail_id")
  detail    Detail  @relation(fields: [detailId], references: [id], onDelete: NoAction, owns: true)
  ${SOFT}
  @@map("os_basket")
}

model Order {
  id        Int    @id
  basketId Int @map("basket_id")
  basket    Basket @relation(fields: [basketId], references: [id], onDelete: NoAction, owns: true)
  ${SOFT}
  @@map("os_order")
}
`;

const APP = `
model AppSpec {
  id     Int        @id
  owners AppOwner[]
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("os_app_spec")
}

model AppOwner {
  id      Int      @id
  specId Int? @map("spec_id")
  spec    AppSpec? @relation(fields: [specId], references: [id], onDelete: NoAction, owns: true)
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("os_app_owner")
}
`;

const CYCLE = `
model Left {
  id       Int     @id
  rightId Int? @map("right_id")
  right    Right?  @relation("LeftOwnsRight", fields: [rightId], references: [id], onDelete: NoAction, owns: true)
  lefts    Right[] @relation("RightOwnsLeft")
  ${SOFT}
  @@map("os_left")
}

model Right {
  id      Int    @id
  leftId Int? @map("left_id")
  left    Left?  @relation("RightOwnsLeft", fields: [leftId], references: [id], onDelete: NoAction, owns: true)
  rights  Left[] @relation("LeftOwnsRight")
  ${SOFT}
  @@map("os_right")
}
`;

// One owner with two owning columns into two targets.
const MULTI = `
model Cover {
  id   Int   @id
  kits Kit[]
  ${SOFT}
  @@map("os_cover")
}

model Stand {
  id   Int   @id
  kits Kit[]
  ${SOFT}
  @@map("os_stand")
}

model Kit {
  id      Int    @id
  coverId Int?   @map("cover_id")
  standId Int?   @map("stand_id")
  cover   Cover? @relation(fields: [coverId], references: [id], onDelete: NoAction, owns: true)
  stand   Stand? @relation(fields: [standId], references: [id], onDelete: NoAction, owns: true)
  ${SOFT}
  @@map("os_kit")
}
`;

// A soft cascade with a kept child.
const SHELF = `
model Shelf {
  id    Int    @id
  books Book[]
  memos Memo[]
  ${SOFT}
  @@map("os_shelf")
}

model Book {
  id      Int   @id
  shelfId Int   @map("shelf_id")
  shelf   Shelf @relation(fields: [shelfId], references: [id], onDelete: Cascade)
  ${SOFT}
  @@map("os_book")
}

model Memo {
  id      Int   @id
  shelfId Int   @map("shelf_id")
  shelf   Shelf @relation(fields: [shelfId], references: [id], onDelete: Cascade) @soft_delete.keep_child_rows
  ${SOFT}
  @@map("os_memo")
}
`;

// A chain of rows that own their parent: the repair stamps one level in each pass, and the bound is two passes.
const NODE = `
model Node {
  id       Int    @id
  parentId Int?   @map("parent_id")
  parent   Node?  @relation("Chain", fields: [parentId], references: [id], onDelete: NoAction, owns: true)
  children Node[] @relation("Chain")
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("os_node")
}
`;

const CLOCK = 'model Clock {\n  id Int @id\n  @@timestamps.updated_at("changed")\n  @@map("os_clock")\n}\n';
const T1 = "'2020-01-01 00:00:00.000'";
const T2 = "'2020-01-02 00:00:00.000'";
const T3 = "'2020-01-03 00:00:00.000'";
const PG = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";

type Env = { db: any; m: any; provider: string; now: string; registry: Registry };

async function open(provider: string, schema: string, header = ""): Promise<Env> {
  const registry = new Registry();
  const head = provider === "sqlite" ? 'datasource db {\n  provider = "sqlite"\n}\n' : header;
  const m: any = loads(head + schema, { registry });
  const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : PG, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  const now = provider === "sqlite" ? "strftime('%Y-%m-%d %H:%M:%f','now')" : "now()";
  return { db, m, provider, now, registry };
}

async function close(e: Env): Promise<void> {
  await e.db.dropTables();
  await e.db.close();
}

const archived = async (db: any, table: string): Promise<number[]> =>
  (await db.fetch(`SELECT id FROM ${table} WHERE deleted IS NOT NULL ORDER BY id`)).map((r: any) => Number(r.id));

const stamp = async (db: any, table: string, key: number): Promise<unknown> =>
  (await db.fetch(`SELECT deleted FROM ${table} WHERE id = ${key}`))[0].deleted;

async function seed(e: Env, guitars: [number, number][] = [], banjos: [number, number][] = [], specs: number[] = [1, 2]): Promise<void> {
  const { m, db } = e;
  await m.Spec.objects.using(db).insertMany(specs.map((id) => ({ id, body: `s${id}` })));
  if (guitars.length > 0) await m.Guitar.objects.using(db).insertMany(guitars.map(([id, specId]) => ({ id, specId })));
  if (banjos.length > 0) await m.Banjo.objects.using(db).insertMany(banjos.map(([id, specId]) => ({ id, specId })));
}

async function remove(e: Env, model: string, ...keys: number[]): Promise<unknown> {
  const M = e.m[model];
  return await M.objects.using(e.db).filter(M.id.in(keys)).delete();
}

async function revive(e: Env, model: string, ...keys: number[]): Promise<unknown> {
  const M = e.m[model];
  return await M.objects.using(e.db).filter(M.id.in(keys)).undelete();
}

const skip = !hasExtension("monorm.soft-delete");
// The package has no entry in the build `paths`, so the specifier stays out of the type check.
const SOFT_DELETE = "monorm/extensions/soft-delete";
const sweepOwned: (options: object) => Promise<Record<string, unknown[]>> = skip ? (async () => ({})) : (await import(SOFT_DELETE)).sweepOwned;
// SQLite reads an integer key as a bigint and PostgreSQL as a number: the report compares as numbers.
const sweep = async (db: any, options: Record<string, unknown> = {}) =>
  JSON.parse(JSON.stringify(await sweepOwned({ db, ...options }), (_k, v) => (typeof v === "bigint" ? Number(v) : v)));

test("a soft owner of a row without soft delete is a schema error", { skip }, () => {
  const source = `
model Plain {
  id     Int @id
  owners Own[]
}
model Own {
  id       Int    @id
  plainId Int? @map("plain_id")
  plain    Plain? @relation(fields: [plainId], references: [id], onDelete: NoAction, owns: true)
  @@soft_delete.deleted_at("deleted")
}
`;
  assert.throws(() => loads(source, { registry: new Registry() }), /keeps the Plain rows/);
});

for (const provider of ["sqlite", "postgresql"]) {
  async function run(name: string, body: (e: Env) => Promise<void>, schema = SCHEMA, header = ""): Promise<void> {
    test(`${name} (${provider})`, { skip }, async () => {
      const e = await open(provider, schema, header);
      try {
        await body(e);
      } finally {
        await close(e);
      }
    });
  }

  await run("the last soft deleted owner archives the owned row with its stamp", async (e) => {
    await seed(e, [[1, 1], [2, 2]]);
    await remove(e, "Guitar", 1);
    assert.deepEqual(await archived(e.db, "os_spec"), [1]);
    assert.deepEqual(await stamp(e.db, "os_spec", 1), await stamp(e.db, "os_guitar", 1));
    assert.deepEqual(await archived(e.db, "os_guitar"), [1]);
  });

  await run("a shared owned row stays live until its last owner is archived", async (e) => {
    await seed(e, [[1, 1], [2, 1]]);
    await remove(e, "Guitar", 1);
    assert.deepEqual(await archived(e.db, "os_spec"), []);
    await remove(e, "Guitar", 2);
    assert.deepEqual(await archived(e.db, "os_spec"), [1]);
    assert.deepEqual(await stamp(e.db, "os_spec", 1), await stamp(e.db, "os_guitar", 2));
  });

  await run("owners of two models share the owned row", async (e) => {
    await seed(e, [[1, 1]], [[1, 1]]);
    await remove(e, "Guitar", 1);
    assert.deepEqual(await archived(e.db, "os_spec"), []);
    await remove(e, "Banjo", 1);
    assert.deepEqual(await archived(e.db, "os_spec"), [1]);
  });

  await run("all owners archived in one statement archive the shared row", async (e) => {
    await seed(e, [[1, 1], [2, 1], [3, 2]]);
    await remove(e, "Guitar", 1, 2);
    assert.deepEqual(await archived(e.db, "os_spec"), [1]);
    assert.deepEqual(await archived(e.db, "os_guitar"), [1, 2]);
  });

  await run("a key move and an archive in one statement release the old row only", async (e) => {
    await seed(e, [[1, 1]]);
    await e.db.execute(`UPDATE os_guitar SET spec_id = 2, deleted = ${e.now} WHERE id = 1`);
    assert.deepEqual(await archived(e.db, "os_spec"), [1]);
    assert.deepEqual(await sweep(e.db), { Spec: [{ id: 2 }] });
  });

  await run("an undelete with the same stamp revives the owned row", async (e) => {
    await seed(e, [[1, 1], [2, 1]]);
    await remove(e, "Guitar", 1, 2);
    assert.deepEqual(await archived(e.db, "os_spec"), [1]);
    await revive(e, "Guitar", 1);
    assert.deepEqual(await archived(e.db, "os_spec"), []);
    assert.deepEqual(await archived(e.db, "os_guitar"), [2]);
  });

  await run("an undelete with another stamp keeps the row while another owner is live", async (e) => {
    await seed(e, [[1, 1], [2, 1]]);
    await e.db.execute(`UPDATE os_spec SET deleted = ${T1} WHERE id = 1`);
    await e.db.execute(`UPDATE os_guitar SET deleted = ${T2} WHERE id = 1`);
    await e.db.execute("UPDATE os_guitar SET deleted = NULL WHERE id = 1");
    assert.deepEqual(await archived(e.db, "os_spec"), [1]);
    assert.deepEqual(await archived(e.db, "os_guitar"), []);
  });

  await run("an undelete of the only live owner revives the row whatever its stamp", async (e) => {
    await seed(e, [[1, 1], [2, 1]]);
    await e.db.execute(`UPDATE os_spec SET deleted = ${T1} WHERE id = 1`);
    await e.db.execute(`UPDATE os_guitar SET deleted = ${T2} WHERE id = 1`);
    await e.db.execute(`UPDATE os_guitar SET deleted = ${T3} WHERE id = 2`);
    await e.db.execute("UPDATE os_guitar SET deleted = NULL WHERE id = 1");
    assert.deepEqual(await archived(e.db, "os_spec"), []);
    assert.deepEqual(await archived(e.db, "os_guitar"), [2]);
  });

  await run("the sweep and the revive go through a two level chain", async (e) => {
    const { db, m } = e;
    await m.Detail.objects.using(db).insertMany([{ id: 1 }]);
    await m.Basket.objects.using(db).insertMany([{ id: 1, detailId: 1 }]);
    await m.Order.objects.using(db).insertMany([{ id: 1, basketId: 1 }]);
    await remove(e, "Order", 1);
    assert.deepEqual(await archived(db, "os_basket"), [1]);
    assert.deepEqual(await archived(db, "os_detail"), [1]);
    assert.deepEqual(await stamp(db, "os_detail", 1), await stamp(db, "os_order", 1));
    await revive(e, "Order", 1);
    assert.deepEqual(await archived(db, "os_basket"), []);
    assert.deepEqual(await archived(db, "os_detail"), []);
  }, CHAIN);

  await run("application mode adds no sweep and the target stays live", async (e) => {
    const { db, m } = e;
    await m.AppSpec.objects.using(db).insertMany([{ id: 1 }]);
    await m.AppOwner.objects.using(db).insertMany([{ id: 1, specId: 1 }]);
    await m.AppOwner.objects.using(db).filter(m.AppOwner.id.eq(1)).delete();
    assert.deepEqual(await archived(db, "os_app_owner"), [1]);
    assert.deepEqual(await archived(db, "os_app_spec"), []);
    const names: string[] =
      provider === "sqlite"
        ? (await db.fetch("SELECT name FROM sqlite_master WHERE type = 'trigger'")).map((r: any) => r.name)
        : (await db.fetch("SELECT tgname AS name FROM pg_trigger WHERE NOT tgisinternal AND tgrelid IN ('os_app_spec'::regclass, 'os_app_owner'::regclass)")).map((r: any) => r.name);
    assert.deepEqual(names.filter((n) => n.includes("soft_delete")), []);
  }, APP);

  await run("skip triggers of soft delete still raises", async () => {
    // A skippable extension must be in use: only then does `skipTriggers()` check the names.
    await skipTriggers(["timestamps"], async () => {});
    await assert.rejects(() => skipTriggers(["soft-delete"], async () => {}), /no skippable triggers for soft-delete/);
  }, SCHEMA + CLOCK);

  await run("raw sql update and delete give the result of the model call", async (e) => {
    const { db } = e;
    await seed(e, [[1, 1], [2, 2], [3, 3]], [], [1, 2, 3]);
    await remove(e, "Guitar", 1);
    await db.execute(`UPDATE os_guitar SET deleted = ${e.now} WHERE id = 2`);
    await db.execute("DELETE FROM os_guitar WHERE id = 3");
    assert.deepEqual(await archived(db, "os_spec"), [1, 2, 3]);
    assert.deepEqual(await archived(db, "os_guitar"), [1, 2, 3]);
    await db.execute("UPDATE os_guitar SET deleted = NULL WHERE id IN (1, 2)");
    assert.deepEqual(await archived(db, "os_spec"), [3]);
  });

  await run("a live owned row goes with the hard delete of an owner without soft delete", async (e) => {
    const { db } = e;
    await seed(e, [], [], [1, 2]);
    await e.m.Frame.objects.using(db).insertMany([{ id: 1, specId: 1 }]);
    await db.execute("DELETE FROM os_frame WHERE id = 1");
    const remaining = (await db.fetch("SELECT id FROM os_spec ORDER BY id")).map((r: any) => Number(r.id));
    const live = await archived(db, "os_spec");
    // PostgreSQL deletes the live row; SQLite soft-deletes it first (the rewrite of the delete fires before the owner trigger).
    assert.deepEqual([remaining, live], provider === "postgresql" ? [[2], []] : [[1, 2], [1]]);
  });

  await run("sweep_owned reports a live row whose owners are all archived", async (e) => {
    const { db } = e;
    await seed(e, [], [], [1, 2, 3]);
    await db.execute(`INSERT INTO os_guitar (id, spec_id, deleted) VALUES (1, 1, ${T1})`);
    await db.execute(`INSERT INTO os_banjo (id, spec_id, deleted) VALUES (1, 1, ${T1}), (2, 3, ${T1})`);
    await db.execute("INSERT INTO os_guitar (id, spec_id) VALUES (2, 3)");
    assert.deepEqual(await sweep(db), { Spec: [{ id: 1 }] });
    assert.deepEqual(await archived(db, "os_spec"), []);
  });

  await run("sweep_owned of a clean database is empty", async (e) => {
    await seed(e, [[1, 1]]);
    assert.deepEqual(await sweep(e.db), {});
    assert.deepEqual(await sweep(e.db, { repair: true }), {});
  });

  await run("sweep_owned repair archives the row and a second report is empty", async (e) => {
    const { db } = e;
    await seed(e, [], [], [1, 2]);
    await db.execute(`INSERT INTO os_guitar (id, spec_id, deleted) VALUES (1, 1, ${T1})`);
    assert.deepEqual(await sweep(db, { repair: true }), { Spec: [{ id: 1 }] });
    assert.deepEqual(await archived(db, "os_spec"), [1]);
    assert.deepEqual(await sweep(db), {});
    assert.deepEqual(await sweep(db, { repair: true }), {});
  });

  await run("sweep_owned repair leaves a row that got a live owner after the report", async (e) => {
    const { db } = e;
    await seed(e, [], [], [1]);
    await db.execute(`INSERT INTO os_guitar (id, spec_id, deleted) VALUES (1, 1, ${T1})`);
    assert.deepEqual(await sweep(db), { Spec: [{ id: 1 }] });
    await db.execute("INSERT INTO os_guitar (id, spec_id) VALUES (2, 1)");
    assert.deepEqual(await sweep(db, { repair: true }), {});
    assert.deepEqual(await archived(db, "os_spec"), []);
  });

  await run("sweep_owned repair of a chain settles through the triggers in one pass", async (e) => {
    const { db } = e;
    await db.execute("INSERT INTO os_detail (id) VALUES (1)");
    await db.execute("INSERT INTO os_basket (id, detail_id) VALUES (1, 1)");
    await db.execute(`INSERT INTO os_order (id, basket_id, deleted) VALUES (1, 1, ${T1})`);
    assert.deepEqual(await sweep(db), { Basket: [{ id: 1 }] });
    assert.deepEqual(await sweep(db, { repair: true }), { Basket: [{ id: 1 }] });
    assert.deepEqual(await archived(db, "os_basket"), [1]);
    assert.deepEqual(await archived(db, "os_detail"), [1]);
    assert.deepEqual(await sweep(db), {});
  }, CHAIN);

  await run("sweep_owned repair needs two passes where no trigger stamps the next level", async (e) => {
    const { db } = e;
    await db.execute("INSERT INTO os_detail (id) VALUES (1)");
    await db.execute("INSERT INTO os_basket (id, detail_id) VALUES (1, 1)");
    await db.execute(`INSERT INTO os_order (id, basket_id, deleted) VALUES (1, 1, ${T1})`);
    assert.deepEqual(await sweep(db), { Basket: [{ id: 1 }] });
    assert.deepEqual(await sweep(db, { repair: true }), { Basket: [{ id: 1 }], Detail: [{ id: 1 }] });
    assert.deepEqual(await archived(db, "os_detail"), [1]);
    assert.deepEqual(await sweep(db), {});
  }, CHAIN.replaceAll(SOFT, '@@soft_delete.deleted_at("deleted", mode: "application")'));

  await run("sweep_owned models filter and a model that is no target", async (e) => {
    const { db, m } = e;
    await db.execute("INSERT INTO os_detail (id) VALUES (1)");
    await db.execute("INSERT INTO os_basket (id, detail_id) VALUES (1, 1)");
    await db.execute(`INSERT INTO os_order (id, basket_id, deleted) VALUES (1, 1, ${T1})`);
    assert.deepEqual(await sweep(db, { models: ["Detail"] }), {});
    assert.deepEqual(await sweep(db, { models: [m.Basket] }), { Basket: [{ id: 1 }] });
    await assert.rejects(() => sweep(db, { models: ["Order"] }), /Order/);
  }, CHAIN);

  await run("the revive cascade undeletes the children with the stamp of the parent and skips a kept child", async (e) => {
    const { db, m } = e;
    await m.Shelf.objects.using(db).insertMany([{ id: 1 }]);
    await m.Book.objects.using(db).insertMany([{ id: 1, shelfId: 1 }, { id: 2, shelfId: 1 }]);
    await m.Memo.objects.using(db).insertMany([{ id: 1, shelfId: 1 }]);
    await db.execute(`UPDATE os_book SET deleted = ${T1} WHERE id = 2`);
    await db.execute(`UPDATE os_shelf SET deleted = ${T2} WHERE id = 1`);
    assert.deepEqual(await archived(db, "os_book"), [1, 2]);
    assert.deepEqual(await archived(db, "os_memo"), [], "the cascade leaves the kept child");
    assert.notEqual(String(await stamp(db, "os_book", 2)), String(await stamp(db, "os_shelf", 1)), "a row that is deleted already keeps its own stamp");
    await db.execute("UPDATE os_shelf SET deleted = NULL WHERE id = 1");
    assert.deepEqual(await archived(db, "os_shelf"), []);
    assert.deepEqual(await archived(db, "os_book"), [2], "only the row that the parent stamped comes back");
    assert.deepEqual(await archived(db, "os_memo"), []);
  }, SHELF);

  await run("one owner with two owning columns sweeps and revives both targets", async (e) => {
    const { db, m } = e;
    await m.Cover.objects.using(db).insertMany([{ id: 1 }, { id: 2 }]);
    await m.Stand.objects.using(db).insertMany([{ id: 1 }]);
    await m.Kit.objects.using(db).insertMany([
      { id: 1, coverId: 1, standId: 1 },
      { id: 2, coverId: 2, standId: null },
      { id: 3, coverId: 2, standId: null },
    ]);
    await remove(e, "Kit", 1, 2);
    assert.deepEqual(await archived(db, "os_cover"), [1], "cover 2 keeps its live owner");
    assert.deepEqual(await archived(db, "os_stand"), [1]);
    const kit = String(await stamp(db, "os_kit", 1));
    assert.equal(String(await stamp(db, "os_cover", 1)), kit);
    assert.equal(String(await stamp(db, "os_stand", 1)), kit);
    await remove(e, "Kit", 3);
    assert.deepEqual(await archived(db, "os_cover"), [1, 2]);
    await revive(e, "Kit", 1);
    assert.deepEqual(await archived(db, "os_cover"), [2]);
    assert.deepEqual(await archived(db, "os_stand"), []);
    await db.execute("UPDATE os_kit SET deleted = NULL WHERE id = 3");
    assert.deepEqual(await archived(db, "os_cover"), [], "the only live owner revives the cover whatever its stamp");
  }, MULTI);

  if (provider === "postgresql") {
    await run("a two model cycle ends on postgresql", async (e) => {
      const { db, m } = e;
      await m.Left.objects.using(db).insertMany([{ id: 1, rightId: null }]);
      await m.Right.objects.using(db).insertMany([{ id: 1, leftId: 1 }]);
      await db.execute("UPDATE os_left SET right_id = 1 WHERE id = 1");
      await remove(e, "Left", 1);
      assert.deepEqual(await archived(db, "os_left"), [1]);
      assert.deepEqual(await archived(db, "os_right"), [1]);
    }, CYCLE);

    await run("two concurrent co owner deletes archive the shared row", async (e) => {
      const { db } = e;
      const other = await connect(PG, { registry: e.registry, default: false });
      try {
        await seed(e, [[1, 1], [2, 1]]);
        let second: Promise<void> | undefined;
        let done = false;
        await db.transaction(async () => {
          await db.execute(`UPDATE os_guitar SET deleted = ${e.now} WHERE id = 1`);
          second = other.transaction(async () => {
            await other.execute(`UPDATE os_guitar SET deleted = ${e.now} WHERE id = 2`);
          }).then(() => { done = true; });
          await new Promise((r) => setTimeout(r, 500));
          assert.equal(done, false, "the second delete must wait for the lock on the shared row");
        });
        await second;
        assert.deepEqual(await archived(db, "os_spec"), [1]);
      } finally {
        await other.close();
      }
    });

    await run("sweep_owned repair raises at the bound and rolls every pass back", async (e) => {
      const { db } = e;
      await db.execute("INSERT INTO os_node (id, parent_id) VALUES (4, NULL), (3, 4), (2, 3)");
      await db.execute(`INSERT INTO os_node (id, parent_id, deleted) VALUES (1, 2, ${T1})`);
      assert.deepEqual(await sweep(db), { Node: [{ id: 2 }] });
      await assert.rejects(() => sweep(db, { repair: true }), /did not settle/);
      assert.deepEqual(await archived(db, "os_node"), [1], "the first pass is rolled back with the others");
    }, NODE);

    await run("a new owner waits for the sweep that holds the lock of the target", async (e) => {
      const { db } = e;
      const other = await connect(PG, { registry: e.registry, default: false });
      try {
        await seed(e, [[1, 1]]);
        let second: Promise<void> | undefined;
        let done = false;
        await db.transaction(async () => {
          await db.execute(`UPDATE os_guitar SET deleted = ${e.now} WHERE id = 1`);
          second = other.transaction(async () => {
            await other.execute("INSERT INTO os_banjo (id, spec_id) VALUES (1, 1)");
          }).then(() => { done = true; });
          await new Promise((r) => setTimeout(r, 500));
          assert.equal(done, false, "the foreign key check of a new owner must wait for the lock on the target");
        });
        await second;
      } finally {
        await other.close();
      }
    });

    await run("sweep_owned refuses a role that is not a superuser or a bypass role", async (e) => {
      const { db } = e;
      try {
        await db.execute("DROP ROLE IF EXISTS os_plain");
        await db.execute("DROP ROLE IF EXISTS os_member");
        await db.execute("DROP ROLE IF EXISTS os_bypass");
        await db.execute("CREATE ROLE os_plain NOLOGIN");
        await db.execute("CREATE ROLE os_bypass NOLOGIN");
        await db.execute("CREATE ROLE os_member NOLOGIN IN ROLE os_bypass");
        await db.execute("GRANT ALL ON ALL TABLES IN SCHEMA public TO os_bypass");
        await seed(e, [], [], [1]);
        await db.execute(`INSERT INTO os_guitar (id, spec_id, deleted) VALUES (1, 1, ${T1})`);
        await db.transaction(async () => {
          await db.execute("SET LOCAL ROLE os_plain");
          await assert.rejects(() => sweep(db), /superuser or a role of bypass_roles/);
        });
        await db.transaction(async () => {
          await db.execute("SET LOCAL ROLE os_member");
          assert.deepEqual(await sweep(db), { Spec: [{ id: 1 }] });
        });
      } finally {
        await db.execute("DROP OWNED BY os_plain, os_member, os_bypass");
        await db.execute("DROP ROLE IF EXISTS os_plain, os_member, os_bypass");
      }
    }, SCHEMA, 'datasource db {\n  provider = "postgresql"\n  bypass_roles = ["os_bypass"]\n}\n');
  }
}
