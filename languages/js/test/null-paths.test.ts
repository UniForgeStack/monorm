/** The null rule on relation paths: `rel.ne(x)`, a NOT NULL or nullable target column, the path-level
 * rule, and the consumers (join `on`, aggregate `filter`), on PostgreSQL and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, func, loads, not, Registry } from "../src/index.js";
import { Comment, Post, User } from "./blog/models.js";

const source = `
model Writer {
  id    Int     @id @default(autoincrement())
  email String
  nick  String?
  notes Note[]
  @@map("np_node_writer")
}

model Note {
  id       Int     @id @default(autoincrement())
  writerId Int?
  writer   Writer? @relation(fields: [writerId], references: [id])
  score    Int?
  @@map("np_node_note")
}
`;

const KEEPS = /orNull\(\) keeps NULL rows/;
const DROPS = /under negation/;

test("a column ne keeps the column rule in a join on and an aggregate filter", () => {
  const totals = Post.objects.select({ authorId: Post.authorId, views: func.sum(Post.views) }).groupBy(Post.authorId).cte("totals");
  assert.throws(() => Comment.objects.join(totals, Comment.authorId.ne(1n)), KEEPS);
  assert.ok(Comment.objects.join(totals, Comment.authorId.ne(1n).orNull()).sql());
  assert.throws(() => func.count({ filter: Comment.authorId.ne(1n) } as never), KEEPS);
  assert.ok(func.count({ filter: Comment.authorId.ne(1n).andNotNull() } as never));
});

test("the check does not run in a case or a select", () => {
  assert.ok(Comment.objects.select({ flag: func.case([Comment.authorId.ne(1n), 1], { default: 0 }) }).sql());
});

test("a to-one notIn a subquery and a mixed path flip in sql", () => {
  const sub = User.objects.select({ email: User.email }).filter(User.name.eq("x"));
  let sql = Comment.objects.filter(Comment.author.email.notIn(sub as never)).sql();
  assert.ok(sql.includes('WHERE NOT EXISTS(SELECT 1 FROM "users" AS "t1"') && sql.includes('"t1"."email" IN (SELECT'), sql);
  sql = Post.objects.filter(Post.comments.author.email.ne("x")).sql();
  assert.ok(sql.includes('"t1"."post_id" = "posts"."id" AND (NOT EXISTS(SELECT 1 FROM "users" AS "t2"'), sql);
});

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"];

  async function setup() {
    const registry = new Registry();
    const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${source}`, { registry });
    const db = await connect(url!, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    const Writer: any = models["Writer"];
    const Note: any = models["Note"];
    const [w1, w2, w3] = await Writer.objects
      .using(db)
      .insertMany([{ email: "a", nick: "n" }, { email: "b", nick: "m" }, { email: "c", nick: null }])
      .returning();
    const [n1, n2, n3, n4, n5] = await Note.objects
      .using(db)
      .insertMany([
        { writerId: w1.id, score: 1 },
        { writerId: w2.id, score: 2 },
        { writerId: null, score: 3 },
        { writerId: w3.id, score: 1 },
        { writerId: w1.id, score: null },
      ])
      .returning();
    const pick = async (model: any, c: any) => (await model.objects.using(db).filter(c).all()).map((r: any) => r.id).sort();
    return {
      db,
      Writer,
      Note,
      w: [w1, w2, w3],
      n: [n1, n2, n3, n4, n5],
      notes: (c: any) => pick(Note, c),
      writers: (c: any) => pick(Writer, c),
    };
  }
  const of = (...rows: any[]) => rows.map((r) => r.id).sort();

  async function run(name: string, body: (s: Awaited<ReturnType<typeof setup>>) => Promise<void>) {
    test(`${name} (${provider})`, { skip: !url }, async () => {
      const s = await setup();
      try {
        await body(s);
      } finally {
        await s.db.dropTables();
        await s.db.close();
      }
    });
  }

  await run("a to-one relation ne keeps the rows with no related row", async ({ Note, w: [w1], n: [n1, n2, n3, n4, n5], notes }) => {
    assert.deepEqual(await notes(Note.writer.eq(w1)), of(n1, n5));
    assert.deepEqual(await notes(Note.writer.ne(w1)), of(n2, n3, n4));
    assert.deepEqual(await notes(not(Note.writer.eq(w1))), of(n2, n3, n4));
    assert.deepEqual(await notes(not(Note.writer.ne(w1))), of(n1, n5));
    assert.deepEqual(await notes(Note.writer.ne(null)), of(n1, n2, n4, n5));
  });

  await run("a column ne keeps the column rule", async ({ Note, w: [w1], n: [, n2, n3, n4], notes }) => {
    await assert.rejects(async () => notes(Note.writerId.ne(w1.id)), KEEPS);
    assert.deepEqual(await notes(Note.writerId.ne(w1.id).orNull()), of(n2, n3, n4));
    assert.deepEqual(await notes(Note.writerId.ne(w1.id).andNotNull()), of(n2, n4));
  });

  await run("a NOT NULL target column is not a nullable value", async ({ Note, n: [n1, n2, n3, n4, n5], notes }) => {
    assert.deepEqual(await notes(Note.writer.email.eq("a")), of(n1, n5));
    assert.deepEqual(await notes(Note.writer.email.ne("a")), of(n2, n3, n4));
    assert.deepEqual(await notes(Note.writer.email.notIn(["a"])), of(n2, n3, n4));
    assert.deepEqual(await notes(not(Note.writer.email.eq("a"))), of(n2, n3, n4));
    assert.deepEqual(await notes(Note.writer.email.ne("a").orNull()), of(n2, n3, n4));
    assert.deepEqual(await notes(Note.writer.email.ne("a").andNotNull()), of(n2, n4));
  });

  await run("a nullable target column takes the strict rule and the methods", async ({ Note, n: [n1, n2, n3, n4, n5], notes }) => {
    await assert.rejects(async () => notes(Note.writer.nick.ne("n")), KEEPS);
    await assert.rejects(async () => notes(not(Note.writer.nick.eq("n"))), DROPS);
    assert.deepEqual(await notes(Note.writer.nick.eq("n")), of(n1, n5));
    assert.deepEqual(await notes(Note.writer.nick.ne("n").orNull()), of(n2, n3, n4));
    assert.deepEqual(await notes(Note.writer.nick.ne("n").andNotNull()), of(n2));
    assert.deepEqual(await notes(Note.writer.nick.eq("n").orNull().not()), of(n2));
    assert.deepEqual(await notes(Note.writer.nick.eq("n").andNotNull().not()), of(n2, n3, n4));
  });

  await run("the path-level rule covers the missing row and every nullable column", async ({ Note, w: [w1], n: [n1, n2, n3, n4, n5], notes }) => {
    assert.deepEqual(await notes(Note.writer.orNull().nick.ne("n")), of(n2, n3, n4));
    assert.deepEqual(await notes(Note.writer.andNotNull().nick.ne("n")), of(n2));
    assert.deepEqual(await notes(not(Note.writer.andNotNull().nick.eq("n"))), of(n2, n3, n4));
    assert.deepEqual(await notes(not(Note.writer.orNull().nick.eq("n"))), of(n2));
    assert.deepEqual(await notes(Note.writer.orNull().ne(w1)), of(n2, n3, n4));
    assert.deepEqual(await notes(Note.writer.andNotNull().ne(w1)), of(n2, n4));
    assert.deepEqual(await notes(Note.writer.andNotNull().eq(w1)), of(n1, n5));
  });

  await run("a method on one comparison wins over the path", async ({ Note, n: [, n2, n3, n4], notes }) => {
    assert.deepEqual(await notes(Note.writer.orNull().nick.ne("n").andNotNull()), of(n2));
    assert.deepEqual(await notes(Note.writer.andNotNull().nick.ne("n").orNull()), of(n2, n3, n4));
  });

  await run("the methods apply per related row on a to-many path", async ({ Writer, w: [w1, w2, w3], writers }) => {
    await assert.rejects(async () => writers(Writer.notes.score.ne(1)), KEEPS);
    assert.deepEqual(await writers(Writer.notes.score.ne(1).orNull()), of(w1, w2));
    assert.deepEqual(await writers(Writer.notes.score.ne(1).andNotNull()), of(w2));
    assert.deepEqual(await writers(Writer.notes.orNull().score.ne(1)), of(w1, w2));
    assert.deepEqual(await writers(Writer.notes.andNotNull().score.ne(1)), of(w2));
    assert.deepEqual(await writers(not(Writer.notes.score.eq(2).andNotNull())), of(w1, w3));
    assert.deepEqual(await writers(not(Writer.notes.score.eq(2).orNull())), of(w3));
  });
}
