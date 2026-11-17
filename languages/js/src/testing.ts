/** Test isolation: `isolated()` runs a test so that it leaves no rows. */

import { type Database, isolatedBlock, resolve } from "./db.js";
import { TransactionRequired } from "./errors.js";

/** `"rollback"` (default) holds the test in one transaction and rolls it back; `"truncate"` runs it
 * with no outer transaction, then empties every model table. */
export type IsolatedMode = "rollback" | "truncate";

export interface IsolatedOptions {
  /** The database to isolate; the default database when omitted. */
  db?: Database;
  mode?: IsolatedMode;
}

/** What `isolated(fn)` gives: `fn` as a test function, and a promise-like that runs `fn()` when awaited. */
export type IsolatedRun<A extends unknown[], T> = ((...args: A) => Promise<T>) & PromiseLike<T>;

class Rollback {}

async function run<T>(options: IsolatedOptions, body: () => Promise<T>): Promise<T> {
  const mode = options.mode ?? "rollback";
  if (mode !== "rollback" && mode !== "truncate") {
    throw new TypeError(`isolated() mode must be "rollback" or "truncate", got ${String(mode)}`);
  }
  const db = resolve(options.db);
  if (mode === "truncate") {
    try {
      return await body();
    } finally {
      await db.truncateTables();
    }
  }
  let result: T;
  try {
    await db.transaction(async () => {
      result = await isolatedBlock.run(true, body);
      throw new Rollback();
    });
  } catch (e) {
    if (e instanceof Rollback) return result!;
    throw e;
  }
  throw new Error("unreachable");
}

/**
 * Isolates a test from the others. Given a function, it gives a test function that runs `fn`
 * isolated with the arguments it gets (`test("x", isolated(async (t) => { ... }))`); the result
 * is also awaitable, which runs `fn()` at once (`await isolated(async () => { ... })`).
 * Do not return it from an async function: the promise would run it.
 *
 * `mode: "rollback"` opens a transaction on `db` (the default database), binds it to the
 * current async context and rolls it back at the end, also after a throw. `db.transaction()`
 * in the code under test is a savepoint and `onCommit` callbacks never run (see
 * {@link runOnCommit}). Sibling savepoints that run in parallel throw `QueryError`.
 * `mode: "truncate"` runs `fn` with no outer transaction, then empties every model table and
 * restarts identities. Throws `NotConnected` when there is no database.
 */
export function isolated<A extends unknown[], T>(fn: (...args: A) => Promise<T>, options: IsolatedOptions = {}): IsolatedRun<A, T> {
  const test = ((...args: A) => run(options, () => fn(...args))) as IsolatedRun<A, T>;
  (test as { then: PromiseLike<T>["then"] }).then = (resolveValue, rejectValue) =>
    run(options, () => (fn as () => Promise<T>)()).then(resolveValue, rejectValue);
  return test;
}

/** Runs the `onCommit` callbacks that the test registered under `isolated()` and clears them;
 * gives how many ran. Throws `TransactionRequired` outside a transaction. */
export async function runOnCommit(db?: Database): Promise<number> {
  const pending = resolve(db).pendingCallbacks();
  if (pending === undefined || pending.ended) {
    throw new TransactionRequired("runOnCommit(): no open isolated() or transaction block on this database");
  }
  const ready = pending.list.splice(0);
  for (const fn of ready) await fn();
  return ready.length;
}
