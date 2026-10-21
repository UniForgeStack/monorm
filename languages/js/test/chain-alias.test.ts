/** One to-many path is one related row for the whole query; `alias()` is another, on PostgreSQL and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry } from "../src/index.js";

const source = `
model Author {
  id    Int    @id @default(autoincrement())
  name  String
  posts Post[]
  @@map("ca_node_author")
}

model Post {
  id        Int     @id @default(autoincrement())
  authorId  Int
  author    Author  @relation(fields: [authorId], references: [id])
  views     Int
  published Boolean
  notes     Note[]
  @@map("ca_node_post")
}

model Note {
  id     Int    @id @default(autoincrement())
  postId Int
  post   Post   @relation(fields: [postId], references: [id])
  body   String
  @@map("ca_node_note")
}
`;

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"];

  test(`one path is one related row; alias() is another (${provider})`, { skip: !url }, async () => {
    const registry = new Registry();
    const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${source}`, { registry });
    const db = await connect(url!, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    try {
      const Author: any = models["Author"];
      const Post: any = models["Post"];
      const Note: any = models["Note"];
      const [a1, a2] = await Author.objects.using(db).insertMany([{ name: "a1" }, { name: "a2" }, { name: "a3" }]).returning();
      // a1: a popular published post and an unpopular unpublished one; a2: a popular unpublished post; a3: none
      const [p1, p2, p3] = await Post.objects
        .using(db)
        .insertMany([
          { authorId: a1.id, views: 20, published: true },
          { authorId: a1.id, views: 5, published: false },
          { authorId: a2.id, views: 20, published: false },
        ])
        .returning();
      await Note.objects
        .using(db)
        .insertMany([{ postId: p1.id, body: "x" }, { postId: p2.id, body: "y" }, { postId: p3.id, body: "x" }, { postId: p3.id, body: "y" }])
        .returning();
      const names = async (qs: any): Promise<string[]> => (await qs.using(db).all()).map((a: any) => a.name).sort();
      const P = Author.posts;

      // one path is one row, in one call or across calls
      assert.deepEqual(await names(Author.objects.filter(P.views.gt(10), P.published.eq(false))), ["a2"]);
      assert.deepEqual(await names(Author.objects.filter(P.views.gt(10)).filter(P.published.eq(false))), ["a2"]);
      // alias() is another row
      assert.deepEqual(await names(Author.objects.filter(P.views.gt(10), P.alias().published.eq(false))), ["a1", "a2"]);
      const other = P.alias();
      assert.deepEqual(await names(Author.objects.filter(other.views.gt(10)).filter(other.published.eq(false))), ["a2"]);
      // or and not are barriers
      assert.deepEqual(await names(Author.objects.filter(P.views.gt(10)).exclude(P.published.eq(true))), ["a2"]);
      const either = P.published.eq(false).or(P.views.lt(10));
      assert.deepEqual(await names(Author.objects.filter(P.views.gt(10), either)), ["a1", "a2"]);
      // the shared row holds inside a deeper path
      assert.deepEqual(await names(Author.objects.filter(P.notes.body.eq("x"), P.notes.body.eq("y"))), []);
      assert.deepEqual(await names(Author.objects.filter(P.notes.body.eq("x"), P.notes.alias().body.eq("y"))), ["a2"]);
      assert.deepEqual(await names(Author.objects.filter(P.notes.body.eq("x"), P.alias().notes.body.eq("y"))), ["a1", "a2"]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}
