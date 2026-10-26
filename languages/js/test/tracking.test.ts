import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, of, Registry, scope, VersionConflict, type Database } from "../src/index.js";
import { artifactCapabilities, hasExtension } from "./artifact-caps.js";

const capabilities = artifactCapabilities();
const OLD = new Date("2000-01-01T00:00:00Z");
type SoftDeletable = { hardDelete(): Promise<void>; undelete(): Promise<void> };
type Models = Record<string, { objects: any; [key: string]: any }>;

async function open(dialect: string, source: string): Promise<[Database, Models]> {
  const registry = new Registry();
  const models = await loads((dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + source, { registry }) as unknown as Models;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  return [db, models];
}

for (const dialect of ["sqlite", "postgres"]) {
  for (const mode of ["database", "application"]) {
    test(`updated_at is set on ORM updates and by the trigger (${dialect}, ${mode})`, { skip: !hasExtension("orm.timestamps") }, async () => {
      const [db, models] = await open(dialect, `
model Note {
  id      Int      @id
  views   Int      @default(0)
  @@timestamps.updated_at("changed", mode: "${mode}")
  @@map("tracking_node_notes")
}`);
      const notes = models.Note!.objects.using(db);
      try {
        const note = await notes.insert({ id: 1, changed: OLD });
        await notes.insert({ id: 2, changed: OLD });
        await note.update({ views: 1 });
        assert.ok(note.changed > OLD);
        const [row] = await notes.filter(models.Note!.id.eq(2)).update({ views: 2 }, { returning: true });
        assert.ok(row.changed > OLD);
        await note.update({ changed: OLD });
        assert.equal(note.changed.getTime(), OLD.getTime());
        await db.execute("UPDATE tracking_node_notes SET views = 4 WHERE id = 1");
        await note.load({ reload: true });
        assert.equal(note.changed > OLD, mode === "database");
      } finally { await db.dropTables(); await db.close(); }
    });

    test(`soft delete updates instead of deleting (${dialect}, ${mode})`, { skip: !hasExtension("orm.soft-delete") }, async () => {
      const [db, models] = await open(dialect, `
model Author {
  id      Int       @id
  books   Book[]
  @@soft_delete.deleted_at("deleted", mode: "${mode}")
  @@map("tracking_node_authors")
}
model Book {
  id        Int       @id
  author_id Int
  author    Author    @relation(fields: [author_id], references: [id], onDelete: Cascade)
  @@soft_delete.deleted_at("removed", mode: "${mode}")
  @@map("tracking_node_books")
}`);
      const Author = models.Author!, authors = Author.objects.using(db);
      try {
        await authors.insertMany([{ id: 1 }, { id: 2 }, { id: 3 }]);
        await models.Book!.objects.using(db).insertMany([{ id: 1, authorId: 1 }, { id: 2, authorId: 2 }]);
        assert.equal(await authors.filter(Author.id.eq(1)).delete(), 1);
        assert.equal(await authors.filter(Author.id.eq(1)).delete(), 0);
        const [row] = await authors.filter(Author.id.eq(2)).delete({ returning: true });
        assert.ok(row.id === 2 && row.deleted !== null);
        assert.equal(await authors.count(), 1);
        assert.deepEqual((await authors.deletedOnly().orderBy(Author.id.asc()).all()).map((a: { id: number }) => a.id), [1, 2]);
        assert.equal(await authors.allWithDeleted().count(), 3);
        const removed = (await models.Book!.objects.using(db).deletedOnly().all()).length;
        assert.equal(removed, mode === "database" ? 2 : 0);
        assert.equal(await authors.filter(Author.id.eq(1)).undelete(), 1);
        const restored = await authors.get(Author.id.eq(1));
        assert.equal(restored.deleted, null);
        await restored.delete();
        assert.equal(await authors.get(Author.id.eq(1)), null);
        assert.notEqual((await authors.allWithDeleted().get(Author.id.eq(1))).deleted, null);
        await (restored as SoftDeletable).undelete();
        assert.equal(restored.deleted, null);
        await db.execute("DELETE FROM tracking_node_books");
        await db.execute("DELETE FROM tracking_node_authors WHERE id = 3");
        assert.equal(await authors.allWithDeleted().filter(Author.id.eq(3)).count(), mode === "database" ? 1 : 0);
        assert.equal(await authors.filter(Author.id.eq(3)).count(), 0);
        assert.equal(await authors.filter(Author.id.eq(2)).hardDelete(), 1);
        await (restored as SoftDeletable).hardDelete();
        assert.equal(await authors.allWithDeleted().filter(Author.id.eq(1)).count(), mode === "database" ? 1 : 0);
      } finally { await db.dropTables(); await db.close(); }
    });
  }

  const versionedSoftDelete = `
model Doc {
  id      Int       @id
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@locking.version
  @@map("tracking_node_soft_docs")
}`;

  test(`a soft-deleted instance can be undeleted (${dialect})`, { skip: !hasExtension("orm.soft-delete") || !hasExtension("orm.locking") }, async () => {
    const [db, models] = await open(dialect, versionedSoftDelete);
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insertMany([{ id: 1 }]);
      const doc = await docs.get(Doc.id.eq(1));
      await doc.delete();
      assert.ok(doc.version === 1 && doc.deleted !== null);
      await (doc as SoftDeletable).undelete();
      assert.ok(doc.version === 2 && doc.deleted === null);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`an instance reloads and updates its own soft-deleted row (${dialect})`, { skip: !hasExtension("orm.soft-delete") || !hasExtension("orm.locking") }, async () => {
    const [db, models] = await open(dialect, versionedSoftDelete);
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insertMany([{ id: 1 }]);
      const doc = await docs.get(Doc.id.eq(1));
      await doc.delete();
      assert.equal((await doc.qs()).length, 1); // qs() applies no root default
      await doc.load({ reload: true });
      assert.ok(doc.deleted !== null);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a partial versioned instance updates twice without a conflict (${dialect})`, { skip: !hasExtension("orm.locking") }, async () => {
    const [db, models] = await open(dialect, `
model Doc {
  id      Int    @id
  title   String
  @@locking.version
  @@map("tracking_node_partial_docs")
}`);
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insertMany([{ id: 1, title: "a" }]);
      const partial = await docs.load(Doc.title, Doc.version).get(Doc.id.eq(1));
      await partial.update({ title: "b" });
      await partial.update({ title: "c" });
      assert.equal(partial.version, 2);
      const stale = await docs.get(Doc.id.eq(1));
      await partial.update({ title: "d" });
      await assert.rejects(stale.update({ title: "e" }), VersionConflict);
    } finally { await db.dropTables(); await db.close(); }
  });

  const versionedSkip = { skip: !hasExtension("orm.soft-delete", "orm.locking") };

  test(`the soft-delete methods bypass only the deleted default (${dialect})`, { skip: !hasExtension("orm.soft-delete") || !capabilities.includes("query-defaults") }, async () => {
    const [db, models] = await open(dialect, `
model Doc {
  id      Int       @id
  shop    Int
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@query.filter("tenant", "shop == scope.shop")
  @@map("tracking_node_tenant_docs")
}`);
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    const ids = async (qs: any) => (await qs.orderBy(Doc.id.asc()).all()).map((d: { id: number }) => d.id);
    try {
      await docs.insertMany([1, 2, 3, 4].map((id) => ({ id, shop: id < 3 ? 1 : 2 })));
      await scope({ shop: 2 }, async () => assert.equal(await docs.filter(Doc.id.eq(3)).delete(), 1));
      await scope({ shop: 1 }, async () => {
        assert.equal(await docs.filter(Doc.id.eq(1)).delete(), 1);
        assert.deepEqual(await ids(docs), [2]);
        assert.deepEqual(await ids(docs.allWithDeleted()), [1, 2]);
        assert.deepEqual(await ids(docs.deletedOnly()), [1]);
        assert.equal(await docs.undelete(), 1);
        assert.equal(await docs.count(), 2);
        assert.equal(await docs.delete(), 2);
        assert.equal(await docs.hardDelete(), 2);
      });
      const rows = await docs.withoutDefaults().orderBy(Doc.id.asc()).all();
      assert.deepEqual(rows.map((d: { id: number; deleted: Date | null }) => [d.id, d.deleted !== null]), [[3, true], [4, false]]);
      const reads = /default "tenant" from orm\.query-defaults .* reads scope\.shop/;
      for (const statement of [docs.undelete(), docs.hardDelete(), docs.allWithDeleted().count(), docs.deletedOnly().count()]) {
        await assert.rejects(Promise.resolve(statement), reads);
      }
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`an instance hardDelete also drops the tenant filter (${dialect})`, { skip: !hasExtension("orm.soft-delete") || !capabilities.includes("query-defaults") }, async () => {
    const [db, models] = await open(dialect, `
model Doc {
  id      Int       @id
  shop    Int
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@query.filter("tenant", "shop == scope.shop")
  @@map("tracking_node_tenant_hard_docs")
}`);
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insertMany([{ id: 1, shop: 1 }, { id: 2, shop: 2 }]);
      const doc = await scope({ shop: 2 }, async () => docs.get(Doc.id.eq(2)));
      // The instance uses a bare bypass: no scope is set, and the shop 2 row is gone.
      await (doc as SoftDeletable).hardDelete();
      const rows = await docs.withoutDefaults().all();
      assert.deepEqual(rows.map((d: { id: number }) => d.id), [1]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`an upsert onto a soft-deleted row changes nothing (${dialect})`, { skip: !hasExtension("orm.soft-delete") }, async () => {
    const [db, models] = await open(dialect, `
model Doc {
  id      Int       @id
  title   String
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("tracking_node_upsert_soft_docs")
}`);
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insertMany([{ id: 1, title: "a" }]);
      await docs.filter(Doc.id.eq(1)).delete();
      await docs.insert({ id: 1, title: "b" }).onConflict(Doc.id, { update: true });
      const [row] = await docs.withoutDefaults().filter(Doc.id.eq(1)).all();
      assert.ok(row.title === "a" && row.deleted !== null);
      assert.equal(await docs.count(), 0);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a query-set bypass lets an upsert update a soft-deleted row (${dialect})`, { skip: !hasExtension("orm.soft-delete") }, async () => {
    const [db, models] = await open(dialect, `
model Doc {
  id      Int       @id
  title   String
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("tracking_node_upsert_bypass_docs")
}`);
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insertMany([{ id: 1, title: "a" }]);
      await docs.filter(Doc.id.eq(1)).delete();
      await docs.withoutDefaults("deleted").insert({ id: 1, title: "b" }).onConflict(Doc.id, { update: true });
      const [row] = await docs.withoutDefaults().filter(Doc.id.eq(1)).all();
      assert.ok(row.title === "b" && row.deleted !== null);
      await assert.rejects(Promise.resolve(docs.withoutDefaults("nope").insert({ id: 2, title: "c" })), /unknown default "nope"/);
    } finally { await db.dropTables(); await db.close(); }
  });

  for (const mode of ["application", "database"]) {
    test(`add restores a soft-deleted link and inserts the missing ones (${dialect}, ${mode})`, { skip: !hasExtension("orm.soft-delete") }, async () => {
      const [db, models] = await open(dialect, `
model Post {
  id   Int   @id
  tags Tag[] @relation(through: PostTag, through_fields: [post, tag])
  @@map("tracking_node_join_posts")
}
model Tag {
  id    Int    @id
  name  String
  posts Post[] @relation(through: PostTag, through_fields: [tag, post])
  @@map("tracking_node_join_tags")
}
model PostTag {
  id      Int       @id @default(autoincrement())
  post_id Int
  tag_id  Int
  post    Post      @relation(fields: [post_id], references: [id])
  tag     Tag       @relation(fields: [tag_id], references: [id])
  @@soft_delete.deleted_at("deleted", mode: "${mode}")
  @@map("tracking_node_join_post_tags")
}`);
      const { Post, Tag, PostTag } = models as Models & Record<string, any>;
      try {
        await Post.objects.using(db).insertMany([{ id: 1 }, { id: 2 }]);
        await Tag.objects.using(db).insertMany([1, 2, 3].map((id) => ({ id, name: String(id) })));
        const post = await Post.objects.using(db).get(Post.id.eq(1));
        const tagIds = async () => (await post.tags.orderBy(Tag.id).all()).map((t: { id: number }) => t.id);
        await post.tags.add(1, 2);
        await post.tags.remove(2);
        assert.deepEqual(await tagIds(), [1]);
        await post.tags.add(1, 2, 3);
        assert.deepEqual(await tagIds(), [1, 2, 3]);
        const links = async () => (await PostTag.objects.using(db).withoutDefaults().orderBy(PostTag.tagId).all()).map((l: { tagId: number; deleted: Date | null }) => [l.tagId, l.deleted === null]);
        assert.deepEqual(await links(), [[1, true], [2, true], [3, true]]);
        await post.tags.set([2]);
        await post.tags.set([1, 2]);
        assert.deepEqual(await links(), [[1, true], [2, true], [3, false]]);
      } finally { await db.dropTables(); await db.close(); }
    });
  }

  test(`a user default named deleted clashes with the soft-delete default (${dialect})`, { skip: !hasExtension("orm.soft-delete") || !capabilities.includes("query-defaults") }, async () => {
    await assert.rejects(open(dialect, `
model Doc {
  id      Int       @id
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@query.filter("deleted", "id > 0")
  @@map("tracking_node_clash_docs")
}`), /default name is set by both orm\.soft-delete default "deleted" and orm\.query-defaults default "deleted"/);
  });

  test(`a reference to a soft-deleted row loads null (${dialect})`, { skip: !hasExtension("orm.soft-delete") }, async () => {
    const [db, models] = await open(dialect, `
model Author {
  id      Int       @id
  books   Book[]
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("tracking_node_ref_authors")
}
model Book {
  id        Int    @id
  author_id Int
  author    Author @relation(fields: [author_id], references: [id])
  @@map("tracking_node_ref_books")
}`);
    const Author = models.Author!, Book = models.Book!;
    try {
      await Author.objects.using(db).insertMany([{ id: 1 }, { id: 2 }]);
      await Book.objects.using(db).insertMany([{ id: 1, authorId: 1 }, { id: 2, authorId: 2 }]);
      assert.equal(await Author.objects.using(db).filter(Author.id.eq(2)).delete(), 1);
      const joined = await Book.objects.using(db).load(Book.author).orderBy(Book.id.asc()).all();
      assert.deepEqual(joined.map((b: any) => b.author?.id ?? null), [1, null]);
      for (const [id, expected] of [[1, 1], [2, null]] as const) {
        const book = await Book.objects.using(db).get(Book.id.eq(id));
        await book.load(Book.author);
        assert.equal(book.author?.id ?? null, expected);
      }
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`an instance update after a query-set soft delete throws VersionConflict (${dialect})`, versionedSkip, async () => {
    const [db, models] = await open(dialect, versionedSoftDelete);
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insertMany([{ id: 1 }]);
      const doc = await docs.get(Doc.id.eq(1));
      assert.equal(await docs.filter(Doc.id.eq(1)).delete(), 1);
      await assert.rejects(doc.update({ deleted: null }), VersionConflict);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`the soft-delete members work on a two-word field (${dialect})`, { skip: !hasExtension("orm.soft-delete") }, async () => {
    const [db, models] = await open(dialect, `
model Page {
  id         Int       @id
  @@soft_delete.deleted_at(mode: "application")
  @@map("tracking_node_two_word_pages")
}`);
    const Page = models.Page!, pages = Page.objects.using(db);
    const ids = async (qs: any) => (await qs.orderBy(Page.id.asc()).all()).map((p: { id: number }) => p.id);
    try {
      await pages.insertMany([1, 2, 3, 4].map((id) => ({ id })));
      assert.equal(await pages.filter(Page.id.lt(4)).delete(), 3);
      assert.deepEqual(await ids(pages.deletedOnly()), [1, 2, 3]);
      assert.deepEqual(await ids(pages.allWithDeleted()), [1, 2, 3, 4]);
      assert.equal(await pages.filter(Page.id.eq(1)).undelete(), 1);
      const [back] = await pages.filter(Page.id.eq(2)).undelete({ returning: true });
      assert.ok(back.id === 2 && back.deletedAt === null);
      const held = await pages.deletedOnly().all();
      assert.equal((await (of(held) as any).hardDelete()).length, 1);
      const gone = await pages.allWithDeleted().get(Page.id.eq(4));
      await gone.delete();
      assert.notEqual(gone.deletedAt, null);
      await (gone as SoftDeletable).undelete();
      assert.equal(gone.deletedAt, null);
      await (gone as SoftDeletable).hardDelete();
      assert.deepEqual(await ids(pages.allWithDeleted()), [1, 2]);
    } finally { await db.dropTables(); await db.close(); }
  });

  const cascadeVersioned = `
model Author {
  id      Int       @id
  books   Book[]
  @@soft_delete.deleted_at("deleted", mode: "database")
  @@map("tracking_node_cascade_authors")
}
model Book {
  id        Int       @id
  author_id Int
  author    Author    @relation(fields: [author_id], references: [id], onDelete: Cascade)
  @@soft_delete.deleted_at("removed", mode: "database")
  @@locking.version
  @@map("tracking_node_cascade_books")
}`;
  const cascadeSkip = { skip: !hasExtension("orm.soft-delete") || !hasExtension("orm.locking") };

  test(`a hard delete of a soft-deleted parent leaves no orphan (${dialect})`, cascadeSkip, async () => {
    const [db, models] = await open(dialect, cascadeVersioned);
    const Author = models.Author!, authors = Author.objects.using(db), books = models.Book!.objects.using(db);
    try {
      await authors.insertMany([{ id: 1 }]);
      assert.equal(await authors.filter(Author.id.eq(1)).delete(), 1);
      await books.insertMany([{ id: 1, authorId: 1 }]);
      const author = await authors.allWithDeleted().get(Author.id.eq(1));
      if (dialect === "postgres") {
        await (author as SoftDeletable).hardDelete();
        assert.equal(await books.allWithDeleted().count(), 0);
        assert.equal(await authors.allWithDeleted().count(), 0);
      } else {
        await assert.rejects((author as SoftDeletable).hardDelete(), /FOREIGN KEY constraint failed/);
        assert.equal(await books.allWithDeleted().count(), 1);
        assert.equal(await authors.allWithDeleted().count(), 1);
      }
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`the soft-delete triggers bump the version (${dialect})`, cascadeSkip, async () => {
    const [db, models] = await open(dialect, cascadeVersioned);
    const Author = models.Author!, Book = models.Book!, authors = Author.objects.using(db), books = Book.objects.using(db);
    try {
      await authors.insertMany([{ id: 1 }, { id: 2 }]);
      await books.insertMany([{ id: 1, authorId: 1 }, { id: 2, authorId: 2 }]);
      const cascaded = await books.get(Book.id.eq(1));
      const raw = await books.get(Book.id.eq(2));
      assert.equal(await authors.filter(Author.id.eq(1)).delete(), 1);
      await db.execute("DELETE FROM tracking_node_cascade_books WHERE id = 2");
      const stored = (await books.allWithDeleted().orderBy(Book.id.asc()).all()).map((b: { version: number }) => b.version);
      assert.deepEqual(stored, [1, 1]);
      for (const stale of [cascaded, raw]) await assert.rejects(stale.update({ removed: null }), VersionConflict);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a second delete of the same instance throws nothing (${dialect})`, { skip: !hasExtension("orm.soft-delete") || !hasExtension("orm.locking") }, async () => {
    const [db, models] = await open(dialect, versionedSoftDelete);
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insertMany([{ id: 1 }]);
      const doc = await docs.get(Doc.id.eq(1));
      await doc.delete();
      await doc.delete();
      assert.equal(doc.version, 1);
      const stale = await docs.allWithDeleted().get(Doc.id.eq(1));
      await (stale as SoftDeletable).undelete();
      await assert.rejects(doc.delete(), VersionConflict);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a stale instance write throws VersionConflict (${dialect})`, { skip: !hasExtension("orm.locking") }, async () => {
    const [db, models] = await open(dialect, `
model Doc {
  id      Int    @id
  title   String
  @@locking.version
  @@map("tracking_node_docs")
}`);
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      const first = await docs.insert({ id: 1, title: "a" });
      const second = await docs.get(Doc.id.eq(1));
      await first.update({ title: "b" });
      assert.equal(first.version, 1);
      await assert.rejects(second.update({ title: "c" }), VersionConflict);
      await assert.rejects(second.delete(), VersionConflict);
      assert.equal(await docs.update({ title: "d" }), 1);
      assert.equal((await docs.get(Doc.id.eq(1))).version, 2);
      await second.load({ reload: true });
      await second.update({ title: "e" });
      assert.equal(second.version, 3);
      await second.delete();
      await assert.rejects(first.update({ title: "f" }), Doc.DoesNotExist);
      assert.equal(docs.deletedOnly, undefined);
    } finally { await db.dropTables(); await db.close(); }
  });

  const upsertDoc = ({ mode, version = false }: { mode?: string; version?: boolean }) => `
model Doc {
  id      Int      @id
  title   String
  ${mode === undefined ? "" : `@@timestamps.updated_at("changed", mode: "${mode}")`}
  ${version ? '@@locking.version' : ""}
  @@map("tracking_node_upsert_docs")
}`;

  test(`an upsert bumps the version (${dialect})`, { skip: !hasExtension("orm.locking") || !hasExtension("orm.timestamps") }, async () => {
    const [db, models] = await open(dialect, upsertDoc({ mode: "application", version: true }));
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insert({ id: 1, title: "a" });
      const stale = await docs.get(Doc.id.eq(1));
      await docs.insert({ id: 1, title: "b" }).onConflict(Doc.id, { update: true });
      assert.equal((await docs.get(Doc.id.eq(1))).version, 1);
      await docs.insert({ id: 1, title: "c" }).onConflict(Doc.id, { update: true, updateFields: [Doc.title] });
      await docs.insert({ id: 1, title: "d" }).onConflict(Doc.id, { update: true, updateValues: { title: "e" } });
      assert.equal((await docs.get(Doc.id.eq(1))).version, 3);
      await assert.rejects(stale.update({ title: "x" }), VersionConflict);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`an upsert sets updated_at in application mode (${dialect})`, { skip: !hasExtension("orm.timestamps") }, async () => {
    const [db, models] = await open(dialect, upsertDoc({ mode: "application" }));
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insert({ id: 1, title: "a", changed: OLD });
      await docs.insert({ id: 1, title: "b" }).onConflict(Doc.id, { update: true });
      assert.ok((await docs.get(Doc.id.eq(1))).changed > OLD);
      await docs.update({ changed: OLD });
      await docs.insert({ id: 1, title: "c", changed: OLD }).onConflict(Doc.id, { update: true, updateFields: [Doc.title, Doc.changed] });
      assert.equal((await docs.get(Doc.id.eq(1))).changed.getTime(), OLD.getTime());
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`the default upsert update list drops updated_at (${dialect})`, { skip: !hasExtension("orm.timestamps") }, async () => {
    const [db, models] = await open(dialect, upsertDoc({ mode: "application" }));
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insert({ id: 1, title: "a", changed: OLD });
      await docs.insert({ id: 1, title: "b", changed: OLD }).onConflict(Doc.id, { update: true });
      const row = await docs.get(Doc.id.eq(1));
      assert.ok(row.title === "b" && row.changed > OLD);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`an update: true upsert does not reset the version (${dialect})`, { skip: !hasExtension("orm.locking") }, async () => {
    const [db, models] = await open(dialect, upsertDoc({ version: true }));
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insert({ id: 1, title: "a" });
      await docs.filter(Doc.id.eq(1)).update({ title: "a2" });
      await docs.insert({ id: 1, title: "b", version: 0 }).onConflict(Doc.id, { update: true });
      assert.equal((await docs.get(Doc.id.eq(1))).version, 2);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`an upsert fires the updated_at trigger in database mode (${dialect})`, { skip: !hasExtension("orm.timestamps") }, async () => {
    const [db, models] = await open(dialect, upsertDoc({ mode: "database" }));
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      await docs.insert({ id: 1, title: "a", changed: OLD });
      await docs.insert({ id: 1, title: "b" }).onConflict(Doc.id, { update: true });
      assert.ok((await docs.get(Doc.id.eq(1))).changed > OLD);
    } finally { await db.dropTables(); await db.close(); }
  });
}

for (const dialect of ["sqlite", "postgres"]) {
  for (const mode of [null, "application", "database"]) {
    for (const operation of ["update", "delete"]) {
      test(`stale proxy instance checks the loaded version (${dialect}, ${operation}, ${mode})`, {
        skip: !capabilities.includes("proxy-models") || !hasExtension("orm.locking") || (mode !== null && !hasExtension("orm.soft-delete")),
      }, async () => {
        const [db, models] = await open(dialect, `
model Doc {
 id Int @id
 title String
 ${mode === null ? "" : `@@soft_delete.deleted_at("deleted", mode: "${mode}")`}
 @@locking.version
 @@map("tracking_node_proxy_docs")
}
model Draft {
 @@proxy.of(Doc)
}
model Review {
 @@proxy.of(Draft)
}`);
        const Doc = models.Doc!, Review = models.Review!, docs = Doc.objects.using(db);
        try {
          await docs.insertMany([{ id: 1, title: "original" }]);
          const stale = await Review.objects.using(db).get(Review.id.eq(1));
          await docs.filter(Doc.id.eq(1)).update({ title: "other writer" });
          await assert.rejects(() => operation === "update" ? stale.update({ title: "stale writer" }) : stale.delete(), VersionConflict);
          const stored = await docs.get(Doc.id.eq(1));
          assert.equal(stored.title, "other writer");
          assert.equal(stored.version, 1);
          await stale.load({ reload: true });
          await stale.update({ title: "fresh writer" });
          assert.equal(stale.version, 2);
          await stale.delete();
          if (mode === null) {
            assert.equal(await docs.count(), 0);
          } else {
            const deleted = await docs.withoutDefaults("deleted").get(Doc.id.eq(1));
            assert.ok(deleted.deleted !== null);
            assert.equal(deleted.version, 3);
          }
        } finally { await db.dropTables(); await db.close(); }
      });
    }
  }

  test(`instance rejects an explicit version (${dialect})`, { skip: !hasExtension("orm.locking") }, async () => {
    const [db, models] = await open(dialect, `
model Doc {
 id Int @id
 title String
 @@locking.version
 @@map("tracking_node_explicit_docs")
}`);
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      const doc = await docs.insert({ id: 1, title: "original" });
      await assert.rejects(() => doc.update({ title: "changed", version: 7 }), { name: "TypeError", message: /Doc.objects.filter.*update\(.*version/ });
      const stored = await docs.get(Doc.id.eq(1));
      assert.equal(stored.title, "original");
      assert.equal(stored.version, 0);
      assert.equal(doc.version, 0);
      await docs.filter(Doc.id.eq(1)).update({ version: 7 });
      assert.equal((await docs.get(Doc.id.eq(1))).version, 7);
    } finally { await db.dropTables(); await db.close(); }
  });
}

const TRACKING = [
  { extension: "orm.timestamps", attribute: "@@timestamps.updated_at", name: "updated_at", field: "updatedAt", customName: "edited_at", customField: "editedAt" },
  { extension: "orm.soft-delete", attribute: "@@soft_delete.deleted_at", name: "deleted_at", field: "deletedAt", customName: "removed_at", customField: "removedAt" },
  { extension: "orm.locking", attribute: "@@locking.version", name: "version", field: "version", customName: "revision", customField: "revision" },
];
const trackingSchema = (attribute: string) => `
model Story {
  id Int @id
  ${attribute}
  @@map("tracking_node_story")
}`;
const inheritedSchema = (attributes: string) => `
model Base {
  id    Int    @id
  title String
  ${attributes}
  @@inherit.abstract
}
model Mid {
  @@inherit.extends(["Base"])
  @@inherit.abstract
}
model Child {
  name String?
  @@inherit.extends(["Base"])
  @@map("tracking_node_inherited_child")
}
model Grandchild {
  @@inherit.extends(["Mid"])
  @@map("tracking_node_inherited_grandchild")
}`;
const trackingFields = async (source: string) => {
  const registry = new Registry();
  const models = await loads('datasource db { provider = "sqlite" }\n' + source, { registry }) as unknown as Models;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    return Object.keys(await models.Story!.objects.using(db).insert({ id: 1 })).filter((k) => k !== "id");
  } finally { await db.close(); }
};

for (const { extension, attribute, name, field, customName, customField } of TRACKING) {
  test(`${attribute} adds the field with its default or given name, TypeScript shows camelCase`, { skip: !hasExtension(extension) }, async () => {
    assert.ok((await trackingFields(trackingSchema(attribute))).includes(field));
    const custom = await trackingFields(trackingSchema(`${attribute}("${customName}")`));
    assert.ok(custom.includes(customField) && !custom.includes(field));
  });

  test(`${attribute} with a field of the same name is an error that names both`, { skip: !hasExtension(extension) }, () => {
    const source = `model Story {\n  id Int @id\n  ${customName} Int\n  ${attribute}("${customName}")\n}\n`;
    assert.throws(() => loads(source, { registry: new Registry() }), (e: Error) => e.message.includes(`Story.${customName}`) && e.message.includes(attribute.slice(2)) && e.message.includes("already exists"));
  });

  test(`${attribute} with a field on the same column is an error that names both`, { skip: !hasExtension(extension) }, () => {
    const source = `model Story {\n  id Int @id\n  taken Int @map("${name}")\n  ${attribute}\n}\n`;
    assert.throws(() => loads(source, { registry: new Registry() }), (e: Error) => e.message.includes(`adds the field Story.${name}, but the field Story.taken already uses the column ${name}`));
  });

  test(`${attribute} on an abstract or extends base reaches each concrete child`, { skip: !hasExtension(extension, "orm.inherit") }, async () => {
    const [db, models] = await open("sqlite", inheritedSchema(attribute));
    try {
      assert.deepEqual(Object.keys(models).sort(), ["Child", "Grandchild"]);
      for (const [id, model] of [[1, "Child"], [2, "Grandchild"]] as const) {
        assert.ok(field in await models[model]!.objects.using(db).insert({ id, title: "a" }), `${model} has ${field}`);
      }
    } finally { await db.dropTables(); await db.close(); }
  });
}

for (const model of ["Child", "Grandchild"]) {
  test(`a concrete child maintains the tracking fields of its base (${model})`, { skip: !hasExtension("orm.inherit", "orm.timestamps", "orm.soft-delete", "orm.locking") }, async () => {
    const attributes = '@@timestamps.updated_at("changed", mode: "application")\n  @@soft_delete.deleted_at(mode: "application")\n  @@locking.version';
    const [db, models] = await open("sqlite", inheritedSchema(attributes));
    const Model = models[model]!, rows = Model.objects.using(db);
    try {
      await rows.insert({ id: 1, title: "a", changed: OLD });
      await rows.filter(Model.id.eq(1)).update({ title: "b" });
      const row = await rows.get(Model.id.eq(1));
      assert.deepEqual([row.title, row.version], ["b", 1]);
      assert.ok(row.changed > OLD);
      assert.equal(await rows.filter(Model.id.eq(1)).delete(), 1);
      assert.equal(await rows.count(), 0);
      assert.equal(await rows.allWithDeleted().count(), 1);
    } finally { await db.dropTables(); await db.close(); }
  });
}

test("the added fields have the documented types and defaults", { skip: !hasExtension("orm.timestamps", "orm.soft-delete", "orm.locking") }, async () => {
  const [db, models] = await open("sqlite", `
model Story {
  id Int @id
  @@timestamps.updated_at
  @@soft_delete.deleted_at
  @@locking.version
  @@map("tracking_node_story_all")
}`);
  try {
    const row = await models.Story!.objects.using(db).insert({ id: 1 });
    assert.ok(row.updatedAt instanceof Date);
    assert.equal(row.deletedAt, null);
    assert.equal(row.version, 0);
  } finally { await db.dropTables(); await db.close(); }
});

for (const dialect of ["sqlite", "postgres"]) {
  for (const [upsert, kept] of [[undefined, false], ["drop", false], ["pick", true]] as const) {
    const argument = upsert === undefined ? "" : `, upsert: "${upsert}"`;
    const label = `upsert ${upsert ?? "default"} ${kept ? "keeps" : "drops"} the field in the default upsert list (${dialect})`;

    test(`timestamps ${label}`, { skip: !hasExtension("orm.timestamps") }, async () => {
      const [db, models] = await open(dialect, `
model Doc {
  id    Int    @id
  title String
  @@timestamps.updated_at("changed", mode: "application"${argument})
  @@map("tracking_node_timestamps_upsert")
}`);
      const Doc = models.Doc!, docs = Doc.objects.using(db);
      try {
        await docs.insert({ id: 1, title: "a", changed: OLD });
        await docs.insert({ id: 1, title: "b", changed: OLD }).onConflict(Doc.id, { update: true });
        const row = await docs.get(Doc.id.eq(1));
        assert.equal(row.title, "b");
        assert.equal(row.changed.getTime() === OLD.getTime(), kept);
        await docs.insert({ id: 1, title: "c", changed: OLD }).onConflict(Doc.id, { update: true, updateFields: [Doc.title, Doc.changed] });
        assert.equal((await docs.get(Doc.id.eq(1))).changed.getTime(), OLD.getTime());
      } finally { await db.dropTables(); await db.close(); }
    });

    test(`locking ${label}`, { skip: !hasExtension("orm.locking") }, async () => {
      const [db, models] = await open(dialect, `
model Doc {
  id    Int    @id
  title String
  @@locking.version("rev"${argument})
  @@map("tracking_node_locking_upsert")
}`);
      const Doc = models.Doc!, docs = Doc.objects.using(db);
      try {
        await docs.insert({ id: 1, title: "a", rev: 7 });
        await docs.insert({ id: 1, title: "b", rev: 3 }).onConflict(Doc.id, { update: true });
        const row = await docs.get(Doc.id.eq(1));
        assert.deepEqual([row.title, row.rev], ["b", kept ? 3 : 8]);
        await docs.insert({ id: 1, title: "c", rev: 5 }).onConflict(Doc.id, { update: true, updateFields: [Doc.title, Doc.rev] });
        assert.equal((await docs.get(Doc.id.eq(1))).rev, 5);
      } finally { await db.dropTables(); await db.close(); }
    });
  }
}

for (const dialect of ["sqlite", "postgres"]) {
  test(`a timestamps map argument keeps the live column (${dialect})`, { skip: !hasExtension("orm.timestamps") }, async () => {
    const [db, models] = await open(dialect, 'model Story {\n  id Int @id\n  @@timestamps.updated_at(map: "_updated_at")\n  @@map("tracking_node_story_map_ts")\n}\n');
    try {
      const stories = models.Story!.objects.using(db);
      await stories.insertMany([{ id: 1 }, { id: 2, updatedAt: OLD }]);
      await db.execute('DELETE FROM tracking_node_story_map_ts WHERE "_updated_at" IS NOT NULL AND id = 1');
      assert.deepEqual((await stories.all()).map((s: { id: number }) => s.id), [2]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a soft-delete map argument keeps the live column (${dialect})`, { skip: !hasExtension("orm.soft-delete") }, async () => {
    const [db, models] = await open(dialect, 'model Story {\n  id Int @id\n  @@soft_delete.deleted_at(map: "_deleted_at")\n  @@map("tracking_node_story_map_sd")\n}\n');
    try {
      const Story = models.Story!;
      const stories = Story.objects.using(db);
      const ids = async (qs: any) => (await qs.orderBy(Story.id.asc()).all()).map((s: { id: number }) => s.id);
      await stories.insertMany([{ id: 1 }, { id: 2 }, { id: 3 }]);
      assert.equal(await stories.filter(Story.id.eq(1)).delete(), 1);
      await db.execute('UPDATE tracking_node_story_map_sd SET "_deleted_at" = CURRENT_TIMESTAMP WHERE id = 2');
      assert.deepEqual(await ids(stories), [3]);
      await db.execute('DELETE FROM tracking_node_story_map_sd WHERE "_deleted_at" IS NOT NULL');
      assert.deepEqual(await ids(stories.allWithDeleted()), [3]);
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`a locking map argument keeps the live column (${dialect})`, { skip: !hasExtension("orm.locking") }, async () => {
    const [db, models] = await open(dialect, 'model Story {\n  id Int @id\n  @@locking.version(map: "rev")\n  @@map("tracking_node_story_map_lk")\n}\n');
    try {
      const Story = models.Story!;
      const stories = Story.objects.using(db);
      await stories.insertMany([{ id: 1 }, { id: 2, version: 3 }]);
      await db.execute("UPDATE tracking_node_story_map_lk SET rev = 7 WHERE id = 1");
      assert.equal((await stories.get(Story.id.eq(1))).version, 7);
      await db.execute("DELETE FROM tracking_node_story_map_lk WHERE rev = 3");
      assert.deepEqual((await stories.all()).map((s: { id: number }) => s.id), [1]);
    } finally { await db.dropTables(); await db.close(); }
  });
}

const OLD_FIELD_ATTRIBUTES = [
  { extension: "orm.timestamps", attribute: "timestamps.updated_at", old: "changed DateTime @default(now()) @timestamps.updated_at", field: "changed", line: '@@timestamps.updated_at("changed")', note: "" },
  { extension: "orm.timestamps", attribute: "timestamps.updated_at", old: 'updated_at DateTime @default(now()) @timestamps.updated_at(mode: "application")', field: "updated_at", line: '@@timestamps.updated_at(mode: "application")', note: "" },
  { extension: "orm.timestamps", attribute: "timestamps.updated_at", old: "editedAt DateTime @default(now()) @timestamps.updated_at", field: "editedAt", line: '@@timestamps.updated_at("edited_at", map: "editedAt")', note: "" },
  { extension: "orm.timestamps", attribute: "timestamps.updated_at", old: 'changed DateTime @default(now()) @map("_changed") @timestamps.updated_at(mode: "database")', field: "changed", line: '@@timestamps.updated_at("changed", map: "_changed", mode: "database")', note: "" },
  { extension: "orm.soft-delete", attribute: "soft_delete.deleted_at", old: "deleted_at DateTime? @soft_delete.deleted_at", field: "deleted_at", line: "@@soft_delete.deleted_at", note: "" },
  { extension: "orm.soft-delete", attribute: "soft_delete.deleted_at", old: 'removed DateTime? @soft_delete.deleted_at(mode: "database")', field: "removed", line: '@@soft_delete.deleted_at("removed", mode: "database")', note: "" },
  { extension: "orm.soft-delete", attribute: "soft_delete.deleted_at", old: "_deleted_at DateTime? @soft_delete.deleted_at", field: "_deleted_at", line: '@@soft_delete.deleted_at(map: "_deleted_at")', note: "" },
  { extension: "orm.soft-delete", attribute: "soft_delete.deleted_at", old: 'deleted_at DateTime? @map("_deleted_at") @soft_delete.deleted_at', field: "deleted_at", line: '@@soft_delete.deleted_at(map: "_deleted_at")', note: "" },
  { extension: "orm.locking", attribute: "locking.version", old: "version Int @default(0) @locking.version", field: "version", line: "@@locking.version", note: "" },
  { extension: "orm.locking", attribute: "locking.version", old: "rev Int @default(0) @locking.version", field: "rev", line: '@@locking.version("rev")', note: "" },
  { extension: "orm.locking", attribute: "locking.version", old: "version BigInt @default(0) @locking.version", field: "version", line: "@@locking.version", note: "; the added field has the type Int, not BigInt; change the type by hand" },
];

for (const { extension, attribute, old, field, line, note } of OLD_FIELD_ATTRIBUTES) {
  test(`the old field attribute "${old}" fails with the replacement line`, { skip: !hasExtension(extension) }, () => {
    assert.throws(
      () => loads(`model Story {\n  id Int @id\n  ${old}\n}\n`, { registry: new Registry() }),
      (e: Error) => e.message.endsWith(`@${attribute}: a field attribute is no longer supported; delete the field Story.${field} and write ${line} in the model block${note}`),
    );
  });
}
