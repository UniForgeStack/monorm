/** `monorm/testing` `isolated()`: rollback and truncate modes on SQLite and PostgreSQL. */
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { connect, getDatabase, loads, NotConnected, QueryError, Registry, TransactionRequired } from "../src/index.js";
import { isolated, runOnCommit } from "../src/testing.js";

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const postgres = process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";

function source(provider: string): string {
  return `datasource db {
  provider = "${provider}"
}
model Item {
  id   Int    @id @default(autoincrement())
  name String
  @@map("iso_items")
}
`;
}

async function withItems(provider: string, url: string, body: (Item: Any) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const Item = (loads(source(provider), { registry }) as Record<string, Any>).Item;
  const db = await connect(url, { registry, maxConnections: 4 });
  try {
    await db.dropTables();
    await db.createTables();
    await body(Item);
  } finally {
    await db.dropTables().catch(() => undefined);
    await db.close().catch(() => undefined);
  }
}

// One test for each database: SQLite always, PostgreSQL when it answers.
function scenario(what: string, body: (Item: Any) => Promise<void>): void {
  test(`${what} (sqlite)`, async () => {
    await withItems("sqlite", `sqlite://${join(mkdtempSync(join(tmpdir(), "iso-")), "iso.db")}`, body);
  });
  test(`${what} (postgres)`, async (t) => {
    try {
      await withItems("postgresql", postgres, body);
    } catch (e) {
      if (e instanceof Error && e.name === "DatabaseError") return t.skip(`PostgreSQL not reachable: ${e.message}`);
      throw e;
    }
  });
}

scenario("rollback leaves no rows in both forms", async (Item) => {
  const form = isolated(async () => {
    await Item.objects.insertMany([{ name: "x" }, { name: "y" }]);
    assert.equal(await Item.objects.count(), 2);
  });
  await form(); // the function form: a test function
  assert.equal(await Item.objects.count(), 0);
  await isolated(async () => {
    await Item.objects.insertMany([{ name: "z" }]);
    assert.equal(await Item.objects.count(), 1);
  }); // the awaited wrapper
  assert.equal(await Item.objects.count(), 0);
  const echo = isolated(async (n: number) => n + 1);
  assert.equal(await echo(1), 2, "the function form passes its arguments and gives the result");
});

scenario("an exception in the test still rolls back", async (Item) => {
  await assert.rejects(
    isolated(async () => {
      await Item.objects.insertMany([{ name: "x" }]);
      throw new RangeError("boom");
    }),
    RangeError,
  );
  assert.equal(await Item.objects.count(), 0);
});

scenario("a nested transaction is a savepoint", async (Item) => {
  await isolated(async () => {
    const db = getDatabase();
    await assert.rejects(
      db.transaction(async () => {
        await Item.objects.insertMany([{ name: "gone" }]);
        throw new RangeError("x");
      }),
      RangeError,
    );
    await db.transaction(async () => {
      await Item.objects.insertMany([{ name: "kept" }]);
    });
    assert.deepEqual((await Item.objects.all()).map((i: Any) => i.name), ["kept"]);
  });
  assert.equal(await Item.objects.count(), 0);
});

scenario("on_commit callbacks are dropped and run on request", async () => {
  const calls: string[] = [];
  await isolated(async () => {
    const db = getDatabase();
    await db.transaction(async () => {
      await db.onCommit(() => calls.push("inner"));
    });
    await db.onCommit(() => calls.push("outer"));
  });
  assert.equal(calls.length, 0);
  await isolated(async () => {
    await getDatabase().onCommit(() => calls.push("run"));
    assert.equal(await runOnCommit(), 1);
    assert.deepEqual(calls, ["run"]);
    assert.equal(await runOnCommit(), 0);
  });
  await assert.rejects(runOnCommit(), TransactionRequired);
});

scenario("truncate empties every table and restarts ids", async (Item) => {
  await isolated(
    async () => {
      await getDatabase().transaction(async () => {
        await Item.objects.insertMany([{ name: "x" }, { name: "y" }]);
      });
      assert.equal(await Item.objects.count(), 2);
    },
    { mode: "truncate" },
  );
  await isolated(
    async () => {
      assert.equal(await Item.objects.count(), 0);
      await Item.objects.insertMany([{ name: "x" }]);
      assert.deepEqual((await Item.objects.all()).map((i: Any) => i.id), [1]);
    },
    { mode: "truncate" },
  );
});

scenario("a wrong mode and no default database raise", async () => {
  await assert.rejects(isolated(async () => undefined, { mode: "fast" as Any }), TypeError);
  await getDatabase().close();
  await assert.rejects(isolated(async () => undefined), NotConnected);
});

scenario("parallel savepoints raise with the hint under rollback only", async (Item) => {
  const both = async () => {
    const db = getDatabase();
    const one = (i: number) =>
      db.transaction(async () => {
        await new Promise((r) => setTimeout(r, 50));
        await Item.objects.insertMany([{ name: String(i) }]);
      });
    for (const r of await Promise.allSettled([one(1), one(2)])) if (r.status === "rejected") throw r.reason;
  };
  await assert.rejects(isolated(both), (e: unknown) => e instanceof QueryError && e.message.includes('mode: "truncate"'));
});
