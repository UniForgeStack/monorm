/** Connections and transactions. */

import { AsyncLocalStorage } from "node:async_hooks";

import { DatabaseError, LockNotAvailable, NotConnected, QueryError, TransactionAborted, TransactionRequired } from "./errors.js";
import type { IR } from "./expr.js";
import { registry as defaultRegistry, type ModelMeta, type Registry } from "./model.js";
import { call, native, wait, type NativeEngine, type NativePreparedRead, type NativeTrace, type NativeTransaction } from "./native.js";
import { allowedWrites } from "./protection.js";
import { active as debugging, capture, record, writeKey } from "./debug.js";

let defaultDb: Database | undefined;

// Native marks carry text; retain application exceptions without keeping ended
// transactions alive, and replace only the matching text cause.
const failures = new WeakMap<NativeTransaction, { message: string; cause: unknown }>();
/** @internal Restores the original exception behind a native failed mark. */
export async function restoreCause<T>(pending: Promise<T>, tx: NativeTransaction | null): Promise<T> {
  try {
    return await pending;
  } catch (error) {
    const failure = tx === null ? undefined : failures.get(tx);
    if (failure && error instanceof TransactionAborted && error.cause instanceof DatabaseError
      && error.cause.constructor === DatabaseError && error.cause.message === failure.message) {
      error.cause = failure.cause;
    }
    throw error;
  }
}

/** For each database (its root) with an open transaction in the current async context: the innermost one. */
const current = new AsyncLocalStorage<ReadonlyMap<Database, NativeTransaction>>();
/** For each database in a `tenant()` call: its primary and replica engines with the tenant set. */
const tenants = new AsyncLocalStorage<ReadonlyMap<Database, { readonly primary: NativeEngine; readonly replicas: readonly NativeEngine[]; readonly id: string }>>();
/** For each database with an open transaction in this async context: the tenant id it began under. */
const txTenants = new AsyncLocalStorage<ReadonlyMap<Database, string | undefined>>();
/** The `scope.<name>` values of default filters. */
const scopeValues = new AsyncLocalStorage<Readonly<Record<string, unknown>>>();
/** For each database with an open transaction: the onCommit callbacks of the innermost one. */
const callbacks = new AsyncLocalStorage<ReadonlyMap<Database, (() => unknown)[]>>();
/** The callback lists of transactions that committed or rolled back: a call started in the
 * transaction can still see its list after that. */
const ended = new WeakSet<(() => unknown)[]>();
/** Set while a query hook runs: the statements it sends give no event. */
const inHook = new AsyncLocalStorage<true>();

/** The isolation levels of {@link TransactionOptions.isolation}. */
export type Isolation = "read_committed" | "repeatable_read" | "serializable";

export interface TransactionOptions {
  /** With `false`, a nested call joins the enclosing transaction with no savepoint. */
  readonly savepoint?: boolean;
  /** The isolation level of an outermost transaction. SQLite accepts every level and runs it as serializable. */
  readonly isolation?: Isolation;
  /** Seconds, finite and above zero: one deadline for the whole transaction. */
  readonly timeout?: number;
}

/** Rejects a bad option before any SQL; the engine checks the values themselves. */
function checkTransactionOptions(options: TransactionOptions, nested: boolean): void {
  const { isolation, timeout } = options;
  if (nested && (isolation != null || timeout != null)) {
    throw new QueryError("a nested transaction() takes no isolation or timeout; it uses those of the outermost transaction");
  }
  if (isolation != null && typeof isolation !== "string") {
    throw new QueryError(`transaction isolation must be a string, got ${typeof isolation}`);
  }
  if (timeout != null && typeof timeout !== "number") {
    throw new QueryError(`transaction timeout must be a number of seconds, got ${typeof timeout}`);
  }
}

export interface LockOptions {
  /** A shared lock (any number of shared holders, but no exclusive one). */
  readonly exclusive?: boolean;
  /** Give `false` instead of waiting when the lock is held. */
  readonly nowait?: boolean;
  readonly session?: false;
}

/** One statement the database ran, given to the hooks of {@link Database.onQuery}. */
export interface QueryEvent {
  /** The SQL with its parameter placeholders: the statement shape, without values. */
  readonly sql: string;
  /** When the statement started, in Unix milliseconds (`Date.now()`). */
  readonly start: number;
  /** How long it ran, in milliseconds. */
  readonly duration: number;
  /** Rows returned, or rows affected by a statement that returns no rows. */
  readonly rows: number;
  /** The database's error message when the statement failed, or `"cancelled"` when the call
   * ended before the statement did. */
  readonly error: string | null;
  /** True for SQL the caller wrote as a string with no parameters ({@link Database.execute}): it
   * can hold literal values. ORM-built SQL and {@link Database.fetch} are `false`. */
  readonly raw: boolean;
}

/** @internal What an engine call tells the tracer: the parts of a write's N+1 key, and whether its SQL is raw. */
export interface CallInfo {
  readonly key?: readonly unknown[];
  readonly raw?: boolean;
}

export interface SessionLockOptions {
  /** Hold the lock while the function runs, on a connection of its own, outside any
   * transaction. */
  readonly session: true;
  /** A shared lock (any number of shared holders, but no exclusive one). */
  readonly exclusive?: boolean;
  /** Throw `LockNotAvailable` at once when the lock is held. */
  readonly nowait?: boolean;
  /** Seconds to wait before `LockNotAvailable` (no limit when absent or `Infinity`). */
  readonly timeout?: number;
}

/** Postgres `lock_timeout` takes at most 2^31 - 1 ms. */
const MAX_TIMEOUT = 2147483.647;

/** Statements a replica may answer (the IR starts with its `op`). */
const READ = /^\{"op":"(select|count|exists|compound)"/;

/** Sets `errors` as the `cause` of `error`, unless there are none or it has a cause. */
function causedBy(error: unknown, errors: readonly unknown[]): void {
  if (errors.length && error instanceof Error && error.cause === undefined) {
    error.cause = errors.length === 1 ? errors[0] : new AggregateError(errors);
  }
}

/**
 * @internal Runs `fn` with `tx` as the current transaction of `db`; does not end it.
 * Gives the `onCommit` callbacks registered in `fn`, for the caller to run after it commits.
 */
export async function inTransaction(db: Database, tx: NativeTransaction, fn: () => Promise<unknown>): Promise<(() => unknown)[]> {
  const mine: (() => unknown)[] = [];
  const scoped = new Map(callbacks.getStore() ?? []).set(db.root, mine);
  const txs = new Map(current.getStore() ?? []).set(db.root, tx);
  try {
    await current.run(txs, async () => await callbacks.run(scoped, async () => await fn()));
  } finally {
    ended.add(mine);
  }
  return mine;
}

/** The callback list of a `bindRequest` block: a callback pushed to it throws. */
function noCommitWork(): (() => unknown)[] {
  const list: (() => unknown)[] = [];
  list.push = (...fns) => {
    if (fns.length > 0) {
      throw new TransactionRequired("onCommit() in a bindRequest() block: the commit happens outside Node; do the work after it");
    }
    return 0;
  };
  return list;
}

/** A connection pool. Created by {@link connect}. */
export class Database {
  /** Hooks live on the root: `primary` views share them. */
  readonly #hooks = new Map<object, (event: QueryEvent) => unknown>();
  private turn = 0;
  /** @internal The database a `primary` view belongs to: they share transactions and callbacks. */
  root: Database = this;

  /** @internal */
  constructor(
    private readonly base: NativeEngine,
    readonly url: string,
    readonly registry: Registry,
    private readonly replicas: readonly NativeEngine[] = [],
    /** @internal `connect(url, { allowUnfilteredWrites: true })`: `update()` and `delete()` take a query set with no filter. */
    readonly allowUnfilteredWrites: boolean = false,
  ) {}

  /**
   * Calls `hook(event)` after each statement this database runs (prefetch queries and
   * `updateMany` batches included), also when it fails or the call ends before it does.
   * Gives a function that removes the hook.
   *
   * The hook runs in the async context that sent the query, after the ORM call ends, so
   * `AsyncLocalStorage` values (a current span, a request id) are those of the caller.
   * The ORM awaits a hook that returns a promise. A statement that a hook sends gives no
   * event.
   *
   * Every hook gets every event. A hook error never replaces the error of the call: when
   * the call failed, its own error rejects, and the hook errors become its `cause` (an
   * `AggregateError` for several) unless it has one. When the call succeeded, the first
   * hook error rejects it, and the others are its `cause`.
   */
  onQuery(hook: (event: QueryEvent) => unknown): () => void {
    const key = {};
    this.root.#hooks.set(key, hook);
    return () => {
      this.root.#hooks.delete(key);
    };
  }

  /** @internal Runs an engine call in the current transaction; traced when a hook or an
   * N+1 scope listens. */
  send<T>(start: (tx: NativeTransaction | null, trace: NativeTrace | null) => Promise<T>, call: CallInfo = {}): Promise<T> {
    const tx = this.tx();
    if (inHook.getStore() || (!this.root.#hooks.size && !debugging())) return restoreCause(wait(() => start(tx, null)), tx);
    const trace = new (native().Trace)();
    return this.#observe(trace, capture(), restoreCause(wait(() => start(tx, trace)), tx), call);
  }

  async #observe<T>(trace: NativeTrace, origin: ReturnType<typeof capture>, pending: Promise<T>, call: CallInfo): Promise<T> {
    let result: T;
    try {
      result = await pending;
    } catch (error) {
      await this.#report(trace, origin, { error }, call);
      throw error;
    }
    await this.#report(trace, origin, null, call);
    return result;
  }

  /** Gives the events of a call to the N+1 finder and to every hook. `failed` is the error of
   * the call: it stays, and the hook errors become its `cause`. Throws the first hook error
   * when the call did not fail. */
  async #report(trace: NativeTrace, origin: ReturnType<typeof capture>, failed: { readonly error: unknown } | null, call: CallInfo): Promise<void> {
    const events: QueryEvent[] = trace.take().map((e) => ({ ...e, error: e.error ?? null, raw: call.raw === true }));
    if (origin) {
      // A write counts by its call, a read by its SQL shape; each key once per engine call.
      const callKey = call.key ? writeKey(call.key) : null;
      const seen = new Set<string>();
      for (const e of events) {
        const key = callKey ?? e.sql;
        if (seen.has(key)) continue;
        seen.add(key);
        record(key, e.sql, origin);
      }
    }
    const errors: unknown[] = [];
    for (const hook of [...this.root.#hooks.values()]) {
      for (const e of events) {
        try {
          await inHook.run(true, async () => await hook(e));
        } catch (error) {
          errors.push(error);
        }
      }
    }
    if (failed) return causedBy(failed.error, errors);
    const [first, ...rest] = errors;
    if (errors.length) {
      causedBy(first, rest);
      throw first;
    }
  }

  /** @internal The primary's engine, with the tenant of an enclosing `tenant()` call. */
  get engine(): NativeEngine {
    return tenants.getStore()?.get(this.root)?.primary ?? this.base;
  }

  /** This database without its replicas: every statement goes to the primary. */
  get primary(): Database {
    const view = new Database(this.base, this.url, this.registry, [], this.allowUnfilteredWrites);
    view.root = this.root;
    return view;
  }

  /** @internal The transaction queries on this database run in, if any. */
  tx(): NativeTransaction | null {
    return current.getStore()?.get(this.root) ?? null;
  }

  /** @internal The engine for a read: the next replica outside a transaction, else the primary. */
  reader(): NativeEngine {
    if (this.replicas.length === 0 || this.tx() !== null) {
      return this.engine;
    }
    const replicas = tenants.getStore()?.get(this.root)?.replicas ?? this.replicas;
    this.turn = (this.turn + 1) % replicas.length;
    return replicas[this.turn]!;
  }

  /**
   * Runs `fn` with a tenant: every transaction on this database in it first runs
   * `SELECT set_config('app.tenant', <id>, true)` (`SET LOCAL`), so Postgres row-level
   * security policies can read `current_setting('app.tenant')`. A statement outside a
   * transaction runs in a transaction of its own. A transaction that is already open keeps
   * its setting, so a call with another tenant while this database has a transaction open under a
   * different tenant (or none) throws `QueryError`. Gives what `fn` gives.
   */
  tenant<T>(id: string | number | bigint, fn: () => Promise<T>): Promise<T> {
    const engines = this.tenantEngines(id);
    const root = this.root;
    if (current.getStore()?.has(root) && txTenants.getStore()?.get(root) !== engines.id) {
      throw new QueryError(
        "db.tenant() inside a transaction opened under another tenant: the transaction keeps its tenant; open the transaction inside the tenant block",
      );
    }
    return tenants.run(new Map(tenants.getStore() ?? []).set(root, engines), async () => await fn());
  }

  /**
   * Runs `fn` on a transaction that something else opened and will end (commit or roll back).
   * Statements in `fn` run on `tx`. The transaction counts as opened under `options.tenant`, so
   * `db.tenant(sameId)` inside `fn` works, and `options.scope` sets the `scope.<name>` values.
   * `onCommit` inside `fn` throws `TransactionRequired`: the commit happens outside it. `fn` must not end `tx`. Gives what `fn` gives.
   */
  bindRequest<T>(
    tx: NativeTransaction,
    options: { readonly scope?: Readonly<Record<string, unknown>>; readonly tenant?: string | number | bigint },
    fn: () => Promise<T>,
  ): Promise<T> {
    const root = this.root;
    const engines = options.tenant === undefined ? undefined : this.tenantEngines(options.tenant);
    const tenantMap = new Map(tenants.getStore() ?? []);
    if (engines === undefined) tenantMap.delete(root);
    else tenantMap.set(root, engines);
    const bound = new Map(callbacks.getStore() ?? []).set(root, noCommitWork());
    const run = async (): Promise<T> => await fn();
    return current.run(new Map(current.getStore() ?? []).set(root, tx), () =>
      txTenants.run(new Map(txTenants.getStore() ?? []).set(root, engines?.id), () =>
        tenants.run(tenantMap, () =>
          callbacks.run(bound, () => scopeValues.run({ ...scopeValues.getStore(), ...options.scope }, run)))));
  }

  /** The primary and replica engines with the tenant `id` set. */
  private tenantEngines(id: string | number | bigint) {
    if (this.url.startsWith("sqlite://")) {
      throw new QueryError("db.tenant() sets a Postgres setting for row-level security; sqlite has none");
    }
    if (!["string", "number", "bigint"].includes(typeof id) || (typeof id === "number" && !Number.isFinite(id))) {
      throw new TypeError(`tenant id must be a string or a finite number, got ${String(id)}`);
    }
    if (id === "") {
      throw new RangeError("tenant id must not be empty: a pooled connection reads '' for no tenant");
    }
    const root = this.root, value = [String(id)];
    return {
      id: value[0]!,
      primary: root.base.withSettings(["app.tenant"], value),
      replicas: root.replicas.map((r) => r.withSettings(["app.tenant"], value)),
    };
  }

  /** @internal */
  run(ir: IR, params: unknown[]): Promise<unknown> {
    return this.runJson(JSON.stringify(ir), params);
  }

  /** @internal Runs a prepared read on the reader engine; `read(engine)` gives that engine's statement. */
  runPrepared(read: (engine: NativeEngine) => NativePreparedRead, params: unknown[]): Promise<unknown> {
    const prepared = read(this.reader());
    return this.send((tx, trace) => prepared.run(params, tx, trace));
  }

  /** @internal */
  runJson(json: string, params: unknown[]): Promise<unknown> {
    [json, params] = withScope(json, params);
    const engine = READ.test(json) ? this.reader() : this.engine;
    const allowed = allowedWrites();
    return this.send((tx, trace) => engine.run(json, params, tx, allowed, trace));
  }

  /**
   * Runs `fn` in a transaction: commits when it resolves, rolls back when it throws.
   * Queries on this database inside it (in this async context, and in work it starts)
   * run in the transaction. Nested calls use savepoints. Gives what `fn` gives.
   *
   * A statement that fails marks its transaction (or savepoint) failed, also when `fn`
   * catches the error: later statements and the commit throw `TransactionAborted`
   * (`cause` is the first error), and nothing commits. Catch errors only around a
   * savepoint, a nested `db.transaction()` call, whose rollback leaves the enclosing
   * transaction usable.
   *
   * With `{ savepoint: false }` a nested call joins the enclosing transaction with no
   * savepoint: an error that leaves it marks the enclosing transaction failed. On an
   * outermost call the option has no effect.
   *
   * `isolation` sets the isolation level of an outermost transaction; SQLite accepts every
   * level and runs it as serializable. `timeout` is one deadline, in seconds, for the whole
   * transaction, counted from the call: it covers the wait for a connection, every
   * statement, idle time in `fn`, and the commit. When it passes, the engine stops the
   * running statement, rolls back, and throws `TransactionTimeoutError`; after that no
   * statement runs and no commit is sent. A nested call takes no options: passing
   * `isolation` or `timeout` throws `QueryError`, and it uses those of the outermost
   * transaction (also with `savepoint: false`).
   */
  async transaction<T>(fn: () => Promise<T>, options: TransactionOptions = {}): Promise<T> {
    const enclosing = this.tx();
    checkTransactionOptions(options, enclosing !== null);
    if (enclosing !== null && options.savepoint === false) {
      try {
        return await fn();
      } catch (e) {
        const message = e instanceof Error ? `${e.name}: ${e.message}` : String(e);
        enclosing.markFailed(message);
        if (!failures.has(enclosing)) failures.set(enclosing, { message, cause: e });
        throw e;
      }
    }
    const outer = enclosing === null;
    const tx = await restoreCause(wait(() => this.engine.begin(enclosing, options.isolation ?? null, options.timeout ?? null)), enclosing);
    const mine: (() => unknown)[] = [];
    const scoped = new Map(callbacks.getStore() ?? []).set(this.root, mine);
    let result: T;
    const txs = new Map(current.getStore() ?? []).set(this.root, tx);
    const txTenant = new Map(txTenants.getStore() ?? []).set(this.root, tenants.getStore()?.get(this.root)?.id);
    try {
      result = await current.run(txs, async () => await txTenants.run(txTenant, async () => await callbacks.run(scoped, async () => await fn())));
    } catch (e) {
      ended.add(mine);
      await wait(() => tx.rollback());
      throw e;
    }
    ended.add(mine);
    await restoreCause(wait(() => tx.commit()), tx);
    await this.afterCommit(mine, outer);
    return result;
  }

  /** A released savepoint hands its callbacks to the enclosing transaction. */
  private async afterCommit(mine: readonly (() => unknown)[], outer: boolean): Promise<void> {
    if (!outer) {
      callbacks.getStore()!.get(this.root)!.push(...mine);
      return;
    }
    for (const fn of mine) {
      await fn();
    }
  }

  /**
   * Calls `fn()` after the outermost transaction on this database commits; a rollback
   * drops it (a rolled-back savepoint drops only the callbacks registered inside it).
   * Outside a transaction, `fn()` runs at once. A promise result is awaited. Callbacks
   * run in order, outside the transaction; an error in one goes to the caller of
   * `transaction()` and the later ones do not run. A call that the transaction started and
   * that runs this after the transaction ended throws `TransactionRequired`.
   */
  async onCommit(fn: () => unknown): Promise<void> {
    const mine = callbacks.getStore()?.get(this.root);
    if (mine !== undefined && ended.has(mine)) {
      throw new TransactionRequired("onCommit(): the transaction of this call has ended");
    }
    if (mine !== undefined) {
      mine.push(fn);
      return;
    }
    await fn();
  }

  /**
   * Takes an advisory lock on `key`: a lock on a name rather than on rows ("only one
   * worker imports this file at a time").
   *
   * `db.lock(key, options)` holds the lock until the transaction ends and must run inside
   * `db.transaction()`. It waits for the lock unless `nowait`, which gives `false` instead
   * of waiting.
   *
   * `db.lock(key, { session: true, timeout }, fn)` holds the lock while `fn` runs, on a
   * connection of its own, with no transaction, and gives what `fn` gives. It waits at
   * most `timeout` seconds (no limit when absent or `Infinity`; not at all with `nowait`) and throws
   * `LockNotAvailable` when another session still holds the lock.
   *
   * A string key is hashed to a 64-bit one the way the Python package hashes it (the
   * first 8 bytes of its BLAKE2b digest, signed big-endian), so both lock the same name.
   */
  // The transaction form is last: `Parameters<Database["lock"]>` takes the last overload.
  lock<T>(key: bigint | number | string, options: SessionLockOptions, fn: () => Promise<T>): Promise<T>;
  lock(key: bigint | number | string, options?: LockOptions): Promise<boolean>;
  async lock<T>(
    key: bigint | number | string,
    options: LockOptions | SessionLockOptions = {},
    fn?: () => Promise<T>,
  ): Promise<boolean | T> {
    if (options.session === true) {
      return this.sessionLock(key, options, fn!);
    }
    if (fn !== undefined || (options as { timeout?: unknown }).timeout !== undefined) {
      throw new TypeError("db.lock(key, options, fn) and a timeout need `{ session: true }`");
    }
    if (this.url.startsWith("sqlite://")) {
      throw new QueryError("sqlite does not support advisory locks");
    }
    if (this.tx() === null) {
      throw new TransactionRequired(
        "db.lock() outside a transaction would release the lock at once; run it inside `db.transaction(...)` or use `{ session: true }`",
      );
    }
    const [k, name] = lockKey(key);
    const { exclusive = true, nowait = false } = options;
    return this.send((tx, trace) => this.engine.advisoryLock(k, name, Boolean(exclusive), Boolean(nowait), tx!, trace));
  }

  private async sessionLock<T>(key: bigint | number | string, options: SessionLockOptions, fn: () => Promise<T>): Promise<T> {
    if (this.url.startsWith("sqlite://")) {
      throw new QueryError("sqlite does not support advisory locks");
    }
    if (typeof fn !== "function") {
      throw new TypeError("db.lock(key, { session: true }, fn) needs the function to run under the lock");
    }
    const [k, name] = lockKey(key);
    const { exclusive = true, nowait = false, timeout } = options;
    if (timeout !== undefined && !(timeout === Infinity || (timeout >= 0 && timeout <= MAX_TIMEOUT))) {
      throw new RangeError(`lock timeout must be a number of seconds from 0 to ${MAX_TIMEOUT}, or Infinity`);
    }
    const timeoutMs = timeout === undefined || timeout === Infinity ? null : Math.min(Math.ceil(timeout * 1000), 2 ** 31 - 1);
    const engine = this.engine;
    const held = await this.send((_, trace) => engine.sessionLock(k, name, Boolean(exclusive), Boolean(nowait), timeoutMs, trace));
    if (held === null) {
      const after = nowait || timeout === undefined ? "" : ` after ${timeout}s`;
      throw new LockNotAvailable(`advisory lock ${JSON.stringify(String(key))} is held by another session${after}`);
    }
    try {
      return await fn();
    } finally {
      await this.send((_, trace) => held.release(trace));
    }
  }

  /** Runs raw SQL (one or more statements); gives the number of rows affected. */
  execute(sql: string): Promise<number> {
    return this.send((tx, trace) => this.engine.execute(sql, tx, trace), { raw: true });
  }

  /**
   * Runs one raw SQL query with parameters; gives its rows as objects by column name.
   *
   * Placeholders are `$1, $2, ...` on Postgres and `?` on SQLite. A parameter's type
   * comes from its JS value (`bigint` and integer numbers are `bigint`, strings `text`,
   * plain objects and arrays JSON); cast in the SQL where the column needs another type
   * (`$1::uuid`). Cells come back by the column types the database reports. Runs in the
   * current transaction, and query hooks see it.
   */
  fetch(sql: string, ...params: unknown[]): Promise<Record<string, unknown>[]> {
    return this.send((tx, trace) => this.engine.fetch(sql, params, tx, trace));
  }

  /** Raw query whose columns are all read as text. For tooling (migrations). */
  fetchText(sql: string): Promise<(string | null)[][]> {
    return wait(() => this.engine.fetchText(sql, this.tx()));
  }

  /**
   * Creates the whole schema (extensions, tables, constraints, indexes, functions,
   * triggers) with `IF NOT EXISTS` / `OR REPLACE` DDL, in one transaction. A development
   * and test helper: it never alters what exists. Evolving databases use migrations.
   */
  createTables(): Promise<void> {
    return wait(() => this.engine.createTables());
  }

  /**
   * `REFRESH MATERIALIZED VIEW` for a materialized view model (PostgreSQL only).
   * `{ concurrently: true }` keeps the view readable during the refresh; PostgreSQL needs a
   * unique index on the view for it. Rejects with `QueryError` for a plain view, any other
   * model, and SQLite. Runs in the current transaction.
   */
  refresh(model: { readonly _meta: ModelMeta }, options: { concurrently?: boolean } = {}): Promise<void> {
    return wait(() => this.engine.refresh(model._meta.name, options.concurrently ?? false, this.tx()));
  }

  /** `DROP TABLE ... CASCADE` every model table and drop generated functions;
   * extensions stay. */
  dropTables(): Promise<void> {
    return wait(() => this.engine.dropTables());
  }

  /** Closes the pools of the primary and of each replica. */
  async close(): Promise<void> {
    if (this.root !== this) {
      return this.root.close();
    }
    await wait(() => this.base.close());
    for (const replica of this.replicas) {
      await wait(() => replica.close());
    }
    if (defaultDb === this) {
      defaultDb = undefined;
    }
  }

  toString(): string {
    return `Database(${this.url.split("@").pop()})`;
  }
}

export interface ConnectOptions {
  readonly maxConnections?: number;
  /** Make this the database queries use unless `.using(db)` says otherwise. */
  readonly default?: boolean;
  /** The models to compile (the default registry unless given). */
  readonly registry?: Registry;
  /** Switch database capabilities off (`"ilike"`, `"update_from_values"`, ...) to test
   * the SQL other databases get. */
  readonly disable?: readonly string[];
  /** URLs of read replicas: reads outside a transaction go to one of them, in turn;
   * writes and every statement in a transaction go to the primary. */
  readonly replicas?: readonly string[];
  /** Let `update()` and `delete()` run on a query set with no filter (they throw
   * `UnfilteredWriteError` otherwise; `.all()` marks one that writes every row on purpose). */
  readonly allowUnfilteredWrites?: boolean;
}

/**
 * Opens a connection pool. Import your model modules first: the schema is compiled from
 * the models registered at this point.
 */
export async function connect(url: string, options: ConnectOptions = {}): Promise<Database> {
  const reg = options.registry ?? defaultRegistry;
  const schema = reg.native();
  const open = (u: string) =>
    wait(() => call(() => native().connect(u, schema, options.maxConnections ?? 10, [...(options.disable ?? [])])));
  const engine = await open(url);
  const readers: NativeEngine[] = [];
  try {
    for (const replica of options.replicas ?? []) {
      readers.push(await open(replica));
    }
  } catch (e) {
    for (const opened of [engine, ...readers]) {
      await wait(() => opened.close());
    }
    throw e;
  }
  const db = new Database(engine, url, reg, readers, options.allowUnfilteredWrites ?? false);
  if (options.default ?? true) {
    defaultDb = db;
  }
  return db;
}

/** The default database ({@link connect}ed last with `default: true`). */
export function getDatabase(): Database {
  if (defaultDb === undefined) {
    throw new NotConnected("no default database; call `await connect(url)` first");
  }
  return defaultDb;
}

/**
 * Runs `fn` with the values that `scope.<name>` reads in default filters
 * (`@@query.filter("tenant", "shop_id == scope.shop")`). A query on a model whose default filter
 * reads a value that no enclosing `scope()` sets throws `QueryError`. Inner values
 * replace outer ones. Gives what `fn` gives.
 */
export function scope<T>(values: Readonly<Record<string, unknown>>, fn: () => Promise<T>): Promise<T> {
  // `await` inside: a returned thenable (a query set) must run while the values are set.
  return scopeValues.run({ ...scopeValues.getStore(), ...values }, async () => await fn());
}

/** @internal The values `scope.<name>` reads in this async context. */
export function currentScope(): Readonly<Record<string, unknown>> {
  return scopeValues.getStore() ?? {};
}

/** @internal `json` (a statement's IR, or with `key` a list wrapped under it) with the
 * scope's parameter indexes, and `params` with the scope's values appended. */
export function withScope(json: string, params: unknown[], key?: string): [string, unknown[]] {
  const values = scopeValues.getStore();
  if (values === undefined || Object.keys(values).length === 0) {
    return [json, params];
  }
  const names = Object.keys(values);
  const index = JSON.stringify(Object.fromEntries(names.map((n, i) => [n, params.length + i])));
  const op = key === undefined ? `${json.slice(0, -1)}${json.slice(1, -1).trim() ? "," : ""}"scope":${index}}` : `{"${key}":${json},"scope":${index}}`;
  return [op, [...params, ...names.map((n) => values[n])]];
}

/** A validated lock key: a decimal 64-bit integer, or the UTF-8 name the engine hashes. */
function lockKey(key: bigint | number | string): [string, Buffer | null] {
  if (typeof key === "string") {
    return ["0", Buffer.from(new TextEncoder().encode(key))];
  }
  if (typeof key === "bigint" || Number.isSafeInteger(key)) {
    const k = BigInt(key);
    if (k !== BigInt.asIntN(64, k)) {
      throw new RangeError("lock key must fit in 64 bits");
    }
    return [String(k), null];
  }
  if (typeof key === "number" && Number.isInteger(key)) {
    throw new TypeError(`lock key ${key} is not a safe integer: pass it as a bigint`);
  }
  throw new TypeError(`lock key must be an integer or a string, got ${String(key)}`);
}

/** `db`, or the default database. */
export function resolve(db: Database | undefined): Database {
  return db ?? getDatabase();
}
