/** `.orNull()` / `.andNotNull()`: a NULL row kept or dropped, on PostgreSQL and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { and, connect, exists, excluded, func, loads, not, or, outer, param, QueryError, Registry } from "../src/index.js";
import { differs, Func } from "../src/expr.js";
import { Comment, Post, Profile, User } from "./blog/models.js";

const source = `
model Reading {
  id    BigInt  @id @default(autoincrement())
  score Int?
  name  String?
  tag   String
  meta  Json?
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

model Note {
  id     Int         @id @default(autoincrement())
  labels Label[]     @relation(through: NoteLabel)
  links  NoteLabel[]
  @@map("nm_node_note")
}

model Label {
  id    Int         @id @default(autoincrement())
  notes Note[]      @relation(through: NoteLabel)
  links NoteLabel[]
  @@map("nm_node_label")
}

model NoteLabel {
  id      Int    @id @default(autoincrement())
  noteId  Int
  labelId Int?
  note    Note   @relation(fields: [noteId], references: [id])
  label   Label? @relation(fields: [labelId], references: [id])
  @@map("nm_node_note_label")
}
`;

const arraySource = `
model Tagged {
  id    BigInt   @id @default(autoincrement())
  links String[] @default([])
  @@map("nm_node_tagged")
}
`;

function where(qs: { sql(): string }): string {
  return qs.sql().split(" WHERE ").slice(1).join(" WHERE ");
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
  assert.throws(() => Comment.id.gt(20n).not().orNull(), { name: "TypeError", message: /applies to one comparison/ });
  assert.throws(() => col.gt(1n).or(col.lt(0n)).orNull(), /applies to one comparison/);
  assert.throws(() => col.isNull().andNotNull(), /already a NULL test/);
  assert.throws(() => col.ne(null).orNull(), /Comment\.authorId IS NOT NULL/);
});

test("a NOT NULL column throws", () => {
  assert.throws(() => User.email.eq("a").orNull(), (e) => e instanceof QueryError && /NOT NULL/.test(e.message));
  assert.throws(() => User.email.like("a%").andNotNull(), /NOT NULL/);
});

test("a nullable column reached through a to-many relation takes the method", () => {
  assert.equal(where(Post.objects.filter(Post.comments.authorId.eq(1n).orNull())).split('"author_id" IS NULL').length - 1, 1);
  assert.throws(() => Post.comments.id.eq(1n).orNull(), /NOT NULL/);
});

test("a computed value takes the method", () => {
  const x = Comment.authorId;
  assert.ok(where(Comment.objects.filter(x.add(1n).gt(5n).orNull())).endsWith('("comments"."author_id" + 1) IS NULL'));
  assert.ok(where(Comment.objects.filter(func.abs(x).gt(5n).andNotNull())).endsWith("IS NOT NULL"));
  assert.ok(where(Comment.objects.filter(func.max(x).gt(5n).orNull())).endsWith("IS NULL"));
  for (const nonNull of [func.coalesce(x, 0n).gt(1n), func.count().gt(1n), func.abs(Comment.id).gt(1n)]) {
    assert.throws(() => nonNull.orNull(), (e) => e instanceof QueryError && /the value is NOT NULL/.test(e.message));
    assert.throws(() => nonNull.andNotNull(), /the value is NOT NULL/);
  }
  assert.throws(() => func.max(x).isNull().orNull(), /already a NULL test/);
});

test("the strict rule runs on a computed value", () => {
  const x = Comment.authorId;
  assert.ok(Comment.objects.filter(func.coalesce(x, 0n).ne(3n)).sql());
  assert.ok(Comment.objects.filter(func.coalesce(x, Comment.id).ne(3n)).sql());
  assert.ok(Comment.objects.filter(not(func.abs(Comment.id).eq(3n))).sql());
  for (const build of [
    () => Comment.objects.filter(func.coalesce(x, x).ne(3n)),
    () => Comment.objects.filter(func.abs(x).ne(3n)),
    () => Comment.objects.filter(func.max(x).ne(3n)),
    () => Comment.objects.filter(func.sum(x).ne(3n)),
    () => Comment.objects.filter(not(func.abs(x).eq(3n))),
    () => Comment.objects.exclude(func.abs(x).eq(3n)),
    () => Comment.objects.filter(func.abs(x).notIn([1n, 2n])),
  ]) {
    assert.throws(build, /orNull/);
  }
});

test("a scalar subquery is nullable", () => {
  const latest: any = Post.objects.filter(Post.authorId.eq(outer(User.id))).select({ id: Post.id }).limit(1).asScalar();
  assert.throws(() => User.objects.filter(latest.ne(3n) as never), /orNull\(\) keeps NULL rows/);
  assert.ok(User.objects.filter(latest.ne(3n).orNull() as never).sql());
  assert.throws(() => not(latest.eq(3n) as never), /under not\(\)/);
});

test("a having condition on an aggregate follows the rule", () => {
  const q = Comment.objects.select({ a: Comment.authorId, n: func.count() }).groupBy(Comment.authorId);
  assert.ok(q.having(func.count().ne(0n)).sql());
  assert.throws(() => q.having(func.max(Comment.id).ne(3n)), /orNull\(\) keeps NULL rows/);
  assert.throws(() => q.having(not(func.sum(Comment.id).gt(3n))), /under not\(\)/);
  assert.ok(q.having(func.max(Comment.id).ne(3n).orNull()).sql());
});

test("the strict rule runs in a join on and an aggregate filter on a computed value", () => {
  const totals = Post.objects.select({ authorId: Post.authorId, views: func.sum(Post.views) }).groupBy(Post.authorId).cte("totals");
  assert.throws(() => Comment.objects.join(totals, func.abs(Comment.authorId).ne(1n)), /orNull\(\) keeps NULL rows/);
  assert.ok(Comment.objects.join(totals, func.coalesce(Comment.authorId, 0n).ne(1n)).sql());
  assert.throws(() => func.count({ filter: func.abs(Comment.authorId).ne(1n) } as never), /orNull\(\) keeps NULL rows/);
  assert.throws(() => func.count({ filter: not(func.max(Comment.id).eq(1n)) } as never), /under not\(\)/);
  assert.ok(func.count({ filter: func.coalesce(Comment.authorId, 0n).ne(1n) } as never));
});

test("the strict rule does not run in case or select on a computed value", () => {
  const x = Comment.authorId;
  assert.ok(Comment.objects.select({ f: func.case([func.abs(x).ne(1n), 1], { default: 0 }) }).sql());
  assert.ok(Comment.objects.select({ f: func.abs(x).ne(1n) as never }).sql());
});

test("a path null rule reaches a condition nested in a function", () => {
  const plain = where(Post.objects.filter(func.coalesce(Post.author.name.eq("x"), false as never).eq(true as never) as never));
  const ruled = where(Post.objects.filter(func.coalesce(Post.author.orNull().name.eq("x"), false as never).eq(true as never) as never));
  assert.ok(ruled.includes('NOT "t1"."name" IS NOT NULL'));
  assert.ok(!plain.includes("IS NOT NULL"));
});

test("a bound parameter and a constant are not nullable", () => {
  const prepared = Post.objects.filter(Post.id.ne(param("x") as never)).prepare();
  assert.equal(where({ sql: () => prepared.sql({ x: 3 }) }), '"posts"."id" <> 3');
  assert.equal(where(Post.objects.filter(not(func.abs(Post.id).eq(3n)))), 'NOT (ABS("posts"."id")) = 3');
});

test("an array index is nullable", () => {
  const first = Profile.links.element(1);
  const fifth = Profile.links.element(5);
  assert.throws(() => Profile.objects.filter(fifth.ne("a")), /orNull\(\) keeps NULL rows/);
  assert.throws(() => not(first.eq("a")), /under not\(\)/);
  assert.ok(where(Profile.objects.filter(fifth.eq("a").orNull())).endsWith("IS NULL"));
  assert.ok(where(Profile.objects.filter(fifth.eq("a").andNotNull())).endsWith("IS NOT NULL"));
});

test("a function is nullable unless it is known to keep a non-null input non-null", () => {
  for (const kept of [func.lower(User.email), func.length(User.email), func.concat(User.email, "x"), func.abs(Comment.id)] as any[]) {
    assert.throws(() => kept.eq("a").orNull(), /the value is NOT NULL/);
    assert.ok(Comment.objects.filter(kept.ne("a")));
  }
  for (const unknown of [new Func("round", [Comment.id]), new Func("sign", [Comment.id]), func.sum(Comment.id)] as any[]) {
    assert.throws(() => Comment.objects.filter(unknown.ne(1n)), /orNull\(\) keeps NULL rows/);
    assert.ok(Comment.objects.filter(unknown.ne(1n).orNull()));
  }
});

test("a division is nullable and stays non-strict", () => {
  const x: any = Comment.id;
  assert.ok(where(Comment.objects.filter(x.div(2n).gt(1n).orNull())).endsWith("IS NULL"));
  assert.ok(where(Comment.objects.filter(x.div(2n).eq(1n).andNotNull())).endsWith("IS NOT NULL"));
  assert.ok(Comment.objects.filter(x.div(2n).ne(1n)));
  assert.throws(() => x.add(2n).gt(1n).orNull(), /the value is NOT NULL/);
});

test("a value over a to-one path is nullable and flips like the column", () => {
  const lower: any = func.lower(Comment.author.name);
  assert.ok(where(Comment.objects.filter(Comment.author.name.ne("x"))).includes("NOT EXISTS"));
  assert.equal(where(Comment.objects.filter(lower.ne("x"))).split("NOT EXISTS").length - 1, 1);
  assert.equal(where(Comment.objects.filter(lower.notIn(["x"]))).split("NOT EXISTS").length - 1, 1);
  assert.ok(Comment.objects.filter(lower.ne("x").orNull()));
});

test("a path null rule reaches the whole value, not only the column", () => {
  const author: any = Comment.author.orNull();
  const plain = where(Comment.objects.filter(func.lower(Comment.author.name).eq("x") as never));
  const ruled = where(Comment.objects.filter(func.lower(author.name).eq("x") as never));
  assert.ok(!plain.includes("IS NOT NULL"));
  assert.ok(ruled.includes('NOT EXISTS(SELECT 1 FROM "users" AS "t1" WHERE "t1"."id" = "comments"."author_id" AND (LOWER("t1"."name")) IS NOT NULL)'), ruled);
});

test("an outer() column is strict", () => {
  const col = outer(Comment.authorId);
  assert.throws(() => Post.objects.filter(Post.id.ne(col as never)), /orNull\(\) keeps NULL rows/);
  assert.ok(Post.objects.filter(Post.id.ne(col as never).orNull()));
  assert.ok(Post.objects.filter(Post.id.ne(outer(Comment.id) as never)));
});

test("two different computed values are two values for the strict rule", () => {
  const sub = (n: number): any => Post.objects.filter(Post.authorId.eq(outer(User.id))).select({ id: Post.id }).limit(n).asScalar();
  const cases: [string, any, any][] = [
    ["scalar subquery", sub(1), sub(2)],
    ["case", func.case([Comment.authorId.gt(1n), 1]), func.case([Comment.authorId.gt(2n), 1])],
    ["window", func.lag(Comment.authorId).over({ orderBy: Comment.id }), func.lag(Comment.authorId).over({ orderBy: Comment.id })],
  ];
  for (const [label, a, b] of cases) {
    const model: any = label === "scalar subquery" ? User : Comment;
    assert.throws(() => model.objects.filter(or(a.ne(3n), b.isNull()) as never), /orNull\(\) keeps NULL rows/, label);
    assert.ok(model.objects.filter(or(a.ne(3n), a.isNull()) as never), label);
  }
});

test("the message names a subquery, a case and a window value", () => {
  const sub: any = Post.objects.filter(Post.authorId.eq(outer(User.id))).select({ id: Post.id }).limit(1).asScalar();
  assert.throws(() => User.objects.filter(sub.ne(3n)), (e) => e instanceof QueryError && /subquery/.test(e.message) && !/\[object/.test(e.message));
  const kase: any = func.case([Comment.authorId.gt(1n), 1]);
  assert.throws(() => Comment.objects.filter(kase.ne(3n)), (e) => e instanceof QueryError && /case\(/.test(e.message) && !/\[object/.test(e.message));
  const win: any = func.lag(Comment.authorId).over({ orderBy: Comment.id });
  assert.throws(() => Comment.objects.filter(win.ne(3n)), (e) => e instanceof QueryError && /lag\(/.test(e.message) && !/\[object/.test(e.message));
});

test("a constant in() keeps its column", () => {
  const col = Comment.authorId;
  assert.equal(where(Comment.objects.filter(col.in([]).orNull())), '(FALSE) OR "comments"."author_id" IS NULL');
  assert.equal(where(Comment.objects.filter(col.notIn([]).orNull())), '(TRUE) OR "comments"."author_id" IS NULL');
  assert.equal(where(Comment.objects.filter(col.in([]).andNotNull())), '(FALSE) AND "comments"."author_id" IS NOT NULL');
  assert.equal(where(Comment.objects.filter(col.notIn([]).andNotNull())), '(TRUE) AND "comments"."author_id" IS NOT NULL');
  assert.throws(() => User.email.in([]).orNull(), /NOT NULL/);
});

test("a method covers every nullable column of a comparison", () => {
  const ref = Comment.authorId;
  assert.equal(where(Comment.objects.filter(Comment.id.between(ref, 5n).orNull().not())).split('"comments"."author_id" IS NULL').length, 2);
  const computed = func.coalesce(Comment.id, 0n).eq(ref).orNull().not();
  assert.ok(where(Comment.objects.filter(computed)).endsWith('"comments"."author_id" IS NULL)'));
});

test("a condition reads back in the messages", () => {
  const col = Comment.authorId;
  assert.throws(() => col.ne(null).orNull(), /Comment\.authorId IS NOT NULL/);
  assert.throws(() => col.isNull().orNull(), /already a NULL test/);
  assert.throws(() => User.email.like("a%").or(User.email.like("b%")).orNull(), /User\.email LIKE "a%"/);
  assert.throws(() => not(col.isNull()).andNotNull(), /not to not\(Comment\.authorId IS NULL\)/);
});

test("differs keeps a NULL row and passes a NULL test through", () => {
  const col = Comment.authorId;
  assert.equal(where(Comment.objects.filter(differs(col, 1n) as never)), '"comments"."author_id" <> 1 OR "comments"."author_id" IS NULL');
  assert.equal(where(Comment.objects.filter(differs(col, [1n, 2n]) as never)), '"comments"."author_id" NOT IN (1, 2) OR "comments"."author_id" IS NULL');
  assert.equal(where(Comment.objects.filter(differs(col, null) as never)), '"comments"."author_id" IS NOT NULL');
  assert.equal(where(User.objects.filter(differs(User.email, "a") as never)), '"users"."email" <> \'a\'');
});

const keepsNull = /orNull\(\) keeps NULL rows/;
const grouped = (cond: any) => Comment.objects.select({ a: Comment.authorId, n: func.count() }).groupBy(Comment.authorId).having(cond);
const filtered = (cond: any) => Comment.objects.filter(cond);
const ofUser = (cond: any) => User.objects.filter(cond);

test("two nodes of one computed value are one value for the strict rule", () => {
  const comment = User.comments.alias();
  const cases: [string, (cond: any) => unknown, () => any][] = [
    ["aggregate filter", grouped, () => func.max(Comment.id, { filter: Comment.authorId.eq(1n) })],
    ["aggregate order", grouped, () => func.arrayAgg(Comment.authorId, { orderBy: Comment.id.desc() })],
    ["aggregate distinct", grouped, () => func.sum(Comment.authorId, { distinct: true })],
    ["text with ...", filtered, () => func.concat(Comment.authorId as never, "...")],
    ["text with [object ", filtered, () => func.concat(Comment.authorId as never, "[object x")],
    ["text with (subquery)", filtered, () => func.concat(Comment.authorId as never, "(subquery)")],
    ["search config", filtered, () => func.toTsvector("english", Comment.authorId as never)],
    ["alias", ofUser, () => func.abs(comment.authorId)],
  ];
  for (const [label, use, build] of cases) {
    assert.ok(use(or(build().ne(3n), build().isNull())), label);
  }
});

test("two computed values that differ in one part are two values for the strict rule", () => {
  const cases: [string, (cond: any) => unknown, () => any, () => any][] = [
    [
      "aggregate filter",
      grouped,
      () => func.max(Comment.id, { filter: Comment.authorId.eq(1n) }),
      () => func.max(Comment.id, { filter: Comment.authorId.eq(2n) }),
    ],
    [
      "aggregate order",
      grouped,
      () => func.arrayAgg(Comment.authorId, { orderBy: Comment.id }),
      () => func.arrayAgg(Comment.authorId, { orderBy: Comment.id.desc() }),
    ],
    ["aggregate distinct", grouped, () => func.sum(Comment.authorId, { distinct: true }), () => func.sum(Comment.authorId)],
    ["two aliases", ofUser, () => func.abs(User.comments.alias().authorId), () => func.abs(User.comments.alias().authorId)],
    ["alias and path", ofUser, () => func.abs(User.comments.alias().authorId), () => func.abs(User.comments.authorId)],
    [
      "date",
      filtered,
      () => func.concat(Comment.authorId as never, new Date(1000) as never),
      () => func.concat(Comment.authorId as never, new Date(1001) as never),
    ],
    [
      "array",
      filtered,
      () => func.concat(Comment.authorId as never, ["a,b"] as never),
      () => func.concat(Comment.authorId as never, ["a", "b"] as never),
    ],
    ["bigint and number", filtered, () => func.concat(Comment.authorId as never, 1n as never), () => func.concat(Comment.authorId as never, 1 as never)],
  ];
  for (const [label, use, left, right] of cases) {
    assert.throws(() => use(or(left().ne(3n), right().isNull())), keepsNull, label);
  }
});

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"];

  async function setup(): Promise<{ db: any; Reading: any; Item: any; Line: any; models: any }> {
    const registry = new Registry();
    const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${source}${provider === "sqlite" ? "" : arraySource}`, { registry });
    const db = await connect(url!, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    return { db, Reading: models["Reading"], Item: models["Item"], Line: models["Line"], models };
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
      // [condition, the non-NULL rows it matches, whether it is a != or not in]
      const cases: [any, any[], boolean][] = [
        [s.eq(10), [a], false],
        [s.ne(10), [b], true],
        [s.lt(20), [a], false],
        [s.gt(20), [b], false],
        [s.in([10]), [a], false],
        [s.notIn([10]), [b], true],
        [s.between(5, 15), [a], false],
        [n.like("a%"), [a], false],
        [n.ilike("A%"), [a], false],
        [n.contains("d"), [b], false],
      ];
      const ids = async (c: any) => (await Reading.objects.using(db).filter(c).all()).map((r: any) => Number(r.id)).sort();
      const ofRows = (rows: any[]) => rows.map((r) => Number(r.id)).sort();
      for (const [c, matched, negative] of cases) {
        const m = ofRows(matched);
        const rest = ofRows([a, b].filter((r) => !matched.includes(r)));
        const withNull = ofRows([...matched, nul]);
        const restWithNull = ofRows([...[a, b].filter((r) => !matched.includes(r)), nul]);
        const label = c.constructor.name;
        if (negative) assert.throws(() => Reading.objects.using(db).filter(c), /orNull\(\)/, label);
        else assert.deepEqual(await ids(c), m, label);
        assert.deepEqual(await ids(c.orNull()), withNull, label);
        assert.deepEqual(await ids(c.andNotNull()), m, label);
        assert.throws(() => c.not(), (e: unknown) => e instanceof QueryError && /orNull\(\)/.test(e.message), label);
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

  test(`a computed comparison keeps or drops a NULL row (${provider})`, { skip: !url }, async () => {
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
      const ids = async (c: any) => (await Reading.objects.using(db).filter(c).all()).map((r: any) => Number(r.id)).sort();
      const of = (...rows: any[]) => rows.map((r) => Number(r.id)).sort();
      const lower: any = func.lower(Reading.name);
      assert.throws(() => Reading.objects.using(db).filter(lower.ne("ab")), /orNull\(\) keeps NULL rows/);
      assert.throws(() => not(lower.eq("ab")), /under not\(\)/);
      assert.deepEqual(await ids(lower.eq("ab")), of(a));
      assert.deepEqual(await ids(lower.ne("ab").orNull()), of(b, nul));
      assert.deepEqual(await ids(lower.ne("ab").andNotNull()), of(b));
      assert.deepEqual(await ids(lower.eq("ab").orNull().not()), of(b));
      assert.deepEqual(await ids(lower.eq("ab").andNotNull().not()), of(b, nul));
      assert.deepEqual(await ids(Reading.score.add(1).gt(20).orNull()), of(b, nul));
      const safe: any = func.coalesce(Reading.score, 0);
      assert.deepEqual(await ids(safe.ne(10)), of(b, nul));
      assert.deepEqual(await ids(not(safe.eq(10))), of(b, nul));
      assert.throws(() => safe.ne(10).orNull(), /the value is NOT NULL/);
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
      assert.deepEqual(await ids(ne), of(two, nul, half));
      assert.deepEqual(await ids(not(eq)), of(two, nul, half));
      assert.deepEqual(await ids(not(ne)), of(one));
      assert.deepEqual(await ids(ne.orNull()), of(two, nul, half));
      assert.deepEqual(await ids(ne.andNotNull()), of(two));
      assert.deepEqual(await ids(eq.orNull().not()), of(two));
      assert.deepEqual(await ids(eq.andNotNull().not()), of(two, nul, half));
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`set() on a nullable link target clears every link (${provider})`, { skip: !url }, async () => {
    const { db, models } = await setup();
    const { Note, Label, NoteLabel } = models;
    try {
      const [note, other] = await Note.objects.using(db).insertMany([{}, {}]).returning();
      const [a, b] = await Label.objects.using(db).insertMany([{}, {}]).returning();
      await NoteLabel.objects.using(db).insertMany([
        { noteId: note.id, labelId: a.id },
        { noteId: note.id, labelId: b.id },
        { noteId: note.id, labelId: null },
        { noteId: other.id, labelId: a.id },
      ]);
      const links = async (of: any) =>
        (await NoteLabel.objects.using(db).filter(NoteLabel.noteId.eq(of.id)).all()).map((r: any) => r.labelId).sort((x: any, y: any) => Number(x === null) - Number(y === null) || (x ?? 0) - (y ?? 0));
      const fresh = await Note.objects.using(db).get(Note.id.eq(note.id));
      await fresh.labels.set([a]);
      assert.deepEqual(await links(note), [a.id]);
      await fresh.labels.set([]);
      assert.deepEqual(await links(note), []);
      assert.deepEqual(await links(other), [a.id]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`a json path is strict (${provider})`, { skip: !url || provider === "sqlite" }, async () => {
    const { db, Reading } = await setup();
    try {
      const [, b, nul] = await Reading.objects
        .using(db)
        .insertMany([
          { name: "a", tag: "t", meta: { k: 1 } },
          { name: "b", tag: "t", meta: { k: 2 } },
          { name: "c", tag: "t", meta: null },
        ])
        .returning();
      const ids = async (c: any) => (await Reading.objects.using(db).filter(c).all()).map((r: any) => Number(r.id)).sort();
      const path: any = Reading.meta.get("k");
      assert.throws(() => Reading.objects.using(db).filter(path.ne(1)), /orNull\(\) keeps NULL rows/);
      assert.throws(() => not(path.eq(1)), /under not\(\)/);
      assert.deepEqual(await ids(path.ne(1).orNull()), [b, nul].map((r) => Number(r.id)).sort());
      assert.deepEqual(await ids(path.ne(1).andNotNull()), [Number(b.id)]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`the strict rule runs in having, a join on and an aggregate filter on rows (${provider})`, { skip: !url }, async () => {
    const { db, Reading } = await setup();
    try {
      await Reading.objects.using(db).insertMany([
        { score: 10, name: "a", tag: "g1" },
        { score: null, name: null, tag: "g2" },
      ]);
      const qs = Reading.objects.using(db);
      const grouped = qs.select({ tag: Reading.tag }).groupBy(Reading.tag);
      const tags = async (q: any) => (await q.all()).map((r: any) => r.tag).sort();
      const top: any = func.max(Reading.score);
      assert.throws(() => grouped.having(top.ne(10)), /orNull\(\) keeps NULL rows/);
      assert.deepEqual(await tags(grouped.having(top.ne(10).orNull())), ["g2"]);
      assert.deepEqual(await tags(grouped.having(top.ne(10).andNotNull())), []);
      assert.deepEqual(await tags(grouped.having(top.eq(10).andNotNull().not())), ["g2"]);

      const totals = Reading.objects.select({ tag: Reading.tag, m: func.max(Reading.score) }).groupBy(Reading.tag).cte("totals");
      const lower: any = func.lower(Reading.name);
      const on = (c: any) => and(totals.c.tag.eq(Reading.tag), c);
      assert.throws(() => qs.join(totals, on(lower.ne("a"))), /orNull\(\) keeps NULL rows/);
      assert.deepEqual(await tags(qs.join(totals, on(lower.ne("a").orNull())).select({ tag: Reading.tag })), ["g2"]);
      assert.deepEqual(await tags(qs.join(totals, on(lower.ne("a").andNotNull())).select({ tag: Reading.tag })), []);

      assert.throws(() => func.count({ filter: lower.ne("a") } as never), /orNull\(\) keeps NULL rows/);
      for (const [method, expected] of [["orNull", 1], ["andNotNull", 0]] as const) {
        const n = func.count({ filter: lower.ne("a")[method]() } as never);
        const rows = await qs.select({ n }).all();
        assert.deepEqual(rows.map((r: any) => Number(r.n)), [expected], method);
      }
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`two separate nodes of one value pair a ne with an isNull (${provider})`, { skip: !url }, async () => {
    const { db, Reading } = await setup();
    try {
      const [, b, nul] = await Reading.objects
        .using(db)
        .insertMany([{ name: "a", tag: "t" }, { name: "b", tag: "t" }, { name: null, tag: "t" }])
        .returning();
      const rows = await Reading.objects
        .using(db)
        .filter(or(func.lower(Reading.name).ne("a"), func.lower(Reading.name).isNull()))
        .all();
      assert.deepEqual(rows.map((r: any) => Number(r.id)).sort(), [b, nul].map((r) => Number(r.id)).sort());
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`an array index keeps or drops a NULL row (${provider})`, { skip: !url || provider === "sqlite" }, async () => {
    const { db, models } = await setup();
    const Tagged = models["Tagged"];
    try {
      const [, b, short] = await Tagged.objects
        .using(db)
        .insertMany([
          { links: ["a", "x"] },
          { links: ["b", "y"] },
          { links: [] },
        ])
        .returning();
      const ids = async (c: any) => (await Tagged.objects.using(db).filter(c).all()).map((r: any) => Number(r.id)).sort();
      const of = (...rows: any[]) => rows.map((r) => Number(r.id)).sort();
      const second: any = Tagged.links.element(2);
      assert.deepEqual(await ids(second.ne("x").orNull()), of(b, short));
      assert.deepEqual(await ids(second.ne("x").andNotNull()), of(b));
      assert.deepEqual(await ids(not(second.eq("x").orNull())), of(b));
      assert.deepEqual(await ids(not(second.eq("x").andNotNull())), of(b, short));
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`a sqlite division by zero is a NULL row (${provider})`, { skip: !url || provider !== "sqlite" }, async () => {
    const { db, Reading } = await setup();
    try {
      const [a, zero, nul] = await Reading.objects
        .using(db)
        .insertMany([{ score: 2, tag: "t" }, { score: 0, tag: "t" }, { score: null, tag: "t" }])
        .returning();
      const ids = async (c: any) => (await Reading.objects.using(db).filter(c).all()).map((r: any) => Number(r.id)).sort();
      const of = (...rows: any[]) => rows.map((r) => Number(r.id)).sort();
      const ratio: any = Reading.score.div(Reading.score);
      assert.deepEqual(await ids(ratio.ne(5).orNull()), of(a, zero, nul));
      assert.deepEqual(await ids(ratio.ne(5).andNotNull()), of(a));
      assert.deepEqual(await ids(ratio.eq(1).orNull()), of(a, zero, nul));
      assert.deepEqual(await ids(ratio.eq(1).andNotNull()), of(a));
      assert.deepEqual(await ids(not(ratio.eq(1).orNull())), []);
      assert.deepEqual(await ids(not(ratio.eq(1).andNotNull())), of(zero, nul));
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`an outer() value keeps or drops a NULL row (${provider})`, { skip: !url }, async () => {
    const { db, Reading, models } = await setup();
    try {
      const Note = models["Note"];
      await Note.objects.using(db).insertMany([{ id: 1 }]);
      const [, b, nul] = await Reading.objects
        .using(db)
        .insertMany([{ score: 1, tag: "t" }, { score: 5, tag: "t" }, { score: null, tag: "t" }])
        .returning();
      const ids = async (method: "orNull" | "andNotNull") => {
        const inner = Note.objects.filter((Note.id.ne(outer(Reading.score)) as any)[method]());
        return (await Reading.objects.using(db).filter(exists(inner)).all()).map((r: any) => Number(r.id)).sort();
      };
      assert.deepEqual(await ids("orNull"), [b, nul].map((r) => Number(r.id)).sort());
      assert.deepEqual(await ids("andNotNull"), [Number(b.id)]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`an excluded() value keeps or drops a NULL row (${provider})`, { skip: !url }, async () => {
    const { db, Reading } = await setup();
    try {
      await Reading.objects.using(db).insertMany([{ id: 1, score: 1, tag: "old" }, { id: 2, score: 2, tag: "old" }]);
      const rows = [{ id: 1, score: 1, tag: "new" }, { id: 2, score: null, tag: "new" }];
      for (const [method, second] of [["orNull", "x"], ["andNotNull", "y"]] as const) {
        const changed = (excluded(Reading.score).ne(1) as any)[method]();
        await Reading.objects
          .using(db)
          .insertMany(rows)
          .onConflict(Reading.id, { update: true, updateValues: { tag: func.case([changed, "x"], { default: "y" }) } });
        const got = (await Reading.objects.using(db).orderBy(Reading.id).all()).map((r: any) => r.tag);
        assert.deepEqual(got, ["y", second], method);
      }
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}
