/**
 * Async field adapters: one write pipeline for fields whose value needs I/O before the SQL.
 *
 * A write runs `validate` (sync, no I/O) for every adapted value, then `await prepare` for each
 * value that needs it, then the SQL. `decode` runs on read. The ORM never deletes a prepared
 * object. If the SQL fails, the error holds the prepared operation, and `operation.execute()`
 * retries the SQL with no new `prepare` call.
 */
import { Expression } from "./expr.js";
import { RESERVED, type ModelMeta, type Row } from "./model.js";
import type { BatchOptions, InsertMany, InsertOne, InspectedStatement, PreparedInsert, PreparedUpdate } from "./query.js";
import type { ModelSpec } from "./meta.js";
import type { QueryInspection } from "./inspection.js";

/** What `validate` returns for a value that needs `prepare`. `placeholder` is a stored value of
 * the right type that checks the statement before any `prepare` call; it is never written. */
export class Pending {
  constructor(readonly value: unknown, readonly placeholder: unknown) {}
}

/** The operations of one adapted field. */
export interface FieldAdapter {
  /** Return the stored value, or a {@link Pending} when `prepare` must run. Sync, no I/O. */
  validate(value: unknown): unknown;
  /** Turn the `value` of a {@link Pending} into the stored value. */
  prepare(value: unknown): Promise<unknown>;
  /** Turn a loaded stored value into the value the model shows. Sync, no I/O. */
  decode(stored: unknown): unknown;
}

/** An adapted value or statement that the pipeline rejects before any `prepare` call. */
export class FieldAdapterError extends TypeError {}

/** A `prepare` call or the SQL failed. `stored` holds the values that `prepare` produced. After
 * every `prepare` call finished, `operation.execute()` retries the SQL with no new `prepare` call. */
export class AdapterWriteError extends Error {
  constructor(readonly operation: PreparedWrite, cause: unknown) {
    super("adapted write failed; inspect error.stored and error.operation", { cause });
  }
  /** By field. A batch write names a value `<row index>.<field>`. */
  get stored(): Readonly<Record<string, unknown>> { return this.operation.stored; }
}

/** The `prepare` calls that one batch write runs at the same time, at most. */
const CONCURRENCY = 16;

type Values = Record<string, unknown>;
/** A batch value also holds its place (`at`). */
type PendingFields = Map<string, { adapter: FieldAdapter; value: unknown; at?: { row: number; field: string } }>;
type UpdateManyOptions = { readonly batchSize?: number; readonly returning?: boolean };
interface AdaptedQuerySet {
  prepareInsert(values: Values): PreparedInsert<ModelSpec>;
  prepareUpdate(values: Values): PreparedUpdate<ModelSpec>;
  insert(values: never): { inspect(): QueryInspection };
  update(values: never, options: { readonly returning: boolean }): { inspect(): QueryInspection };
  checkInsertRows(rows: readonly object[], batchSize: number | null): void;
  insertRows(rows: readonly object[], conflict: undefined, returning: boolean, batchSize: number | null): Promise<unknown[] | number>;
  inspectInsert(rows: readonly object[], conflict: undefined, returning: boolean, batchSize: number | null): QueryInspection;
  checkUpdateRows(rows: readonly object[], options: UpdateManyOptions): void;
  executeUpdateMany(rows: readonly object[], options: UpdateManyOptions): Promise<number | Row[]>;
  inspectUpdateMany(rows: readonly object[], options: UpdateManyOptions): QueryInspection;
}
type Statement<T> = (stored: Values) => Promise<T>;

const ADAPTERS = new WeakMap<ModelMeta, Map<string, FieldAdapter>>();

/** Adapt `meta.field`: its writes run the pipeline and its loaded value reads through `decode`.
 * `methods` become instance methods; a name that a model member already uses throws `FieldAdapterError`. */
export function registerFieldAdapter(meta: ModelMeta, field: string, adapter: FieldAdapter, methods: Readonly<Record<string, (this: Row, ...args: never[]) => unknown>> = {}): void {
  const registered = ADAPTERS.get(meta) ?? new Map<string, FieldAdapter>();
  if (registered.has(field)) throw new FieldAdapterError(`${meta.name}.${field} already has a field adapter`);
  if (!meta.fieldList.some(f => f.name === field)) throw new FieldAdapterError(`${meta.name} has no field ${field}`);
  const prototype = meta.Row.prototype as object;
  for (const name of Object.keys(methods)) {
    if (RESERVED.has(name) || name in prototype || meta.fieldList.some(f => f.name === name) || meta.relations.has(name)) {
      throw new FieldAdapterError(`${meta.name}.${name}: adapter method collides with a model member`);
    }
  }
  for (const [name, value] of Object.entries(methods)) Object.defineProperty(prototype, name, { value, configurable: true, writable: true });
  ADAPTERS.set(meta, registered);
  registered.set(field, adapter);
  meta.addRowDecoder((row: Row) => { if (Object.hasOwn(row, field)) row[field] = adapter.decode(row[field]); });
}

/** @internal Validate the adapted values: the values that check the statement, and the pending
 * ones by field. `undefined` when `values` has no adapted field. */
export function splitAdapted(meta: ModelMeta, values: object): { checked: Values; pending: PendingFields } | undefined {
  const adapters = ADAPTERS.get(meta);
  if (!adapters || !Object.keys(values).some(name => adapters.has(name))) return undefined;
  const checked: Values = { ...values };
  const pending: PendingFields = new Map();
  for (const [name, value] of Object.entries(values)) {
    const adapter = adapters.get(name);
    if (adapter === undefined || value === undefined) continue;
    if (value instanceof Expression) throw new FieldAdapterError(`${meta.name}.${name}: an adapted field takes no expression`);
    const result = adapter.validate(value);
    if (result instanceof Pending) {
      pending.set(name, { adapter, value: result.value });
      checked[name] = result.placeholder;
    } else checked[name] = result;
  }
  return { checked, pending };
}

/** @internal Validate the adapted values of every row of a batch write: the rows that check the
 * statement, and the pending values by `<row index>.<field>`. */
export function splitBatch<R extends object>(meta: ModelMeta, rows: readonly R[]): { checked: readonly R[]; pending: PendingFields } {
  const pending: PendingFields = new Map();
  if (!ADAPTERS.has(meta)) return { checked: rows, pending };
  const checked = rows.map((row, index) => {
    const adapted = splitAdapted(meta, row);
    if (adapted === undefined) return row;
    for (const [field, value] of adapted.pending) pending.set(`${index}.${field}`, { ...value, at: { row: index, field } });
    return adapted.checked as R;
  });
  return { checked, pending };
}

/** @internal Run the `prepare` calls of the adapted `values` once. Return `values` with the stored
 * values in place of the pending ones. */
export async function prepareValues(meta: ModelMeta, values: Values): Promise<Values> {
  const adapted = splitAdapted(meta, values);
  if (adapted === undefined || adapted.pending.size === 0) return { ...values };
  const operation = new PreparedWrite(adapted.pending);
  await operation.prepare();
  return { ...values, ...operation.stored };
}

/** Copies of `rows` with the stored values in the place of their placeholders. */
function fillRows(rows: readonly object[], stored: Values, pending: PendingFields): Values[] {
  const filled: Values[] = rows.map(row => ({ ...row }));
  for (const [key, value] of Object.entries(stored)) {
    const { row, field } = pending.get(key)!.at!;
    filled[row]![field] = value;
  }
  return filled;
}

/** The `prepare` calls of one statement, run once. A failed `prepare` call is final, because a
 * stream cannot be replayed. With a `limit`, up to `limit` calls run at the same time; after a failure,
 * the calls that run finish and no new call starts. */
export class PreparedWrite {
  // One object in several slots (a shared one-shot stream) prepares once.
  readonly #groups: { adapter: FieldAdapter; value: unknown; keys: string[] }[] = [];
  readonly #limit: number | undefined;
  readonly #stored: Values = {};
  #state: "new" | "preparing" | "ready" | "failed" = "new";
  #executing = false;
  #statement: Statement<unknown> | undefined;

  /** @internal */
  constructor(pending: PendingFields, limit?: number) {
    const shared = new Map<FieldAdapter, Map<object, string[]>>();
    for (const [key, { adapter, value }] of pending) {
      const byValue = value !== null && (typeof value === "object" || typeof value === "function") ? (shared.get(adapter) ?? shared.set(adapter, new Map()).get(adapter)!) : undefined;
      const keys = byValue?.get(value as object);
      if (keys !== undefined) keys.push(key);
      else {
        const group = { adapter, value, keys: [key] };
        this.#groups.push(group);
        byValue?.set(value as object, group.keys);
      }
    }
    this.#limit = limit;
  }

  /** The stored values that `prepare` produced so far, by field. A batch write names a value `<row index>.<field>`. */
  get stored(): Readonly<Values> { return { ...this.#stored }; }

  /** Run the `prepare` calls without a statement. A later `execute` runs no new call. */
  async prepare(): Promise<void> {
    if (this.#executing) throw new FieldAdapterError("adapted write already executing");
    this.#executing = true;
    try {
      await this.#prepare();
    } catch (error) {
      throw error instanceof AdapterWriteError ? error : new AdapterWriteError(this, error);
    } finally {
      this.#executing = false;
    }
  }

  /** Run `prepare` once, then `statement(stored)`. Without `statement`, run the last one again. */
  async execute<T>(statement?: Statement<T>): Promise<T> {
    if (statement !== undefined) this.#statement = statement;
    if (this.#statement === undefined) throw new FieldAdapterError("no statement to run");
    if (this.#executing) throw new FieldAdapterError("adapted write already executing");
    this.#executing = true;
    try {
      await this.#prepare();
      return (await this.#statement(this.stored)) as T;
    } catch (error) {
      throw error instanceof AdapterWriteError ? error : new AdapterWriteError(this, error);
    } finally {
      this.#executing = false;
    }
  }

  async #prepare(): Promise<void> {
    if (this.#state === "ready") return;
    if (this.#state !== "new") throw new FieldAdapterError("prepare is running or failed; its input cannot be replayed");
    this.#state = "preparing";
    try {
      if (this.#limit === undefined) {
        for (const group of this.#groups) this.#store(group, await group.adapter.prepare(group.value));
      } else await this.#prepareConcurrently(this.#limit);
      this.#state = "ready";
    } catch (error) {
      this.#state = "failed";
      throw error;
    }
  }

  #store(group: { keys: string[] }, stored: unknown): void {
    for (const key of group.keys) this.#stored[key] = stored;
  }

  async #prepareConcurrently(limit: number): Promise<void> {
    const queue = [...this.#groups];
    const failures: unknown[] = [];
    const worker = async (): Promise<void> => {
      for (let next = queue.shift(); next !== undefined && failures.length === 0; next = queue.shift()) {
        try {
          this.#store(next, await next.adapter.prepare(next.value));
        } catch (error) {
          failures.push(error);
        }
      }
    };
    await Promise.all(Array.from({ length: Math.min(limit, queue.length) }, worker));
    if (failures.length > 0) throw failures[0];
  }
}

/** @internal `insert(values)` with a value that needs `prepare`. `inspect()` shows the statement over the
 * placeholder values and runs no `prepare` call. */
export function adaptedInsert<M extends ModelSpec>(Insert: typeof InsertOne, qs: AdaptedQuerySet, checked: Values, pending: PendingFields): InsertOne<M> & { readonly operation: PreparedWrite; prepare(): Promise<void> } {
  const operation = new PreparedWrite(pending);
  const insert = new Insert<M>(async () => {
    // Statement and write protection are checked before the first `prepare` call.
    const prepared = qs.prepareInsert(checked);
    return [await operation.execute(stored => prepared.execute({ ...checked, ...stored } as never))];
  }, () => qs.insert(checked as never).inspect());
  return Object.assign(insert, {
    operation,
    /** Run the checks and the `prepare` calls now, so the awaited insert runs no `prepare` call. */
    async prepare(): Promise<void> {
      qs.prepareInsert(checked);
      await operation.prepare();
    },
    onConflict(): never { throw new FieldAdapterError("a value that needs prepare is unsupported in conflict writes"); },
  });
}

/** @internal `update(values)` with a value that needs `prepare`: one `prepare` call, and the stored value goes to every matching row. */
export function adaptedUpdate(Statement: typeof InspectedStatement, qs: AdaptedQuerySet, checked: Values, pending: PendingFields, returning: boolean): InspectedStatement<number | Row[]> & { readonly operation: PreparedWrite } {
  const operation = new PreparedWrite(pending);
  const update = new Statement<number | Row[]>(async () => {
    const prepared = qs.prepareUpdate(checked);
    return (await operation.execute(stored => prepared.execute({ ...checked, ...stored } as never, { returning }))) as number | Row[];
  }, () => qs.update(checked as never, { returning }).inspect());
  return Object.assign(update, { operation });
}

/** @internal `insertMany(rows)` with a value that needs `prepare`. `inspect()` shows the statement over the
 * placeholder values and runs no `prepare` call. */
export function adaptedInsertMany<M extends ModelSpec>(Insert: typeof InsertMany, qs: AdaptedQuerySet, rows: readonly object[], pending: PendingFields, options: BatchOptions | undefined): InsertMany<M> & { readonly operation: PreparedWrite } {
  const batchSize = options?.batchSize ?? null;
  const operation = new PreparedWrite(pending, CONCURRENCY);
  const insert = new Insert<M>(async (_conflict, returning) => {
    qs.checkInsertRows(rows, batchSize); // write protection runs before the first `prepare` call
    return operation.execute(stored => qs.insertRows(fillRows(rows, stored, pending), undefined, returning, batchSize));
  }, (_conflict, returning) => qs.inspectInsert(rows, undefined, returning, batchSize));
  return Object.assign(insert, {
    operation,
    onConflict(): never { throw new FieldAdapterError("a value that needs prepare is unsupported in conflict writes"); },
  });
}

/** @internal `updateMany(rows)` with a value that needs `prepare`. */
export function adaptedUpdateMany(Statement: typeof InspectedStatement, qs: AdaptedQuerySet, rows: readonly object[], pending: PendingFields, options: UpdateManyOptions): InspectedStatement<number | Row[]> & { readonly operation: PreparedWrite } {
  const operation = new PreparedWrite(pending, CONCURRENCY);
  const update = new Statement<number | Row[]>(async () => {
    qs.checkUpdateRows(rows, options);
    return operation.execute(stored => qs.executeUpdateMany(fillRows(rows, stored, pending), options));
  }, () => qs.inspectUpdateMany(rows, options));
  return Object.assign(update, { operation });
}
