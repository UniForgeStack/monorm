/** update() reads joined CTEs and to-one relations: it compiles to UPDATE ... FROM, on both databases. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, func, loads, param, Registry, UnfilteredWriteError } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";
import { DATABASE_URL } from "./helpers.js";

const schema = `
model Author {
  id Int @id
  name String
  nick String?
  post_count Int @default(0)
  posts Post[] @relation("a")
  edited Post[] @relation("e")
  @@map("uf_author")
}
model Post {
  id Int @id
  author_id Int
  editor_id Int?
  title String
  views Int @default(0)
  author_name String @default("")
  editor_name String?
  author Author @relation("a", fields: [author_id], references: [id])
  editor Author? @relation("e", fields: [editor_id], references: [id])
  @@map("uf_post")
}
`;

async function setup(provider: string) {
  const registry = new Registry();
  const models = loads(`datasource db {\n provider = "${provider}"\n}\n${schema}`, { registry });
  const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  const Author: any = models["Author"], Post: any = models["Post"];
  const A: any = Author.objects.using(db), P: any = Post.objects.using(db);
  await A.insertMany([{ id: 1, name: "A", nick: "aa" }, { id: 2, name: "B" }, { id: 3, name: "C" }]);
  await P.insertMany([
    { id: 1, authorId: 1, title: "p1", views: 5 },
    { id: 2, authorId: 1, title: "p2", views: 7 },
    { id: 3, authorId: 2, title: "p3", views: 1, editorId: 1 },
  ]);
  const totals = P.select({ authorId: Post.authorId, n: func.count() }).groupBy(Post.authorId).cte("totals");
  const sqlOf = async (statement: any): Promise<[any, string[]]> => {
    const sql: string[] = [];
    const remove = db.onQuery((e: any) => { sql.push(e.sql); });
    try {
      return [await statement, sql];
    } finally {
      remove();
    }
  };
  const counts = async () => Object.fromEntries((await A.all()).map((a: any) => [a.name, a.postCount]));
  return { db, Author, Post, A, P, totals, sqlOf, counts };
}

for (const provider of ["sqlite", "postgresql"]) {
  test(`update recounts from an aggregate cte (${provider})`, async () => {
    const { db, Author, A, totals, sqlOf, counts } = await setup(provider);
    try {
      const [count, sql] = await sqlOf(A.join(totals, totals.c.authorId.eq(Author.id)).update({ postCount: totals.c.n }));
      assert.equal(count, 2); // an inner join: Carol has no posts, her row stays out
      assert.equal(sql.length, 1);
      assert.ok(sql[0]!.includes('UPDATE "uf_author" SET "post_count" = "totals"."n" FROM "totals" WHERE '), sql[0]);
      assert.ok(!sql[0]!.split("UPDATE")[1]!.includes("SELECT") && !sql[0]!.includes("JOIN"), sql[0]);
      assert.deepEqual(await counts(), { A: 2, B: 1, C: 0 });
      // A filter reads the CTE too, and RETURNING reads the updated row.
      const rows = await A.join(totals, totals.c.authorId.eq(Author.id)).filter(totals.c.n.gt(1))
        .update({ postCount: totals.c.n.add(10) }).returning(Author.name, Author.postCount);
      assert.deepEqual(rows.map((r: any) => [r.name, r.postCount]), [["A", 12]]);
    } finally {
      await db.close();
    }
  });

  test(`update with an outer join keeps the rows without a match (${provider})`, async () => {
    const { db, Author, A, totals, sqlOf, counts } = await setup(provider);
    try {
      await A.all().update({ postCount: 7 });
      const statement = A.join(totals, totals.c.authorId.eq(Author.id), { outer: true }).all()
        .update({ postCount: func.coalesce(totals.c.n, 0) });
      const [count, sql] = await sqlOf(statement);
      assert.equal(count, 3);
      assert.ok(sql[0]!.includes('FROM "uf_author" AS "r1" LEFT JOIN "totals" ON "totals"."authorId" = "r1"."id" WHERE "uf_author"."id" = "r1"."id"'), sql[0]);
      assert.deepEqual(await counts(), { A: 2, B: 1, C: 0 });
      // A filter on the target narrows the self-join rows.
      const filtered = A.join(totals, totals.c.authorId.eq(Author.id), { outer: true }).filter(Author.id.eq(3))
        .update({ postCount: func.coalesce(totals.c.n, 5) });
      assert.equal(await filtered, 1);
      assert.deepEqual(await counts(), { A: 2, B: 1, C: 5 });
    } finally {
      await db.close();
    }
  });

  test(`update reads a not null relation with an inner join (${provider})`, async () => {
    const { db, Post, P, sqlOf } = await setup(provider);
    try {
      const [count, sql] = await sqlOf(P.all().update({ authorName: Post.author.name }));
      assert.equal(count, 3);
      assert.deepEqual(sql, ['UPDATE "uf_post" SET "author_name" = "j1"."name" FROM "uf_author" AS "j1" WHERE "j1"."id" = "uf_post"."author_id"']);
      assert.deepEqual((await P.all()).map((p: any) => [p.title, p.authorName]), [["p1", "A"], ["p2", "A"], ["p3", "B"]]);
    } finally {
      await db.close();
    }
  });

  test(`update reads a nullable relation and gives null without one (${provider})`, async () => {
    const { db, Post, P, sqlOf } = await setup(provider);
    try {
      await P.all().update({ editorName: "old" });
      const [rows, sql] = await sqlOf(P.all().update({ editorName: Post.editor.name }).returning(Post.id, Post.editorName));
      assert.ok(sql[0]!.includes('FROM "uf_post" AS "r1" LEFT JOIN "uf_author" AS "j2" ON "j2"."id" = "r1"."editor_id" WHERE "uf_post"."id" = "r1"."id"'), sql[0]);
      assert.ok(sql[0]!.includes('RETURNING "uf_post"."id", "uf_post"."editor_name"'), sql[0]);
      assert.deepEqual(rows.map((r: any) => [r.id, r.editorName]).sort((a: any, b: any) => a[0] - b[0]), [[1, null], [2, null], [3, "A"]]);
      assert.equal(P.filter(Post.id.eq(1)).update({ editorName: Post.editor.name }).inspect().statements.length, 1);
    } finally {
      await db.close();
    }
  });

  test(`a prepared update reads a joined cte (${provider})`, async () => {
    const { db, Author, A, totals, counts } = await setup(provider);
    try {
      const prepared = A.join(totals, totals.c.authorId.eq(Author.id), { outer: true })
        .filter(Author.id.gt(param("min")))
        .update({ postCount: func.coalesce(totals.c.n, 0) })
        .prepare();
      assert.equal(await prepared.run({ min: 1 }), 2);
      assert.deepEqual(await counts(), { A: 0, B: 1, C: 0 });
    } finally {
      await db.close();
    }
  });

  test(`update counts a to many relation in set (${provider})`, async () => {
    const { db, Author, A } = await setup(provider);
    try {
      // An aggregate of a to-many path compiles as load() compiles it, to a correlated subquery.
      assert.equal(await A.filter(Author.id.gt(0)).update({ postCount: func.count(Author.posts) }), 3);
      assert.deepEqual(Object.fromEntries((await A.all()).map((a: any) => [a.name, a.postCount])), { A: 2, B: 1, C: 0 });
    } finally {
      await db.close();
    }
  });

  test(`update join errors (${provider})`, async () => {
    const { db, Author, Post, A, P, totals } = await setup(provider);
    try {
      await assert.rejects(async () => { await A.all().update({ nick: Author.posts.title }); }, /a to-many relation can't give one value/);
      await assert.rejects(async () => { await A.from(Author.objects.cte("a")).all().update({ nick: "x" }); }, /can't run on a query set with from\(\)/);
      await assert.rejects(async () => { await A.join(totals, totals.c.authorId.eq(Author.id)).all().updateMany([{ id: 1, nick: "x" }]); }, /can't run on a query set with from\(\)/);
      // An inner join narrows the rows, so it counts as a filter; an outer join and a relation path do not.
      assert.equal(await A.join(totals, totals.c.authorId.eq(Author.id)).update({ nick: "n" }), 2);
      await assert.rejects(async () => { await A.join(totals, totals.c.authorId.eq(Author.id), { outer: true }).update({ nick: "n" }); }, UnfilteredWriteError);
      await assert.rejects(async () => { await P.update({ authorName: Post.author.name }); }, UnfilteredWriteError);
    } finally {
      await db.close();
    }
  });
}

const composed = `
model Person {
  id Int @id @default(autoincrement())
  name String
  @@map("uf_composed_person")
}
model Staff {
  salary Int
  @@composition.model(parent: "Person", parentRef: "person", childRef: "staff")
  @@map("uf_composed_staff")
}
`;

test("update refuses a join on a multi table model", { skip: !hasExtension("monorm.composition") }, async () => {
  const registry = new Registry();
  const models = loads(`datasource db {\n provider = "sqlite"\n}\n${composed}`, { registry });
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  try {
    const Staff: any = models["Staff"];
    const totals = Staff.objects.using(db).select({ salary: Staff.salary, n: func.count() }).groupBy(Staff.salary).cte("totals");
    await assert.rejects(async () => { await Staff.objects.using(db).join(totals, totals.c.salary.eq(Staff.salary)).update({ salary: 1 }); }, /multi-table model can't read a join/);
  } finally {
    await db.close();
  }
});
