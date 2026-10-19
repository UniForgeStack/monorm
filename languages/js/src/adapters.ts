/**
 * Async field adapters: one write pipeline for fields whose value needs I/O before the SQL.
 *
 * A write runs `validate` (sync, no I/O) for every adapted value, then `await prepare` for each
 * value that needs it, then the SQL. `decode` runs on read. The ORM never deletes a prepared
 * object. If the SQL fails, the error holds the prepared operation, and `operation.execute()`
 * retries the SQL with no new `prepare` call. This language adapter declares the
 * `async-field-adapter.v1` capability.
 */
import { Expression } from "./expr.js";
import type { ModelMeta, Row } from "./model.js";
import type { PreparedInsert, PreparedUpdate } from "./query.js";
import type { ModelSpec } from "./meta.js";

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
  get stored(): Readonly<Record<string, unknown>> { return this.operation.stored; }
}

type Values = Record<string, unknown>;
type PendingFields = Map<string, { adapter: FieldAdapter; value: unknown }>;
interface AdaptedQuerySet {
  prepareInsert(values: Values): PreparedInsert<ModelSpec>;
  prepareUpdate(values: Values): PreparedUpdate<ModelSpec>;
}
type Statement<T> = (stored: Values) => Promise<T>;

const ADAPTERS = new WeakMap<ModelMeta, Map<string, FieldAdapter>>();

/** Adapt `meta.field`: its writes run the pipeline and its loaded value reads through `decode`. */
export function registerFieldAdapter(meta: ModelMeta, field: string, adapter: FieldAdapter): void {
  const registered = ADAPTERS.get(meta) ?? new Map<string, FieldAdapter>();
  if (registered.has(field)) throw new FieldAdapterError(`${meta.name}.${field} already has a field adapter`);
  if (!meta.fieldList.some(f => f.name === field)) throw new FieldAdapterError(`${meta.name} has no field ${field}`);
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

/** @internal Validate the adapted values of every row of a batch write. A value that needs `prepare`
 * is not supported in a batch yet: it would store the raw input. */
export function checkedRows<R extends object>(meta: ModelMeta, rows: readonly R[], statement: string): readonly R[] {
  if (!ADAPTERS.has(meta)) return rows;
  return rows.map(row => {
    const adapted = splitAdapted(meta, row);
    if (adapted === undefined) return row;
    if (adapted.pending.size > 0) {
      throw new FieldAdapterError(`${meta.name}.${[...adapted.pending.keys()].join(", ")}: ${statement} takes no value that needs prepare yet`);
    }
    return adapted.checked as R;
  });
}

/** The `prepare` calls of one statement, run once. A failed `prepare` call is final, because a
 * stream cannot be replayed. */
export class PreparedWrite {
  readonly #pending: PendingFields;
  readonly #stored: Values = {};
  #state: "new" | "preparing" | "ready" | "failed" = "new";
  #executing = false;
  #statement: Statement<unknown> | undefined;

  /** @internal */
  constructor(pending: PendingFields) { this.#pending = pending; }

  /** The stored values that `prepare` produced so far, by field. */
  get stored(): Readonly<Values> { return { ...this.#stored }; }

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
      for (const [name, { adapter, value }] of this.#pending) this.#stored[name] = await adapter.prepare(value);
      this.#state = "ready";
    } catch (error) {
      this.#state = "failed";
      throw error;
    }
  }
}

/** The thenable of an adapted write: it runs once, on the first `then`. A rejected write gives a
 * rejected promise. `operation` keeps the stored values for a retry. */
abstract class AdaptedWrite<T> implements PromiseLike<T> {
  readonly operation: PreparedWrite;
  #started: Promise<T> | undefined;
  constructor(pending: PendingFields) { this.operation = new PreparedWrite(pending); }
  protected abstract run(): Promise<T>;
  then<A = T, B = never>(onfulfilled?: ((value: T) => A | PromiseLike<A>) | null, onrejected?: ((reason: unknown) => B | PromiseLike<B>) | null): Promise<A | B> {
    return (this.#started ??= this.run()).then(onfulfilled, onrejected);
  }
  catch<B = never>(onrejected?: ((reason: unknown) => B | PromiseLike<B>) | null): Promise<T | B> { return this.then(undefined, onrejected); }
  finally(onfinally?: (() => void) | null): Promise<T> { return this.then().finally(onfinally); }
}

/** @internal `insert(values)` with a value that needs `prepare`. */
export class AdaptedInsert extends AdaptedWrite<Row> {
  constructor(private readonly qs: AdaptedQuerySet, private readonly checked: Values, pending: PendingFields) { super(pending); }
  onConflict(): never { throw new FieldAdapterError("a value that needs prepare is unsupported in conflict writes"); }
  protected async run(): Promise<Row> {
    // Statement and write protection are checked before the first `prepare` call.
    const prepared = this.qs.prepareInsert(this.checked);
    return (await this.operation.execute(stored => prepared.execute({ ...this.checked, ...stored } as never))) as Row;
  }
}

/** @internal `update(values)` with a value that needs `prepare`, on one unique row. */
export class AdaptedUpdate extends AdaptedWrite<number | Row[]> {
  constructor(private readonly qs: AdaptedQuerySet, private readonly checked: Values, pending: PendingFields, private readonly returning: boolean) { super(pending); }
  protected async run(): Promise<number | Row[]> {
    const prepared = this.qs.prepareUpdate(this.checked);
    if (!prepared.unique) throw new FieldAdapterError("an update with a value that needs prepare must target one unique row");
    return (await this.operation.execute(stored => prepared.execute({ ...this.checked, ...stored } as never, { returning: this.returning }))) as number | Row[];
  }
}

/** @internal The capability this language adapter provides. */
export const LANGUAGE_CAPABILITIES: ReadonlySet<string> = new Set(["async-field-adapter.v1"]);

/** @internal Fail when an extension of the artifact requires a capability that this language adapter lacks. */
export function checkRequires(artifact: { readonly requires?: Readonly<Record<string, readonly string[]>> }, declared: ReadonlySet<string> = LANGUAGE_CAPABILITIES): void {
  for (const [extension, capabilities] of Object.entries(artifact.requires ?? {})) {
    for (const capability of capabilities) {
      if (!declared.has(capability)) {
        throw new TypeError(`extension ${extension} requires the capability ${capability}, which this orm language adapter does not provide; upgrade orm or rebuild the artifact`);
      }
    }
  }
}
