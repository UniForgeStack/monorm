/** A failed statement marks its transaction failed, also when the block catches the error. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, DatabaseError, getDatabase, IntegrityError, TransactionAborted, type Database } from "../src/index.js";
import { User } from "./blog/models.js";
import { Author, sqliteRegistry } from "./sqlite/models.js";
import { useDatabase } from "./helpers.js";

useDatabase();

interface Case {
  db: Database;
  insert(tag: string): Promise<unknown>;
  count(): Promise<number>;
}

async function run(_name: string, body: (c: Case) => Promise<void>): Promise<void> {
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

test("a caught error aborts the commit and drops the callbacks", () =>
  run("caught", async (c) => {
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
    assert.equal(await c.count(), 1);
    assert.deepEqual(seen, []);
  }));

test("a later statement raises TransactionAborted chained to the first error", () =>
  run("later", async (c) => {
    await c.insert("dup");
    await assert.rejects(
      c.db.transaction(async () => {
        const first = await c.insert("dup").catch((e: unknown) => e);
        assert.ok(first instanceof IntegrityError);
        const later = await c.insert("later").catch((e: unknown) => e);
        assert.ok(later instanceof TransactionAborted && later.cause instanceof IntegrityError);
        await assert.rejects(c.count(), TransactionAborted);
        throw first;
      }),
      IntegrityError,
    );
    assert.equal(await c.count(), 1);
  }));

test("a savepoint in a try keeps the other writes", () =>
  run("savepoint", async (c) => {
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
  run("inner", async (c) => {
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
  run("join", async (c) => {
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

test("an uncaught error rolls back and throws the original error", () =>
  run("uncaught", async (c) => {
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
