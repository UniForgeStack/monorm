/** `.orNull()` / `.andNotNull()`: a NULL row kept or dropped, on PostgreSQL and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, QueryError, Registry } from "../src/index.js";
import { Comment, Post, User } from "./blog/models.js";

const source = `
model Reading {
  id    BigInt  @id @default(autoincrement())
  score Int?
  name  String?
  tag   String
  @@map("nm_node_reading")
}

model Item {
  shop  Int
  id    Int
  lines Line[]
  @@id([shop, id])
  @@map("nm_node_item")
}

model Line {
  id     Int   @id @default(autoincrement())
  shop   Int?
  itemId Int?
  item   Item? @relation(fields: [shop, itemId], references: [shop, id])
  @@map("nm_node_line")
}
`;

function where(qs: { sql(): string }): string {
  return qs.sql().split(" WHERE ")[1]!;
}

test("the method binds before not()", () => {
  const col = Comment.authorId;
  assert.equal(
    where(Comment.objects.filter(col.gt(20n).orNull().not())),
    'NOT ("comments"."author_id" > 20 OR "comments"."author_id" IS NULL)',
  );
  assert.equal(
    where(Comment.objects.filter(col.gt(20n).andNotNull().not())),
    'NOT ("comments"."author_id" > 20 AND "comments"."author_id" IS NOT NULL)',
  );
});

test("sql of each form", () => {
  const col = Comment.authorId;
  assert.equal(where(Comment.objects.filter(col.eq(1n).orNull())), '"comments"."author_id" = 1 OR "comments"."author_id" IS NULL');
  assert.equal(
    where(Comment.objects.filter(col.in([1n, 2n]).andNotNull())),
    '"comments"."author_id" IN (1, 2) AND "comments"."author_id" IS NOT NULL',
  );
  assert.equal(
    where(Comment.objects.filter(col.between(1n, 5n).orNull())),
    '("comments"."author_id" >= 1 AND "comments"."author_id" <= 5) OR "comments"."author_id" IS NULL',
  );
});

test("a precedence trap throws instead of guessing", () => {
  const col = Comment.authorId;
  assert.throws(() => col.gt(20n).not().orNull(), { name: "TypeError", message: /applies to one comparison/ });
  assert.throws(() => col.gt(1n).or(col.lt(0n)).orNull(), /applies to one comparison/);
  assert.throws(() => col.isNull().andNotNull(), /applies to one comparison/);
});

test("a NOT NULL column throws", () => {
  assert.throws(() => User.email.eq("a").orNull(), (e) => e instanceof QueryError && /NOT NULL/.test(e.message));
  assert.throws(() => User.email.like("a%").andNotNull(), /NOT NULL/);
});

test("a column reached through a relation throws for now", () => {
  assert.throws(() => Post.comments.authorId.eq(1n).orNull(), /relation/);
});

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"];

  async function setup(): Promise<{ db: any; Reading: any; Item: any; Line: any }> {
    const registry = new Registry();
    const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${source}`, { registry });
    const db = await connect(url!, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    return { db, Reading: models["Reading"], Item: models["Item"], Line: models["Line"] };
  }

  test(`a NULL row is kept or dropped (${provider})`, { skip: !url }, async () => {
    const { db, Reading } = await setup();
    try {
      const [a, b, nul] = await Reading.objects
        .using(db)
        .insertMany([
          { score: 10, name: "ab", tag: "t" },
          { score: 30, name: "cd", tag: "t" },
          { score: null, name: null, tag: "t" },
        ])
        .returning();
      const s = Reading.score;
      const n = Reading.name;
      // [condition, the non-NULL rows it matches]
      const cases: [any, any[]][] = [
        [s.eq(10), [a]],
        [s.ne(10), [b]],
        [s.lt(20), [a]],
        [s.gt(20), [b]],
        [s.in([10]), [a]],
        [s.notIn([10]), [b]],
        [s.between(5, 15), [a]],
        [n.like("a%"), [a]],
        [n.ilike("A%"), [a]],
        [n.contains("d"), [b]],
      ];
      const ids = async (c: any) => (await Reading.objects.using(db).filter(c).all()).map((r: any) => Number(r.id)).sort();
      const ofRows = (rows: any[]) => rows.map((r) => Number(r.id)).sort();
      for (const [c, matched] of cases) {
        const m = ofRows(matched);
        const rest = ofRows([a, b].filter((r) => !matched.includes(r)));
        const withNull = ofRows([...matched, nul]);
        const restWithNull = ofRows([...[a, b].filter((r) => !matched.includes(r)), nul]);
        const label = c.constructor.name;
        assert.deepEqual(await ids(c), m, label);
        assert.deepEqual(await ids(c.orNull()), withNull, label);
        assert.deepEqual(await ids(c.andNotNull()), m, label);
        assert.deepEqual(await ids(c.not()), rest, label);
        assert.deepEqual(await ids(c.orNull().not()), rest, label);
        assert.deepEqual(await ids(c.andNotNull().not()), restWithNull, label);
      }
      const excluded = async (c: any) => (await Reading.objects.using(db).exclude(c).all()).map((r: any) => Number(r.id)).sort();
      assert.deepEqual(await excluded(s.gt(20).andNotNull()), ofRows([a, nul]));
      assert.deepEqual(await excluded(s.gt(20).orNull()), ofRows([a]));
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`a composite key has one IS NULL per column (${provider})`, { skip: !url }, async () => {
    const { db, Item, Line } = await setup();
    try {
      const [a] = await Item.objects.using(db).insertMany([{ shop: 1, id: 1 }, { shop: 2, id: 1 }]).returning();
      const [one, two, nul, half] = await Line.objects
        .using(db)
        .insertMany([{ item: a }, { shop: 2, itemId: 1 }, { shop: null, itemId: null }, { shop: 1, itemId: null }])
        .returning();
      const ids = async (c: any) => (await Line.objects.using(db).filter(c).all()).map((r: any) => r.id).sort();
      const of = (...rows: any[]) => rows.map((r) => r.id).sort();
      const eq = Line.item.eq(a);
      const ne = Line.item.ne(a);
      assert.equal(where(Line.objects.using(db).filter(eq.orNull())).split(" IS NULL").length - 1, 2);
      assert.deepEqual(await ids(eq), of(one));
      assert.deepEqual(await ids(eq.orNull()), of(one, nul, half));
      assert.deepEqual(await ids(eq.andNotNull()), of(one));
      assert.deepEqual(await ids(ne), of(two));
      assert.deepEqual(await ids(ne.orNull()), of(two, nul, half));
      assert.deepEqual(await ids(ne.andNotNull()), of(two));
      assert.deepEqual(await ids(eq.orNull().not()), of(two));
      assert.deepEqual(await ids(eq.andNotNull().not()), of(two, nul, half));
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}
