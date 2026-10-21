/** The strict null rule: a comparison on a nullable column under `not()` / `.not()` / `exclude()`,
 * and `ne` / `notIn` on one in any `filter()`, need `orNull()` or `andNotNull()`. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { func, loads, not, QueryError, Registry } from "../src/index.js";
import { Comment, Post, User } from "./blog/models.js";

const x = Comment.authorId; // nullable

function where(qs: { sql(): string }): string {
  return qs.sql().split(" WHERE ")[1]!;
}

const underNegation = /orNull\(\) drops NULL rows under negation, andNotNull\(\) keeps them/;
const positiveNe = /orNull\(\) keeps NULL rows, andNotNull\(\) drops them/;

test("a negation of a nullable comparison throws", () => {
  const builds: [string, () => unknown][] = [
    ["not()", () => not(x.gt(20n))],
    [".not()", () => x.eq(1n).not()],
    ["in", () => x.in([1n, 2n]).not()],
    ["between", () => x.between(1n, 5n).not()],
    ["or with a NOT NULL column", () => x.gt(1n).or(Comment.id.gt(2n)).not()],
    ["and with a NOT NULL column", () => Comment.id.gt(2n).and(x.gt(1n)).not()],
    ["exclude", () => Comment.objects.exclude(x.eq(1n))],
    ["exclude, two conditions", () => Comment.objects.exclude(Comment.id.eq(1n), x.eq(1n))],
  ];
  for (const [label, build] of builds) {
    assert.throws(build, (e: unknown) => e instanceof QueryError && underNegation.test(e.message), label);
  }
});

test("ne and notIn on a nullable column throw in a positive filter", () => {
  const builds: [string, () => unknown][] = [
    ["ne", () => Comment.objects.filter(x.ne(1n))],
    ["notIn", () => Comment.objects.filter(x.notIn([1n, 2n]))],
    ["inside or", () => Comment.objects.filter(Comment.id.eq(1n), x.ne(1n).or(Comment.id.eq(2n)))],
  ];
  for (const [label, build] of builds) {
    assert.throws(build, (e: unknown) => e instanceof QueryError && positiveNe.test(e.message), label);
  }
});

test("a having condition follows the same rule", () => {
  assert.throws(() => Comment.objects.select({ a: x, n: func.count() }).groupBy(x).having(x.ne(1n)), /orNull/);
});

for (const method of ["orNull", "andNotNull"] as const) {
  test(`${method}() on the comparison is accepted`, () => {
    const m = (c: any): any => c[method]();
    assert.ok(Comment.objects.filter(m(x.ne(1n))).sql());
    assert.ok(Comment.objects.filter(m(x.notIn([1n]))).sql());
    assert.ok(Comment.objects.filter(m(x.gt(20n)).not()).sql());
    assert.ok(Comment.objects.exclude(m(x.eq(1n))).sql());
    assert.ok(Comment.objects.exclude(m(x.eq(1n)), m(x.between(1n, 5n))).sql());
    assert.ok(Comment.objects.filter(not(m(x.gt(1n)).or(m(x.lt(0n)))) as never).sql());
  });
}

test("a method on one comparison does not cover another", () => {
  assert.throws(() => x.gt(1n).orNull().and(x.lt(9n)).not(), /orNull/);
});

test("the sql is unchanged by the check", () => {
  assert.equal(where(Comment.objects.exclude(x.gt(20n).orNull())), 'NOT ("comments"."author_id" > 20 OR "comments"."author_id" IS NULL)');
  assert.equal(where(Comment.objects.filter(x.ne(1n).andNotNull())), '"comments"."author_id" <> 1 AND "comments"."author_id" IS NOT NULL');
});

test("a NOT NULL column and a null test need nothing", () => {
  assert.ok(Comment.objects.filter(Comment.id.eq(1n).not()).sql());
  assert.ok(User.objects.exclude(User.email.eq("a")).sql());
  assert.ok(User.objects.filter(User.email.ne("a"), User.email.notIn(["b"])).sql());
  assert.ok(Comment.objects.filter(x.isNull().not(), x.isNotNull()).sql());
  assert.ok(Comment.objects.filter(x.gt(20n), x.eq(1n), x.in([1n])).sql());
});

test("a nullable column behind a to-many relation follows the column rule", () => {
  assert.throws(() => Post.objects.exclude(Post.comments.authorId.eq(1n)), QueryError);
  assert.throws(() => Post.objects.filter(Post.comments.authorId.ne(1n)), QueryError);
  assert.ok(Post.objects.filter(Post.comments.id.ne(1n)).sql());
  assert.ok(Post.objects.exclude(Post.comments.id.eq(1n)).sql());
});

const nested = `
type Geo {
  lat Float
  lng Float?
}

type Spot {
  name  String
  geo   Geo
  where Geo?
}

model Place {
  id   Int   @id
  spot Spot
  opt  Spot?
  js   Spot  @storage(json)
  @@map("ns_node_place")
}

model Reading {
  id   Int     @id
  name String?
  @@map("ns_node_reading")
}

model Item {
  shop  Int
  id    Int
  lines Line[]
  @@id([shop, id])
  @@map("ns_node_item")
}

model Line {
  id     Int   @id
  shop   Int?
  itemId Int?
  item   Item? @relation(fields: [shop, itemId], references: [shop, id])
  @@map("ns_node_line")
}
`;

test("a nested subfield is nullable only when its declaration says so", () => {
  const { Place } = loads(nested, { registry: new Registry() }) as any;
  const value = (col: any): unknown => (String(col).endsWith("name") ? "a" : 1.5);
  const sure = [Place.spot.name, Place.spot.geo.lat, Place.js.name, Place.js.geo.lat];
  const maybe = [Place.spot.geo.lng, Place.spot.where.lat, Place.opt.name, Place.opt.geo.lat, Place.js.geo.lng, Place.js.where.lat];
  for (const col of sure) {
    assert.ok(Place.objects.filter(col.ne(value(col))).sql(), String(col));
    assert.ok(Place.objects.exclude(col.eq(value(col))).sql(), String(col));
    assert.throws(() => col.ne(value(col)).orNull(), (e: unknown) => e instanceof QueryError && /NOT NULL/.test(e.message), String(col));
  }
  for (const col of maybe) {
    assert.throws(() => Place.objects.filter(col.ne(value(col))), positiveNe, String(col));
    assert.throws(() => Place.objects.exclude(col.eq(value(col))), underNegation, String(col));
    assert.ok(Place.objects.filter(col.ne(value(col)).orNull()).sql(), String(col));
  }
});

test("a null test of the wrong direction does not handle a comparison", () => {
  const builds: [string, () => unknown][] = [
    ["not or isNotNull", () => not(x.gt(1n).or(x.ne(null)))],
    ["not and isNull", () => not(x.gt(1n).and(x.eq(null)))],
    ["not and isNull, ne", () => not(x.ne(1n).and(x.eq(null)))],
    ["or isNotNull", () => Comment.objects.filter(x.ne(1n).or(x.ne(null)))],
    ["and isNull", () => Comment.objects.filter(x.ne(1n).and(x.eq(null)))],
  ];
  for (const [label, build] of builds) assert.throws(build, QueryError, label);
});

test("a null test of the right direction handles a comparison", () => {
  assert.ok(Comment.objects.filter(x.ne(1n).or(x.eq(null))).sql());
  assert.ok(Comment.objects.filter(x.ne(1n).and(x.ne(null))).sql());
  assert.ok(Comment.objects.filter(not(x.gt(1n).or(x.eq(null)))).sql());
  assert.ok(Comment.objects.filter(not(x.gt(1n).and(x.ne(null)))).sql());
});

test("a composite key ne keeps the rows with a NULL key column", () => {
  const { Line, Item } = loads(nested, { registry: new Registry() }) as any;
  const item = Object.assign(new Item._meta.Row(), { shop: 1n, id: 2n });
  assert.equal(Line.objects.filter(Line.item.ne(item)).sql().split(" IS NULL").length - 1, 2);
  assert.equal(Line.objects.filter(not(Line.item.eq(item))).sql().split(" IS NOT NULL").length - 1, 2);
});

test("the message prints the comparison, then the nullable column", () => {
  assert.throws(
    () => not(Comment.id.between(Comment.authorId, 5n)),
    (e: unknown) => e instanceof QueryError && e.message.startsWith("(Comment.id >= Comment.authorId) sits under not() / exclude() and Comment.authorId is nullable"),
  );
});

test("a negated like on a nullable column throws, on a NOT NULL one it does not", () => {
  const { Reading } = loads(nested, { registry: new Registry() }) as any;
  for (const build of [() => Reading.name.like("a%").not(), () => Reading.name.contains("a").not(), () => Reading.name.ilike("a%").not()]) {
    assert.throws(build, underNegation);
  }
  assert.ok(Reading.objects.filter(Reading.name.like("a%").orNull().not()).sql());
  assert.ok(Reading.objects.filter(Reading.name.contains("a").andNotNull().not()).sql());
  assert.ok(User.objects.filter(User.email.like("a%").not()).sql());
  assert.ok(User.objects.filter(User.email.contains("a").not()).sql());
});

test("a single-key relation ne never raises; the key column ne keeps the column rule", () => {
  const alice = Object.assign(new (User as any)._meta.Row(), { id: 1n }) as never;
  assert.ok(Comment.objects.filter(Comment.author.ne(alice)).sql());
  assert.ok(Comment.objects.get(Comment.author.ne(alice)));
  assert.throws(() => Comment.objects.get(x.ne(1n)), positiveNe);
  assert.ok(Comment.objects.get(x.ne(1n).orNull()));
});
