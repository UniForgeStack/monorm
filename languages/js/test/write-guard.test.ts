/** update() and delete() with no caller filter raise UnfilteredWriteError; all() and the connect option lift it. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { and, connect, loads, NotConnected, or, ORMError, Registry, scope, UnfilteredWriteError } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";
import { DATABASE_URL } from "./helpers.js";

const schema = `
model Parent {
  id Int @id
  n Int @default(0)
  children Child[]
  @@map("wg_node_parent")
}
model Child {
  id Int @id
  parent_id Int
  parent Parent @relation(fields: [parent_id], references: [id])
  views Int @default(0)
  @@map("wg_node_child")
}
`;

async function openDb(provider: string, options: { allowUnfilteredWrites?: boolean } = {}) {
  const registry = new Registry();
  const models = loads(`datasource db {\n provider = "${provider}"\n}\n${schema}`, { registry });
  const Parent: any = models["Parent"], Child: any = models["Child"];
  const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false, ...options });
  await db.dropTables();
  await db.createTables();
  await Parent.objects.using(db).insertMany([{ id: 1 }, { id: 2 }]);
  await Child.objects.using(db).insertMany([
    { id: 1, parentId: 1, views: 1 }, { id: 2, parentId: 1, views: 2 }, { id: 3, parentId: 2, views: 3 },
  ]);
  return { db, Parent, Child };
}

for (const provider of ["sqlite", "postgresql"]) {
  test(`a write with no filter raises before any sql (${provider})`, async () => {
    const { db, Child } = await openDb(provider);
    try {
      const statements: string[] = [];
      const remove = db.onQuery((e) => { statements.push(e.sql); });
      const children = Child.objects.using(db);
      const writes = [() => children.update({ views: 0 }), () => children.delete()];
      for (const write of writes) {
        await assert.rejects(async () => { await write(); }, (error: UnfilteredWriteError) => {
          assert.ok(error instanceof UnfilteredWriteError && error instanceof ORMError);
          assert.match(error.message, /\.all\(\)/);
          assert.match(error.message, /allowUnfilteredWrites/);
          assert.equal(error.op, write === writes[0] ? "update" : "delete");
          assert.equal(error.model, "Child");
          return true;
        });
        assert.throws(() => write().inspect(), UnfilteredWriteError);
      }
      remove();
      assert.deepEqual(statements, []);
      assert.equal(await children.count(), 3);
    } finally { await db.close(); }
  });

  test(`a caller filter passes the guard (${provider})`, async () => {
    const { db, Parent, Child } = await openDb(provider);
    try {
      const children = Child.objects.using(db);
      assert.equal(await children.filter(Child.id.eq(1)).update({ views: 10 }), 1);
      assert.equal(await children.exclude(Child.id.eq(1)).update({ views: 20 }), 2);
      const parent = await Parent.objects.using(db).get(Parent.id.eq(1));
      assert.equal(await parent.children.update({ views: 30 }), 2);
      assert.equal(await parent.children.delete(), 2);
      assert.equal(await children.count(), 1);
    } finally { await db.close(); }
  });

  test(`all marks every row on each kind of query set (${provider})`, async () => {
    const { db, Parent, Child } = await openDb(provider);
    try {
      const children = Child.objects.using(db);
      const parent = await Parent.objects.using(db).get(Parent.id.eq(1));
      assert.equal(await children.all().update({ views: 5 }), 3);
      assert.equal(await children.filter(Child.id.gt(0)).all().update({ views: 6 }), 3);
      assert.equal(await children.all().filter(Child.id.gt(0)).update({ views: 7 }), 3);
      assert.equal(await parent.children.all().update({ views: 8 }), 2);
      assert.equal(await parent.children.all().delete(), 2);
      assert.equal(await children.all().delete(), 1);
      assert.equal(await children.count(), 0);
    } finally { await db.close(); }
  });

  test(`the connect option turns the guard off (${provider})`, async () => {
    const { db, Child } = await openDb(provider, { allowUnfilteredWrites: true });
    try {
      const children = Child.objects.using(db);
      assert.equal(await children.update({ views: 1 }), 3);
      assert.equal(await children.delete(), 3);
    } finally { await db.close(); }
  });

  test(`a filter that is always true does not pass the guard (${provider})`, async () => {
    const { db, Child } = await openDb(provider);
    try {
      const children = Child.objects.using(db);
      const alwaysTrue = [
        children.filter(and()),
        children.exclude(or()),
        children.filter(Child.id.notIn([])),
        children.filter(and(), and()),
      ];
      for (const qs of alwaysTrue) {
        await assert.rejects(async () => { await qs.update({ views: 0 }); }, UnfilteredWriteError);
        await assert.rejects(async () => { await qs.delete(); }, UnfilteredWriteError);
      }
      assert.equal(await children.count(), 3);
      assert.equal(await children.filter(and(), Child.id.eq(1)).update({ views: 10 }), 1);
      assert.equal(await children.filter(and()).all().update({ views: 5 }), 3);
    } finally { await db.close(); }
  });

  test(`a union keeps the every row mark of either side (${provider})`, async () => {
    const { db, Child } = await openDb(provider);
    try {
      const some = Child.objects.using(db).filter(Child.id.eq(1));
      const every = Child.objects.using(db).all();
      assert.equal(await some.or(Child.objects.all()).update({ views: 1 }), 3);
      assert.equal(await every.or(Child.objects.filter(Child.id.eq(1))).update({ views: 2 }), 3);
      assert.equal(await every.or(Child.objects.all()).update({ views: 3 }), 3);
      await assert.rejects(async () => { await some.or(Child.objects).update({ views: 4 }); }, UnfilteredWriteError);
      assert.equal(await Child.objects.using(db).filter(Child.views.eq(3)).count(), 3);
    } finally { await db.close(); }
  });

  test(`writes outside the guard still run (${provider})`, async () => {
    const { db, Child } = await openDb(provider);
    try {
      const children = Child.objects.using(db);
      await children.insert({ id: 4, parentId: 2, views: 0 });
      assert.equal(await children.updateMany([{ id: 1, views: 9 }, { id: 2, views: 9 }]), 2);
      const first = await children.get(Child.id.eq(1));
      await first.update({ views: 11 });
      await first.delete();
      await db.execute("DELETE FROM wg_node_child");
      assert.equal(await children.count(), 0);
    } finally { await db.close(); }
  });
}

test("a write with no database raises not connected before the guard", async () => {
  const registry = new Registry();
  const models = loads(`datasource db {\n provider = "sqlite"\n}\n${schema}`, { registry });
  const Child: any = models["Child"];
  const db = await connect("sqlite://:memory:", { registry, default: false, allowUnfilteredWrites: true });
  try {
    for (const write of [() => Child.objects.update({ views: 0 }), () => Child.objects.delete()]) {
      await assert.rejects(async () => { await write(); }, NotConnected);
      assert.throws(() => write().inspect(), NotConnected);
    }
  } finally { await db.close(); }
});

const extensionsSchema = `datasource db {
  provider = "sqlite"
}
model Ledger {
  id Int @id
  shop_id Int
  active Boolean @default(true)
  @@query.filter("active", "active == true")
  @@query.filter("tenant", "shop_id == scope.shop")
}
model Doc {
  id Int @id
  title String
  @@soft_delete.deleted_at("deleted", mode: "application")
}
`;

const softDeleteSchema = `datasource db {
  provider = "sqlite"
}
model Doc {
  id Int @id
  title String
  @@soft_delete.deleted_at("deleted", mode: "application")
}
`;

const manyToManySchema = `datasource db {
  provider = "sqlite"
}
model Post {
  id Int @id
  tags Tag[] @relation(through: PostTag, through_fields: [post, tag])
}
model Tag {
  id Int @id
  name String
  posts Post[] @relation(through: PostTag, through_fields: [tag, post])
}
model PostTag {
  id Int @id @default(autoincrement())
  post_id Int
  tag_id Int
  post Post @relation(fields: [post_id], references: [id])
  tag Tag @relation(fields: [tag_id], references: [id])
}
`;

test("a query default the scope and soft delete do not count as a filter", { skip: !(hasExtension("monorm.query-defaults") && hasExtension("monorm.soft-delete")) }, async () => {
  const registry = new Registry();
  const models = loads(extensionsSchema, { registry });
  const Ledger: any = models["Ledger"], Doc: any = models["Doc"];
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await Ledger.objects.using(db).insertMany([{ id: 1, shopId: 1 }, { id: 2, shopId: 2 }]);
    await Doc.objects.using(db).insertMany([{ id: 1, title: "a" }, { id: 2, title: "b" }]);
    await scope({ shop: 1 }, async () => {
      await assert.rejects(async () => { await Ledger.objects.using(db).update({ active: true }); }, UnfilteredWriteError);
      await assert.rejects(async () => { await Ledger.objects.using(db).delete(); }, UnfilteredWriteError);
      assert.equal(await Ledger.objects.using(db).all().update({ active: true }), 1);
    });
    await assert.rejects(async () => { await Doc.objects.using(db).delete(); }, UnfilteredWriteError);
    assert.equal(await Doc.objects.using(db).all().delete(), 2);
  } finally { await db.close(); }
});

test("a many to many set counts as a filter", async () => {
  const registry = new Registry();
  const models = loads(manyToManySchema, { registry });
  const Post: any = models["Post"], Tag: any = models["Tag"], PostTag: any = models["PostTag"];
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await Post.objects.using(db).insertMany([{ id: 1 }]);
    await Tag.objects.using(db).insertMany([{ id: 1, name: "x" }, { id: 2, name: "y" }]);
    await PostTag.objects.using(db).insertMany([{ postId: 1, tagId: 1 }]);
    await assert.rejects(async () => { await Tag.objects.using(db).update({ name: "z" }); }, UnfilteredWriteError);
    const post = await Post.objects.using(db).get(Post.id.eq(1));
    assert.equal(await post.tags.update({ name: "z" }), 1);
    assert.deepEqual((await Tag.objects.using(db).orderBy(Tag.id)).map((t: any) => t.name), ["z", "y"]);
  } finally { await db.close(); }
});

test("undelete passes the guard and hard delete does not", { skip: !hasExtension("monorm.soft-delete") }, async () => {
  const registry = new Registry();
  const models = loads(softDeleteSchema, { registry });
  const Doc: any = models["Doc"];
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await Doc.objects.using(db).insertMany([{ id: 1, title: "a" }, { id: 2, title: "b" }]);
    assert.equal(await Doc.objects.using(db).all().delete(), 2);
    // The verb names its rows (the soft-deleted ones), so it needs no .all().
    assert.equal(await Doc.objects.using(db).undelete(), 2);
    await assert.rejects(async () => { await Doc.objects.using(db).hardDelete(); }, UnfilteredWriteError);
    assert.equal(await Doc.objects.using(db).all().hardDelete(), 2);
  } finally { await db.close(); }
});
