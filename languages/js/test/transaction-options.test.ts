/**
 * Transaction options: `isolation` and `timeout` of `db.transaction(fn, options)`.
 *
 * The timeout is one deadline for the whole outermost transaction: waiting for a connection, setup,
 * statements, idle time and commit. The cases run on Postgres and on SQLite where the behavior is shared.
 * Cases that need a driver seam (a stalled BEGIN or COMMIT, a failed ROLLBACK) are Rust tests in
 * crates/engine/tests/transaction_deadline.rs.
 */
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { connect, DatabaseError, loads, QueryError, Registry, TransactionTimeoutError, type Database, type QueryEvent } from '../src/index.js';

const DATABASE_URL = process.env['ORM_TEST_DATABASE_URL'] ?? 'postgres://postgres:postgres@localhost/orm_test';
const SQLITE = 'datasource db { provider = "sqlite" }\nmodel Unused {\n  id Int @id\n}\n';
const TIMEOUT = 0.3;
type Kind = 'postgres' | 'sqlite';
// A statement that runs for a long time and stops only when the engine interrupts it.
const LONG: Record<Kind, string> = {
  postgres: 'SELECT pg_sleep(30)',
  sqlite: 'WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 2000000000) SELECT count(*) FROM c',
};
// A statement far below the deadline of the cumulative test, also on a loaded machine with the debug addon.
const SHORT: Record<Kind, string> = {
  postgres: 'SELECT pg_sleep(0.1)',
  sqlite: 'WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 20000) SELECT count(*) FROM c',
};

class Backend {
  readonly opened: Database[] = [];
  constructor(readonly kind: Kind, readonly url: string) {}

  async open(maxConnections = 4): Promise<Database> {
    const registry = new Registry();
    if (this.kind === 'sqlite') loads(SQLITE, { registry });
    const db = await connect(this.url, { registry, default: false, maxConnections });
    this.opened.push(db);
    return db;
  }

  async count(db: Database = this.opened[0]!): Promise<number> {
    return Number((await db.fetch('SELECT count(*) AS n FROM tx_opt'))[0]!['n']);
  }
}

/** Runs `body` once for each backend, on a fresh `tx_opt` table. */
async function onBackends(body: (b: Backend) => Promise<void>, kinds: Kind[] = ['postgres', 'sqlite']): Promise<void> {
  for (const kind of kinds) {
    const dir = mkdtempSync(join(tmpdir(), 'monorm-txopt-'));
    const b = new Backend(kind, kind === 'postgres' ? DATABASE_URL : `sqlite://${join(dir, 'tx.db')}`);
    try {
      const first = await b.open();
      await first.execute('DROP TABLE IF EXISTS tx_opt');
      await first.execute('CREATE TABLE tx_opt (id integer PRIMARY KEY, v integer NOT NULL)');
      await body(b);
    } finally {
      if (kind === 'postgres' && b.opened[0]) await b.opened[0].execute('DROP TABLE IF EXISTS tx_opt');
      for (const db of b.opened) await db.close();
      rmSync(dir, { recursive: true, force: true });
    }
  }
}

function gate() {
  let open!: () => void;
  const promise = new Promise<void>((resolve) => { open = resolve; });
  return { promise, open };
}

function watch(db: Database): QueryEvent[] {
  const events: QueryEvent[] = [];
  db.onQuery((e) => { events.push(e); });
  return events;
}

const isTimeout = (outcome = 'rolled_back') => (e: unknown) => {
  assert.ok(e instanceof TransactionTimeoutError, String(e));
  assert.ok(e instanceof DatabaseError);
  assert.equal(e.outcome, outcome);
  return true;
};

async function isolationOf(db: Database): Promise<unknown> {
  return (await db.fetch('SHOW transaction_isolation'))[0]!['transaction_isolation'];
}

/** Postgres sessions of the test database that sit in an open transaction. */
async function openTransactions(db: Database): Promise<number> {
  const rows = await db.fetch("SELECT count(*) AS n FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid() AND state LIKE 'idle in transaction%'");
  return Number(rows[0]!['n']);
}

const never = async (): Promise<never> => { throw new Error('the body must not run'); };

test('options validate before sql', async () => {
  await onBackends(async (b) => {
    const db = await b.open();
    const events = watch(db);
    const bad: Record<string, unknown>[] = [
      { isolation: 'bogus' }, { isolation: 'READ COMMITTED' }, { isolation: 3 },
      { timeout: 0 }, { timeout: -1 }, { timeout: NaN }, { timeout: Infinity }, { timeout: 1e300 },
      { timeout: true }, { timeout: false }, { timeout: '1' }, { isolation: 'serializable', timeout: 0 },
    ];
    for (const options of bad) {
      await assert.rejects(db.transaction(never, options as never), (e) => e instanceof QueryError, JSON.stringify(options));
    }
    assert.deepEqual(events, []);
    // A short duration is a deadline, never "disabled".
    await assert.rejects(db.transaction(() => db.execute('SELECT 1'), { timeout: 1e-9 }), isTimeout());
    await db.transaction(() => db.execute('INSERT INTO tx_opt VALUES (1, 1)'), {});
    assert.equal(await b.count(db), 1);
  });
});

test('nested options reject', async () => {
  await onBackends(async (b) => {
    const db = await b.open();
    const events = watch(db);
    await db.transaction(async () => {
      await db.execute('INSERT INTO tx_opt VALUES (1, 1)');
      const before = events.length;
      for (const options of [{ isolation: 'serializable' }, { timeout: 5 }, { savepoint: false, timeout: 5 }, { savepoint: false, isolation: 'read_committed' }]) {
        await assert.rejects(db.transaction(never, options as never), (e) => e instanceof QueryError);
      }
      assert.equal(events.length, before);
      await db.transaction(() => db.execute('INSERT INTO tx_opt VALUES (2, 2)'), {});
    });
    assert.equal(await b.count(db), 2);
  });
});

test('outer no savepoint accepts options', async () => {
  await onBackends(async (b) => {
    const db = await b.open();
    await db.transaction(async () => {
      await db.execute('INSERT INTO tx_opt VALUES (1, 1)');
      await db.transaction(() => db.execute('INSERT INTO tx_opt VALUES (2, 2)'), { savepoint: false });
    }, { savepoint: false, isolation: 'repeatable_read', timeout: 5 });
    assert.equal(await b.count(db), 2);
    await assert.rejects(db.transaction(() => db.execute(LONG[b.kind]), { savepoint: false, timeout: TIMEOUT }), isTimeout());
  });
});

test('isolation levels', async () => {
  await onBackends(async (b) => {
    const db = await b.open();
    if (b.kind === 'sqlite') {
      // SQLite runs every level as serializable; it accepts them all.
      let i = 0;
      for (const level of ['read_committed', 'repeatable_read', 'serializable'] as const) {
        await db.transaction(() => db.execute(`INSERT INTO tx_opt VALUES (${i}, ${i})`), { isolation: level });
        i += 1;
      }
      assert.equal(await b.count(db), 3);
      // A second writer on the same file waits for the first one and runs after its commit.
      const other = await b.open();
      let settled = false;
      let waiting: Promise<void> = Promise.resolve();
      await db.transaction(async () => {
        await db.execute('INSERT INTO tx_opt VALUES (10, 10)');
        waiting = other.transaction(() => other.execute('INSERT INTO tx_opt VALUES (11, 11)'), { isolation: 'serializable' }).then(() => { settled = true; });
        await new Promise((resolve) => setTimeout(resolve, 200));
        assert.equal(settled, false);
      }, { isolation: 'serializable' });
      await waiting;
      assert.equal(await b.count(db), 5);
      return;
    }
    const other = await b.open();
    const names = { read_committed: 'read committed', repeatable_read: 'repeatable read', serializable: 'serializable' } as const;
    for (const [level, name] of Object.entries(names)) {
      await db.transaction(async () => assert.equal(await isolationOf(db), name), { isolation: level as keyof typeof names });
    }
    await db.execute('INSERT INTO tx_opt VALUES (1, 1)');
    // Read committed sees a commit that happens inside the transaction; repeatable read does not.
    await db.transaction(async () => {
      assert.equal(await b.count(db), 1);
      await other.execute('INSERT INTO tx_opt VALUES (2, 2)');
      assert.equal(await b.count(db), 2);
    }, { isolation: 'read_committed' });
    await db.transaction(async () => {
      assert.equal(await b.count(db), 2);
      await other.execute('INSERT INTO tx_opt VALUES (3, 3)');
      assert.equal(await b.count(db), 2);
    }, { isolation: 'repeatable_read' });
    assert.equal(await b.count(db), 3);
    // A serializable write skew: one of the two transactions fails and nothing retries it.
    await db.execute('DELETE FROM tx_opt');
    const read = [gate(), gate()] as const;
    const dbs = [db, other] as const;
    const skew = (me: 0 | 1) => dbs[me].transaction(async () => {
      await dbs[me].fetch('SELECT count(*) AS n FROM tx_opt');
      read[me].open();
      await read[1 - me]!.promise;
      await dbs[me].execute(`INSERT INTO tx_opt VALUES (${10 + me}, 1)`);
    }, { isolation: 'serializable' });
    const results = await Promise.allSettled([skew(0), skew(1)]);
    const failures = results.filter((r): r is PromiseRejectedResult => r.status === 'rejected');
    assert.equal(failures.length, 1);
    assert.ok(failures[0]!.reason instanceof DatabaseError);
    assert.equal(failures[0]!.reason.sqlstate, '40001');
    assert.equal(await b.count(db), 1);
  });
});

test('isolation does not leak', async () => {
  await onBackends(async (b) => {
    const solo = await b.open(1);
    await solo.transaction(async () => assert.equal(await isolationOf(solo), 'serializable'), { isolation: 'serializable' });
    await solo.transaction(async () => assert.equal(await isolationOf(solo), 'read committed'));
    await assert.rejects(solo.transaction(() => solo.execute(LONG.postgres), { isolation: 'repeatable_read', timeout: TIMEOUT }), isTimeout());
    await solo.transaction(async () => assert.equal(await isolationOf(solo), 'read committed'));
    assert.equal(await isolationOf(solo), 'read committed');
  }, ['postgres']);
});

test('timeout waiting for connection', async () => {
  await onBackends(async (b) => {
    const solo = await b.open(1);
    const events = watch(solo);
    const held = gate(), release = gate();
    const owner = solo.transaction(async () => {
      await solo.execute('INSERT INTO tx_opt VALUES (1, 1)');
      held.open();
      await release.promise;
    });
    await held.promise;
    const before = events.length;
    const start = Date.now();
    await assert.rejects(solo.transaction(never, { timeout: TIMEOUT }), isTimeout());
    assert.ok(Date.now() - start < 3000);
    assert.equal(events.length, before);
    release.open();
    await owner;
    assert.equal(await b.count(solo), 1);
  });
});

test('timeout during begin', async () => {
  await onBackends(async (b) => {
    if (b.kind === 'postgres') {
      const solo = await b.open(1);
      // BEGIN needs a round trip, so a deadline of a nanosecond passes in it or right after it.
      // A stalled BEGIN is a Rust test with a driver seam.
      await assert.rejects(solo.transaction(() => solo.execute('SELECT 1'), { isolation: 'serializable', timeout: 1e-9 }), isTimeout());
      await delay(200);
      assert.equal(await openTransactions(await b.open()), 0);
      await solo.transaction(() => solo.execute('INSERT INTO tx_opt VALUES (1, 1)'));
      return;
    }
    // SQLite: another connection holds the write lock, and BEGIN IMMEDIATE waits for it.
    const holder = await b.open(), waiter = await b.open();
    const held = gate(), release = gate();
    const owner = holder.transaction(async () => {
      await holder.execute('INSERT INTO tx_opt VALUES (1, 1)');
      held.open();
      await release.promise;
    });
    await held.promise;
    const start = Date.now();
    await assert.rejects(waiter.transaction(never, { timeout: TIMEOUT }), isTimeout());
    assert.ok(Date.now() - start < 3000);
    release.open();
    await owner;
    assert.equal(await b.count(waiter), 1);
    await waiter.transaction(() => waiter.execute('INSERT INTO tx_opt VALUES (2, 2)'));
  });
});

test('timeout at transaction start', async () => {
  // The deadline has passed at once, so it ends the start of the transaction: in the acquisition or in
  // BEGIN and the settings. Both leave nothing open when the error arrives. The Rust fake-driver tests
  // pin each phase; a real server cannot slow the settings step deterministically.
  await onBackends(async (b) => {
    const solo = await b.open(1);
    await solo.tenant(7, async () => {
      await assert.rejects(solo.transaction(() => solo.execute('SELECT 1'), { timeout: 1e-9 }), isTimeout());
      assert.equal(await openTransactions(await b.open()), 0);
      await solo.transaction(async () => {
        assert.equal((await solo.fetch("SELECT current_setting('app.tenant', true) AS t"))[0]!['t'], '7');
      });
    });
  }, ['postgres']);
});

test('timeout running statement', async () => {
  await onBackends(async (b) => {
    const db = await b.open(1);
    const start = Date.now();
    await assert.rejects(db.transaction(async () => {
      await db.execute('INSERT INTO tx_opt VALUES (1, 1)');
      await db.execute(LONG[b.kind]);
    }, { timeout: TIMEOUT }), (e) => {
      isTimeout()(e);
      // The cancel is a client-side deadline: the server holds no SQLSTATE for it.
      if (b.kind === 'postgres') assert.ok([null, '57014'].includes((e as DatabaseError).sqlstate));
      return true;
    });
    assert.ok(Date.now() - start < 5000);
    assert.equal(await b.count(db), 0);
  });
});

test('timeout is cumulative', async () => {
  await onBackends(async (b) => {
    const db = await b.open(1);
    let done = 0;
    const start = Date.now();
    await assert.rejects(db.transaction(async () => {
      // Bound by time, not by count: the deadline ends the loop on a fast build too.
      while (Date.now() - start < 3000) {
        await db.execute(SHORT[b.kind]);
        done += 1;
      }
    }, { timeout: 0.6 }), isTimeout());
    assert.ok(Date.now() - start < 5000);
    // Each statement is shorter than the deadline; their sum is not.
    assert.ok(done >= 1);
  });
});

test('timeout while body idle', async () => {
  await onBackends(async (b) => {
    const solo = await b.open(1);
    const observer = b.kind === 'postgres' ? solo : await b.open();
    const observed = gate();
    const task = (async () => {
      await delay(50);
      // Needs the connection (Postgres) or the write lock (SQLite) that the idle body holds.
      await observer.transaction(() => observer.execute('INSERT INTO tx_opt VALUES (7, 7)'));
      observed.open();
    })();
    const start = Date.now();
    await assert.rejects(solo.transaction(async () => {
      await solo.execute('INSERT INTO tx_opt VALUES (1, 1)');
      // The body stays idle: the engine releases the lease at the deadline, without the body.
      await observed.promise;
      await solo.execute('SELECT 1');
    }, { timeout: TIMEOUT }), isTimeout());
    await task;
    assert.ok(Date.now() - start < 5000);
    assert.deepEqual((await solo.fetch('SELECT id FROM tx_opt')).map((r) => Number(r['id'])), [7]);
  });
});

test('timeout invalidates savepoints', async () => {
  await onBackends(async (b) => {
    const db = await b.open(1);
    const events = watch(db);
    await assert.rejects(db.transaction(async () => {
      await db.execute('INSERT INTO tx_opt VALUES (1, 1)');
      try {
        await db.transaction(() => db.execute(LONG[b.kind]));
      } catch (e) {
        assert.ok(e instanceof TransactionTimeoutError);
      }
      const before = events.length;
      // The caught timeout ended the whole transaction: no savepoint, no statement.
      await assert.rejects(db.transaction(() => db.execute('SELECT 1')), isTimeout());
      await assert.rejects(db.execute('SELECT 1'), isTimeout());
      // The engine refuses each one with the timeout error: no statement runs.
      assert.ok(events.slice(before).every((e) => e.error?.includes('timeout')));
    }, { timeout: TIMEOUT }), isTimeout());
    assert.equal(await b.count(db), 0);
    // An ordinary failure in a savepoint still recovers.
    await db.transaction(async () => {
      await db.execute('INSERT INTO tx_opt VALUES (1, 1)');
      await assert.rejects(db.transaction(() => db.execute('INSERT INTO tx_opt VALUES (1, 2)')), (e) => e instanceof DatabaseError);
      await db.execute('INSERT INTO tx_opt VALUES (2, 2)');
    });
    assert.equal(await b.count(db), 2);
  });
});

test('timeout invalidates joined scope', async () => {
  await onBackends(async (b) => {
    const db = await b.open(1);
    await assert.rejects(db.transaction(async () => {
      await db.execute('INSERT INTO tx_opt VALUES (1, 1)');
      try {
        await db.transaction(() => db.execute(LONG[b.kind]), { savepoint: false });
      } catch (e) {
        assert.ok(e instanceof TransactionTimeoutError);
      }
      await assert.rejects(db.execute('SELECT 1'), isTimeout());
    }, { timeout: TIMEOUT }), isTimeout());
    assert.equal(await b.count(db), 0);
  });
});

test('caught timeout prevents commit', async () => {
  await onBackends(async (b) => {
    const db = await b.open(1);
    const seen: string[] = [];
    // The body catches the error and leaves normally: the exit still reports it, and nothing commits.
    await assert.rejects(db.transaction(async () => {
      await db.execute('INSERT INTO tx_opt VALUES (1, 1)');
      await db.onCommit(() => { seen.push('committed'); });
      try {
        await db.execute(LONG[b.kind]);
      } catch (e) {
        assert.ok(e instanceof TransactionTimeoutError);
      }
    }, { timeout: TIMEOUT }), isTimeout());
    assert.deepEqual(seen, []);
    assert.equal(await b.count(db), 0);
  });
});

test('reuse after timeout', async () => {
  await onBackends(async (b) => {
    const db = await b.open(1);
    for (const round of [0, 1]) {
      await assert.rejects(db.transaction(() => db.execute(LONG[b.kind]), { timeout: TIMEOUT }), isTimeout());
      await db.transaction(() => db.execute(`INSERT INTO tx_opt VALUES (${round}, ${round})`), { timeout: 5 });
      assert.equal(Number((await db.fetch('SELECT 1 AS one'))[0]!['one']), 1);
    }
    assert.equal(await b.count(db), 2);
  });
});

test('stale task after timeout', async () => {
  await onBackends(async (b) => {
    const db = await b.open(1);
    const go = gate(), finished = gate();
    const outcome: unknown[] = [];
    let task: Promise<void> = Promise.resolve();
    await assert.rejects(db.transaction(async () => {
      // The task inherits the handle of this transaction.
      task = (async () => {
        await go.promise;
        try {
          await db.execute('INSERT INTO tx_opt VALUES (99, 99)');
        } catch (e) {
          outcome.push(e);
        }
        finished.open();
      })();
      await db.execute(LONG[b.kind]);
    }, { timeout: TIMEOUT }), isTimeout());
    await db.transaction(async () => {
      // The new transaction holds the lease the old one had.
      go.open();
      await finished.promise;
      await db.execute('INSERT INTO tx_opt VALUES (1, 1)');
    });
    await task;
    assert.equal(outcome.length, 1);
    assert.deepEqual((await db.fetch('SELECT id FROM tx_opt')).map((r) => Number(r['id'])), [1]);
  });
});

test('callbacks outside deadline', async () => {
  await onBackends(async (b) => {
    const db = await b.open(1);
    const seen: string[] = [];
    await db.transaction(async () => {
      await db.execute('INSERT INTO tx_opt VALUES (1, 1)');
      await db.onCommit(async () => {
        await delay(800);
        seen.push('slow');
      });
    }, { timeout: 0.5 });
    assert.deepEqual(seen, ['slow']);
    assert.equal(await b.count(db), 1);
  });
});
