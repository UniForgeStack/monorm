/** A failed statement marks its transaction failed, also when the block catches the error. */

import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Migration, Migrations, Migrator } from "../src/migrations.js";
import { test } from "node:test";

import { connect, DatabaseError, getDatabase, IntegrityError, loads, QueryError, Registry, TransactionAborted, type Database } from "../src/index.js";
import { Post, PostTag, Tag, User } from "./blog/models.js";
import { Author, sqliteRegistry } from "./sqlite/models.js";
import { useDatabase } from "./helpers.js";

useDatabase();

interface Case {
  db: Database;
  insert(tag: string): Promise<unknown>;
  count(): Promise<number>;
}

async function run(body: (c: Case) => Promise<void>): Promise<void> {
  const pg = getDatabase();
  await body({
    db: pg,
    insert: (tag) => User.objects.using(pg).insert({ email: `${tag}@x.io`, name: "n" }),
    count: () => User.objects.using(pg).count(),
  });
  await User.objects.using(pg).filter(User.id.gt(0n)).delete();
  const lite = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await lite.createTables();
    await body({
      db: lite,
      insert: (tag) => Author.objects.using(lite).insert({ email: `${tag}@x.io`, name: "n" }),
      count: () => Author.objects.using(lite).count(),
    });
  } finally {
    await lite.close();
  }
}

test("a caught error aborts the commit", () =>
  run(async (c) => {
    const seen: string[] = [];
    await c.insert("dup");
    const err = await c.db
      .transaction(async () => {
        await c.insert("kept");
        await assert.rejects(c.insert("dup"), IntegrityError);
        await c.db.onCommit(() => void seen.push("ran"));
      })
      .then(
        () => assert.fail("must throw"),
        (e: unknown) => e,
      );
    assert.ok(err instanceof TransactionAborted && err instanceof DatabaseError);
    assert.ok(err.cause instanceof IntegrityError);
    assert.equal(err.cause.sqlstate, "23505");
    assert.ok(err.message.includes("earlier error"));
    assert.ok(err.message.includes(err.cause.message.split("\n")[0]!));
    assert.equal(await c.count(), 1);
    assert.deepEqual(seen, []);
  }));

test("a later statement raises TransactionAborted", () =>
  run(async (c) => {
    await c.insert("dup");
    await assert.rejects(
      c.db.transaction(async () => {
        const first = await c.insert("dup").catch((e: unknown) => e);
        assert.ok(first instanceof IntegrityError);
        await assert.rejects(c.db.transaction(async () => { throw new TypeError("a later application failure"); }, { savepoint: false }), TypeError);
        const later = await c.insert("later").catch((e: unknown) => e);
        assert.ok(later instanceof TransactionAborted && later.cause instanceof IntegrityError);
        assert.notEqual(later.cause, first);
        await assert.rejects(c.count(), TransactionAborted);
        throw first;
      }),
      IntegrityError,
    );
    assert.equal(await c.count(), 1);
  }));

test("a savepoint in a try keeps the other writes", () =>
  run(async (c) => {
    const seen: string[] = [];
    await c.insert("dup");
    await c.db.transaction(async () => {
      await c.insert("before");
      await assert.rejects(
        c.db.transaction(async () => {
          await c.db.onCommit(() => void seen.push("inner"));
          await c.insert("dup");
        }),
        IntegrityError,
      );
      await c.insert("after");
      await c.db.onCommit(() => void seen.push("outer"));
    });
    assert.equal(await c.count(), 3);
    assert.deepEqual(seen, ["outer"]);
  }));

test("a caught error in a savepoint block aborts that savepoint only", () =>
  run(async (c) => {
    await c.insert("dup");
    await c.db.transaction(async () => {
      await assert.rejects(
        c.db.transaction(async () => {
          await assert.rejects(c.insert("dup"), IntegrityError);
        }),
        TransactionAborted,
      );
      await c.insert("after");
    });
    assert.equal(await c.count(), 2);
  }));

test("savepoint false joins the enclosing transaction", () =>
  run(async (c) => {
    await c.insert("dup");
    await c.db.transaction(() => c.db.transaction(() => c.insert("joined"), { savepoint: false }));
    assert.equal(await c.count(), 2);
    await assert.rejects(
      c.db.transaction(async () => {
        await c.insert("lost");
        await assert.rejects(
          c.db.transaction(() => c.insert("dup"), { savepoint: false }),
          IntegrityError,
        );
      }),
      TransactionAborted,
    );
    assert.equal(await c.count(), 2);
    await c.db.transaction(() => c.insert("outer"), { savepoint: false });
    assert.equal(await c.count(), 3);
  }));

test("an uncaught error rolls back and raises the original error", () =>
  run(async (c) => {
    await c.insert("dup");
    const err = await c.db
      .transaction(async () => {
        await c.insert("lost");
        await c.insert("dup");
      })
      .catch((e: unknown) => e);
    assert.ok(err instanceof IntegrityError && !(err instanceof TransactionAborted));
    assert.equal(await c.count(), 1);
  }));

test("joined application failures retain the first original cause", () =>
  run(async (c) => {
    const original = new TypeError("application failure");
    const commit = await c.db.transaction(async () => {
      await c.insert("lost");
      await assert.rejects(c.db.transaction(async () => { throw original; }, { savepoint: false }), TypeError);
      await assert.rejects(c.db.transaction(async () => { throw new RangeError("second failure"); }, { savepoint: false }), RangeError);
      const later = await c.count().catch((e: unknown) => e);
      assert.ok(later instanceof TransactionAborted);
      assert.equal(later.cause, original);
      const begin = await c.db.transaction(async () => {}).catch((e: unknown) => e);
      assert.ok(begin instanceof TransactionAborted);
      assert.equal(begin.cause, original);
    }).catch((e: unknown) => e);
    assert.ok(commit instanceof TransactionAborted);
    assert.equal(commit.cause, original);
    assert.equal(await c.count(), 0);
  }));

for (const applicationError of [false, true]) {
  test(`a joined ${applicationError ? "application" : "database"} failure in a savepoint leaves the outer usable`, () =>
    run(async (c) => {
      const seen: string[] = [];
      await c.insert("dup");
      await c.db.transaction(async () => {
        await c.insert("before");
        await assert.rejects(c.db.transaction(async () => {
          await c.insert("lost");
          await c.db.onCommit(() => void seen.push("inner"));
          await assert.rejects(c.db.transaction(async () => {
            if (applicationError) throw new TypeError("joined savepoint failed");
            await c.insert("dup");
          }, { savepoint: false }), applicationError ? TypeError : IntegrityError);
        }), TransactionAborted);
        await c.insert("after");
        await c.db.onCommit(() => void seen.push("outer"));
      });
      assert.equal(await c.count(), 3);
      assert.deepEqual(seen, ["outer"]);
    }));
}

for (const applicationError of [false, true]) {
  test(`a caught data migration failure (${applicationError ? "application" : "database"}) rolls back SQL, data, history and callbacks`, () =>
    run(async (c) => {
      const dir = mkdtempSync(join(tmpdir(), "monorm-tx-failure-"));
      const original = new TypeError("data migration failed");
      const path = join(dir, "0001_failure");
      mkdirSync(path);
      writeFileSync(join(path, "up.sql"), "CREATE TABLE migration_probe (id INTEGER)");
      writeFileSync(join(path, "down.sql"), "DROP TABLE migration_probe");
      writeFileSync(join(path, "snapshot.json"), c.db.registry.prepare().snapshot());
      const seen: string[] = [];
      try {
        await c.insert("dup");
        const migrator = new Migrator(c.db, new Migrations(dir, "unused.prisma"));
        await migrator.loadPending(null);
        const aborted = await migrator.apply([[new Migration("0001_failure", path), async (db) => {
          await db.execute("INSERT INTO migration_probe VALUES (1)");
          await c.insert("lost");
          if (applicationError) {
            await assert.rejects(db.transaction(async () => { throw original; }, { savepoint: false }), TypeError);
          } else {
            await assert.rejects(c.insert("dup"), IntegrityError);
          }
          await db.onCommit(() => void seen.push("ran"));
        }, false]]).catch((e: unknown) => e);
        assert.ok(aborted instanceof TransactionAborted);
        if (applicationError) assert.equal(aborted.cause, original);
        else assert.ok(aborted.cause instanceof IntegrityError);
        assert.equal(await c.count(), 1);
        assert.deepEqual(seen, []);
        await assert.rejects(c.db.fetch("SELECT * FROM migration_probe"), DatabaseError);
        assert.deepEqual(await c.db.fetch("SELECT * FROM monorm_migrations"), []);
        await c.db.execute("DROP TABLE monorm_migrations");
      } finally {
        rmSync(dir, { recursive: true, force: true });
      }
    }));
}

const followSource = `datasource db { provider = "sqlite" }
model Person {
  id Int @id @default(autoincrement())
  name String
  following Person[] @relation(through: Follow, through_fields: [follower, followee])
}
model Follow {
  id Int @id @default(autoincrement())
  follower_id Int
  followee_id Int
  follower Person @relation("follower", fields: [follower_id], references: [id])
  followee Person @relation("followee", fields: [followee_id], references: [id])
  @@unique([follower_id, followee_id])
}`;

interface Linked {
  db: Database;
  rel: { set(keys: unknown[]): Promise<unknown> };
  bad: unknown;
  kept: unknown;
  count(): Promise<number>;
  links(): Promise<unknown[]>;
}

// `body` gets a person that follows one other, a key that fails to link, and the follow count.
async function runLinked(body: (c: Linked) => Promise<void>): Promise<void> {
  const pg = getDatabase();
  const alice = await User.objects.insert({ email: "a@x.io", name: "A" });
  const post = await Post.objects.insert({ author: alice, title: "t", body: "b" });
  const [t1, t2] = (await Tag.objects.insertMany([{ name: "a" }, { name: "b" }]).returning()) as [Tag, Tag];
  await post.tags.add(t1);
  await body({
    db: pg,
    rel: post.tags as never,
    bad: t2.id + 10_000n,
    kept: t1.id,
    count: () => PostTag.objects.count(),
    links: async () => (await PostTag.objects.all()).map((l) => l.tagId),
  });
  const registry = new Registry();
  const models = loads(followSource, { registry }) as Record<string, any>;
  const lite = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await lite.createTables();
    const { Person, Follow } = models;
    const [ann, bob, cat] = await Person.objects.using(lite).insertMany([{ name: "a" }, { name: "b" }, { name: "c" }]).returning();
    await ann.following.using(lite).add(bob);
    await body({
      db: lite,
      rel: ann.following.using(lite),
      bad: cat.id + 1000,
      kept: bob.id,
      count: () => Follow.objects.using(lite).count(),
      links: async () => (await Follow.objects.using(lite).all()).map((l: any) => l.followeeId),
    });
  } finally {
    await lite.close();
  }
}

test("a failed related set marks the enclosing transaction", () =>
  runLinked(async (c) => {
    const err = await c.db
      .transaction(async () => {
        await assert.rejects(c.rel.set([c.bad]), DatabaseError);
        await c.db.onCommit(() => undefined);
      })
      .then(
        () => assert.fail("must throw"),
        (e: unknown) => e,
      );
    assert.ok(err instanceof TransactionAborted);
    assert.ok(err.cause instanceof DatabaseError);
    assert.equal(await c.count(), 1);
  }));

test("a failed related set outside a transaction stays atomic", () =>
  runLinked(async (c) => {
    await assert.rejects(c.rel.set([c.bad]), DatabaseError);
    assert.deepEqual(await c.links(), [c.kept]);
  }));

test("a nested transaction recovers a failed related set", () =>
  runLinked(async (c) => {
    await c.db.transaction(async () => {
      await assert.rejects(c.db.transaction(() => c.rel.set([c.bad])), DatabaseError);
      assert.equal(await c.count(), 1);
    });
    assert.deepEqual(await c.links(), [c.kept]);
  }));

test("concurrent nested transactions raise", { timeout: 10000 }, () =>
  run(async (c) => {
    let started!: () => void;
    let attempted!: () => void;
    const ready = new Promise<void>((resolve) => { started = resolve; });
    const finished = new Promise<void>((resolve) => { attempted = resolve; });
    await c.db.transaction(async () => {
      await Promise.all([
        c.db.transaction(async () => {
          await c.insert("first");
          started();
          await finished;
        }),
        (async () => {
          await ready;
          try {
            const ownershipError = (e: unknown) => e instanceof QueryError && e.message.includes("concurrent nested transactions share one connection");
            for (let i = 0; i < 2; i++) {
              await assert.rejects(c.db.transaction(async () => {
                assert.fail("overlapping sibling entered");
              }), ownershipError);
            }
            await assert.rejects(c.db.execute("SELECT 1"), ownershipError);
          } finally {
            attempted();
          }
        })(),
      ]);
    });
    assert.equal(await c.count(), 1);
    await c.db.transaction(async () => {
      for (const tag of ["one", "two"]) {
        await c.db.transaction(() => c.db.transaction(() => c.insert(tag)));
      }
    });
    assert.equal(await c.count(), 3);
  }),
);
