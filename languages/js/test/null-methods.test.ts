/** `.orNull()` / `.andNotNull()`: a NULL row kept or dropped, on PostgreSQL and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, func, loads, not, QueryError, Registry } from "../src/index.js";
import { differs } from "../src/expr.js";
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

test("a computed value is not checked yet", () => {
  assert.throws(() => Comment.authorId.add(1n).gt(5n).orNull(), /applies to a plain column/);
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

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"];

  async function setup(): Promise<{ db: any; Reading: any; Item: any; Line: any; models: any }> {
    const registry = new Registry();
    const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${source}`, { registry });
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
}
