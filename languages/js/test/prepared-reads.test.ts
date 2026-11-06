/** Prepared reads held in Rust: a call sends what the unprepared query sends. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, func, getDatabase, loads, param, QueryError, Registry, scope, type Database } from "../src/index.js";
import { Post, User } from "./blog/models.js";
import { useDatabase } from "./helpers.js";
import { hasExtension } from "./artifact-caps.js";

useDatabase();

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const KINDS = `
model Kind {
  id Int @id @default(autoincrement())
  big BigInt
  small Int
  real Float
  name String
  body String @db.Text
  at DateTime
  day DateTime @db.Date
  uid String @db.Uuid
  price Decimal
  @@map("pr_js_kind")
}
model Item {
  id Int @id @default(autoincrement())
  shop Int
  name String
  views Int @default(0)
  @@query.filter("tenant", "shop == scope.shop")
  @@map("pr_js_item")
}
`;

const N = 5;
const STAMP = new Date("2024-01-01T12:00:00Z");
const day = (n: number) => new Date(Date.UTC(2024, 0, 1 + n));
const VALUES: Record<string, unknown[]> = {
  big: Array.from({ length: N }, (_, i) => 2n ** 40n + BigInt(i)),
  small: Array.from({ length: N }, (_, i) => i * 7),
  real: Array.from({ length: N }, (_, i) => i * 1.5),
  name: Array.from({ length: N }, (_, i) => `n${i}`),
  body: Array.from({ length: N }, (_, i) => `b${i}`),
  at: Array.from({ length: N }, (_, i) => new Date(STAMP.getTime() + i * 86_400_000)),
  day: Array.from({ length: N }, (_, i) => day(i)),
  uid: Array.from({ length: N }, (_, i) => `00000000-0000-0000-0000-00000000000${i + 1}`),
  price: Array.from({ length: N }, (_, i) => i + 0.25),
};
// A value of the column that no row holds.
const MISSING: Record<string, unknown> = {
  big: 2n ** 62n,
  small: 9999,
  real: 123.5,
  name: "zz",
  body: "zz",
  at: new Date(STAMP.getTime() + 99 * 86_400_000),
  day: day(3000),
  uid: "00000000-0000-0000-0000-000000000999",
  price: 99.5,
};

const urlOf = (provider: string) => (provider === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"]);

async function openDb(provider: string, source = KINDS): Promise<{ db: Database; models: Record<string, Any> }> {
  if (provider === "sqlite") source = source.replace("  price Decimal\n", "");
  const registry = new Registry();
  const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${source}`, { registry }) as Record<string, Any>;
  const db = await connect(urlOf(provider)!, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  return { db, models };
}

const ids = (rows: Any[]) => rows.map((r) => r.id);

// -- in of a param: a list of any length ------------------------------------------------------

for (const provider of ["sqlite", "postgresql"]) {
  for (const column of Object.keys(VALUES)) {
    const skip = !urlOf(provider) || (provider === "sqlite" && column === "price");
    test(`in of a param matches the plain in for each column type (${provider} ${column})`, { skip }, async () => {
      const { db, models } = await openDb(provider);
      const Kind = models["Kind"];
      try {
        await Kind.objects.using(db).insertMany(
          Array.from({ length: N }, (_, i) => ({
            id: i + 1,
            ...Object.fromEntries(Object.entries(VALUES).filter(([c]) => provider !== "sqlite" || c !== "price").map(([c, v]) => [c, v[i]])),
          })),
        );
        const col = Kind[column];
        const values = VALUES[column]!;
        const inside = Kind.objects.using(db).filter(col.in(param("v"))).orderBy(Kind.id).prepare();
        const outside = Kind.objects.using(db).filter(col.notIn(param("v"))).orderBy(Kind.id).prepare();
        for (const chosen of [[], [values[1]], [values[0], values[3], MISSING[column]], values, [MISSING[column]]]) {
          const plainIn = Kind.objects.using(db).filter(col.in(chosen)).orderBy(Kind.id);
          const plainOut = Kind.objects.using(db).filter(col.notIn(chosen)).orderBy(Kind.id);
          assert.deepEqual(ids(await inside.all({ v: chosen })), ids(await plainIn), `${column} ${String(chosen)}`);
          assert.deepEqual(ids(await outside.all({ v: chosen })), ids(await plainOut), `${column} ${String(chosen)}`);
        }
        assert.equal(await inside.count({ v: [values[2]] }), 1);
      } finally {
        await db.dropTables();
        await db.close();
      }
    });
  }

  test(`in of a param takes null elements and an empty list (${provider})`, { skip: !urlOf(provider) }, async () => {
    const { db, models } = await openDb(provider, 'model Maybe {\n  id Int @id\n  n Int?\n  @@map("pr_js_maybe")\n}\n');
    const Maybe = models["Maybe"];
    try {
      await Maybe.objects.using(db).insertMany([{ id: 1, n: 1 }, { id: 2, n: 2 }, { id: 3, n: null }]);
      const inside = Maybe.objects.using(db).filter(Maybe.n.in(param("v"))).orderBy(Maybe.id).prepare();
      const outside = Maybe.objects.using(db).filter(Maybe.n.notIn(param("v")).andNotNull()).orderBy(Maybe.id).prepare();
      for (const chosen of [[], [1], [1, null], [null]]) {
        const plainIn = Maybe.objects.using(db).filter(Maybe.n.in(chosen)).orderBy(Maybe.id);
        const plainOut = Maybe.objects.using(db).filter(Maybe.n.notIn(chosen).andNotNull()).orderBy(Maybe.id);
        assert.deepEqual(ids(await inside.all({ v: chosen })), ids(await plainIn), String(chosen));
        assert.deepEqual(ids(await outside.all({ v: chosen })), ids(await plainOut), String(chosen));
      }
      await assert.rejects(inside.all({ v: null } as never), /can't be null/);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}

// -- the same statement and the same rows as the unprepared query -----------------------------

async function seed() {
  const [alice, bob] = (await User.objects
    .insertMany([
      { email: "alice@example.com", name: "Alice" },
      { email: "bob@example.com", name: "Bob" },
    ])
    .returning()) as [User, User];
  await Post.objects.insertMany([
    { author: alice, title: "a1", body: "", views: 5 },
    { author: alice, title: "A_2%", body: "", views: 50 },
    { author: alice, title: "a3", body: "", views: 20 },
    { author: bob, title: "b1", body: "", views: 100 },
  ]);
  return { alice, bob };
}

test("a call sends the statement of the plain query", async () => {
  const { alice } = await seed();
  const base = Post.objects.orderBy(Post.id);
  const cases: [Any, Record<string, unknown>, Any][] = [
    [base.filter(Post.authorId.eq(param("a"))), { a: alice.id }, base.filter(Post.authorId.eq(alice.id))],
    [base.filter(Post.views.gte(param("lo")), Post.views.lt(param("hi"))), { lo: 10, hi: 60 }, base.filter(Post.views.gte(10), Post.views.lt(60))],
    [base.filter(Post.title.icontains(param("t"))), { t: "_2%" }, base.filter(Post.title.icontains("_2%"))],
    [base.filter(Post.title.startsWith(param("t"))), { t: "a" }, base.filter(Post.title.startsWith("a"))],
    [base.filter(Post.id.in(param("ids"))), { ids: [1n, 3n, 99n] }, base.filter(Post.id.in([1n, 3n, 99n]))],
    [base.filter(Post.id.notIn(param("ids"))), { ids: [1n, 3n] }, base.filter(Post.id.notIn([1n, 3n]))],
    [base.limit(param("n")).offset(param("o")), { n: 2, o: 1 }, base.slice(1, 3)],
    [base.filter(Post.views.add(param("bump")).gt(param("min"))), { bump: 10, min: 55 }, base.filter(Post.views.add(10).gt(55))],
    [base.load(Post.author).filter(Post.id.eq(param("id"))), { id: 2n }, base.load(Post.author).filter(Post.id.eq(2n))],
  ];
  for (const [preparedQs, values, plain] of cases) {
    const prepared = preparedQs.prepare();
    if (!plain.sql().includes("IN (")) {
      // A param list becomes one array, so the text differs on purpose.
      assert.equal(prepared.sql(values), plain.sql(), JSON.stringify(values, (_, v) => (typeof v === "bigint" ? Number(v) : v)));
      assert.equal(prepared.inspect(values).sql, plain.inspect().sql);
    }
    const got = (await prepared.all(values)) as Any[];
    const expect = (await plain) as Any[];
    assert.deepEqual(got.map((r) => [r.id, r.title]), expect.map((r) => [r.id, r.title]));
    assert.equal(await prepared.count(values), await plain.count());
    assert.equal(await prepared.exists(values), await plain.exists());
  }
});

test("a prefetch of one key uses one array on postgres", async () => {
  await seed();
  const q = User.objects.orderBy(User.id).load(User.posts.objects.filter(Post.views.gte(param("min")))).prepare();
  const seen: string[] = [];
  const remove = getDatabase().onQuery((e) => { seen.push(e.sql); });
  let users: Any[];
  try {
    users = (await q.all({ min: 10 })) as Any[];
  } finally {
    remove();
  }
  assert.equal(seen.length, 2);
  assert.ok(seen[1]!.includes("ANY("), seen[1]);
  assert.deepEqual(users.map((u) => u.posts.cached.length).sort(), [1, 2]);
});

test("a param in a case needs a declared type", () => {
  const caseOf = (big: unknown, small: unknown) => (func as Any).case([Post.views.gt(10), big], { default: small });
  assert.throws(() => Post.objects.select({ v: caseOf(param("big"), param("small")) }).prepare(), (e: Error) => e instanceof QueryError && /declare its type/.test(e.message));
});

test("a declared type lets a case of params prepare", async () => {
  await seed();
  const c = (func as Any).case([Post.views.gt(10), param("big", "int")], { default: param("small", "int") });
  const q = Post.objects.select({ id: Post.id, v: c }).orderBy(Post.id).prepare();
  const rows = (await q.all({ big: 1, small: 0 } as never)) as Any[];
  assert.deepEqual(rows.map((r) => [Number(r.id), Number(r.v)]), [[1, 0], [2, 1], [3, 1], [4, 1]]);
});

test("a param none is refused for in too", async () => {
  const q = Post.objects.filter(Post.id.in(param("ids"))).prepare();
  await assert.rejects(q.all({ ids: null } as never), /can't be null/);
});

// -- scope: per-call values -------------------------------------------------------------------

const queryDefaults = hasExtension("monorm.query-defaults");

for (const provider of ["sqlite", "postgresql"]) {
  test(`scope comes from the context or the keyword (${provider})`, { skip: !queryDefaults || !urlOf(provider) }, async () => {
    const { db, models } = await openDb(provider);
    const Item = models["Item"];
    try {
      await Item.objects.using(db).withoutDefaults().insertMany([
        { shop: 1, name: "a", views: 1 },
        { shop: 1, name: "b", views: 20 },
        { shop: 2, name: "c", views: 30 },
      ]);
      // Prepared with no scope active.
      const q = Item.objects.using(db).filter(Item.views.gte(param("lo"))).orderBy(Item.id).prepare();
      const names = (rows: Any[]) => rows.map((r) => r.name);
      await scope({ shop: 1 }, async () => {
        assert.deepEqual(names(await q.all({ lo: 0 })), ["a", "b"]);
        assert.equal(await q.count({ lo: 0 }), 2);
      });
      await scope({ shop: 2 }, async () => {
        assert.deepEqual(names(await q.all({ lo: 0 })), ["c"]);
        assert.equal((await q.first({ lo: 0 }))!.name, "c");
      });
      assert.deepEqual(names(await q.all({ lo: 0, scope: { shop: 2 } })), ["c"]);
      assert.deepEqual(names(await q.all({ lo: 10, scope: { shop: 1 } })), ["b"]);
      assert.ok(await q.exists({ lo: 25, scope: { shop: 2 } }));
      assert.ok(!(await q.exists({ lo: 25, scope: { shop: 1 } })));
      assert.equal((await q.get({ lo: 20, scope: { shop: 1 } })).name, "b");
      // The keyword wins over the context.
      await scope({ shop: 1 }, async () => {
        assert.deepEqual(names(await q.all({ lo: 0, scope: { shop: 2 } })), ["c"]);
      });
      // The same call and the plain query agree on the SQL.
      await scope({ shop: 2 }, async () => {
        const plain = Item.objects.using(db).filter(Item.views.gte(5)).orderBy(Item.id);
        assert.equal(q.sql({ lo: 5 }), plain.sql());
        assert.equal(q.inspect({ lo: 5 }).sql, plain.inspect().sql);
        assert.equal(q.sql({ lo: 5 }), q.sql({ lo: 5, scope: { shop: 2 } }));
      });
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`scope errors (${provider})`, { skip: !queryDefaults || !urlOf(provider) }, async () => {
    const { db, models } = await openDb(provider);
    const Item = models["Item"];
    try {
      const q = Item.objects.using(db).filter(Item.views.gte(param("lo"))).prepare();
      await assert.rejects(q.all({ lo: 0 }), /reads scope\.shop, which is not set/);
      await assert.rejects(q.all({ lo: 0, scope: { shop: null } }), /scope\.shop is None\/null/);
      await assert.rejects(scope({ shop: null }, async () => { await q.all({ lo: 0 }); }), /scope\.shop is None\/null/);
      assert.throws(() => param("scope"), /reserved/);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}

// -- prepare-time errors ----------------------------------------------------------------------

test("a shape that cannot be a slot raises at prepare", async () => {
  const source =
    'model Owner {\n  id Int @id\n  things Thing[]\n  @@map("pr_js_owner")\n}\n' +
    'model Thing {\n  id Int @id\n  ownerId Int\n  owner Owner @relation(fields: [ownerId], references: [id])\n  @@map("pr_js_thing")\n}\n';
  const { db, models } = await openDb("sqlite", source);
  try {
    const Owner = models["Owner"];
    // SQLite builds the prefetch text per call, so a param in its limit has no slot.
    assert.throws(() => Owner.objects.using(db).load(Owner.things.objects.limit(param("k"))).prepare(), /limit or offset cannot be prepared/);
  } finally {
    await db.close();
  }
});

test("prepared calls run in the current transaction", async () => {
  await seed();
  const q = Post.objects.filter(Post.views.gte(param("lo"))).orderBy(Post.id).prepare();
  await assert.rejects(
    getDatabase().transaction(async () => {
      await Post.objects.filter(Post.id.eq(1n)).update({ views: 500 });
      assert.deepEqual(((await q.all({ lo: 400 })) as Any[]).map((p) => p.views), [500]);
      throw new Error("rolled back");
    }),
    /rolled back/,
  );
  assert.deepEqual(await q.all({ lo: 400 }), []);
});

test("hooks run for every call", async () => {
  await seed();
  const q = Post.objects.filter(Post.id.eq(param("id"))).prepare();
  const seen: string[] = [];
  const remove = getDatabase().onQuery((e) => { seen.push(e.sql); });
  try {
    await q.all({ id: 1n });
    await q.all({ id: 2n });
  } finally {
    remove();
  }
  assert.equal(seen.length, 2);
});
