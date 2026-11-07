/**
 * Prepared writes: `prepare()` on an insert, update, delete or batch write.
 *
 * The statement compiles once, and the engine holds its SQL text and parameter map. A call
 * binds the `param()` values, and a batch call converts its rows:
 *
 * ```ts
 * const add = Author.objects.insert({ name: param("n"), bio: param("b") }).prepare();
 * const author = await add.run({ n: "A", b: "x" });
 *
 * const addMany = Author.objects.insertMany(param("rows", { fields: [Author.name, Author.bio] })).prepare();
 * const count = await addMany.run({ rows: [{ name: "A", bio: "x" }, { name: "B", bio: "y" }] });
 * ```
 */

import { Builder } from "./build.js";
import { currentScope } from "./db.js";
import { QueryError } from "./errors.js";
import { Column, Slot, SlotParams, type IR, type RowsRef } from "./expr.js";
import { emptyInspection, inspection, inspector, type QueryInspection } from "./inspection.js";
import { call, type NativeInspector, type NativePreparedRows, type NativeReturned } from "./native.js";
import type { ModelMeta } from "./model.js";
import type { FieldMeta } from "./meta.js";
import { allowedWrites } from "./protection.js";
import { Compiled, splitScope, type CallScope, type QuerySet, type Values } from "./query.js";
import { prepareInsertTemplate, requireFields } from "./write.js";

type AnyQuerySet = QuerySet<any, any, any, any, any, any>;
type Conflict = Parameters<AnyQuerySet["conflictPlan"]>[2];

/** What a call gives: the row count, one instance, an instance or `null`, or the instances. */
export type WriteShape = "count" | "one" | "optional" | "list";

/** The held statement behind a {@link PreparedWrite}. */
interface Held {
  readonly names: ReadonlySet<string>;
  run(values: Record<string, unknown> | undefined): Promise<unknown>;
  inspect(values: Record<string, unknown> | undefined): QueryInspection;
  sql(values: Record<string, unknown> | undefined): string;
}

/**
 * A write compiled by `prepare()`. `run(values)` gives the row count, or with `returning()` the
 * rows, or for an insert the instance. `values.scope` sets scope values for the call.
 */
export class PreparedWrite<R, P = {}> {
  readonly #held: Held;
  /** The names of the values each call takes. */
  readonly params: ReadonlySet<string>;

  /** @internal */
  constructor(held: Held) {
    this.#held = held;
    this.params = held.names;
  }

  run(...[values]: Values<P>): Promise<R> {
    return this.#held.run(values) as Promise<R>;
  }

  /** The statements a call with these values plans, as `inspect()` of the same write gives them. */
  inspect(...[values]: Values<P>): QueryInspection {
    return this.#held.inspect(values);
  }

  /** The SQL of one statement for these values, parameters inlined (for debugging). */
  sql(...[values]: Values<P>): string {
    return this.#held.sql(values);
  }
}

function known(names: ReadonlySet<string>, values: Record<string, unknown>): void {
  let hits = 0;
  let total = 0;
  for (const k in values) {
    total++;
    if (names.has(k)) hits++;
  }
  if (hits === names.size && total === hits) return; // the keys are the declared names: the common call
  const missing = [...names].filter((n) => !(n in values));
  if (missing.length) throw new TypeError(`missing values for ${missing.sort().join(", ")}`);
  const unknown = Object.keys(values).filter((n) => !names.has(n));
  if (unknown.length) throw new TypeError(`the prepared write has no param ${unknown.sort().join(", ")}`);
}

function slotNames(params: readonly unknown[]): Set<string> {
  return new Set(params.filter((p): p is Slot => p instanceof Slot).map((p) => p.name));
}

// -- update and delete ------------------------------------------------------------------------------

class SetWrite implements Held {
  readonly names: Set<string>;
  readonly #compiled: Compiled | null;
  readonly #empty: unknown;

  constructor(
    private readonly qs: AnyQuerySet,
    ir: IR | null,
    params: SlotParams,
    private readonly returning: boolean,
  ) {
    this.names = slotNames(params);
    this.#empty = returning ? [] : 0;
    this.#compiled = ir === null ? null : new Compiled(ir, params, true); // null: an update without values
    this.#compiled?.validate(inspector(qs.meta, qs.state.db));
  }

  async run(values: Record<string, unknown> | undefined): Promise<unknown> {
    const c = this.#compiled;
    if (c === null) {
      known(this.names, splitScope(values)[0]);
      return this.#empty;
    }
    const [params, scope] = c.callValues(values, this.names);
    const db = this.qs.db();
    const write = c.nativeWrite(db.engine, scope);
    const allowed = allowedWrites();
    const res = await db.send((tx, trace) => write.run(params, tx, allowed, trace));
    return this.returning ? new Builder(db.registry, this.qs.state.db).returned(res as NativeReturned) : res;
  }

  inspect(values: Record<string, unknown> | undefined): QueryInspection {
    const c = this.#compiled;
    if (c === null) return emptyInspection();
    const [, scope] = c.callValues(values, this.names);
    return call(() => inspection(c.nativeWrite(inspector(this.qs.meta, this.qs.state.db), scope).inspect()));
  }

  sql(values: Record<string, unknown> | undefined): string {
    const c = this.#compiled;
    if (c === null) return "";
    const [params, scope] = c.callValues(values, this.names);
    return call(() => c.nativeWrite(this.qs.meta.registry.native(), scope).sql(params));
  }
}

/** The held `update(values)` or `delete()`; `ir` is `null` for an update that sets nothing. */
export function prepareSet<R, P>(qs: AnyQuerySet, ir: IR | null, params: SlotParams, returning: boolean): PreparedWrite<R, P> {
  return new PreparedWrite(new SetWrite(qs, ir, params, returning));
}

// -- rows -------------------------------------------------------------------------------------------

/** The row of a prepared `insert(values)`: constants and the cells a call fills. */
class OneRow {
  readonly names: Set<string>;
  readonly #slots: [number, string][];

  constructor(private readonly template: readonly unknown[], refs: readonly (readonly [number, string])[]) {
    this.#slots = refs.map(([i, n]) => [i, n]);
    this.names = new Set(refs.map(([, n]) => n));
  }

  rows(values: Record<string, unknown>): unknown[][] {
    const row = [...this.template];
    for (const [i, name] of this.#slots) {
      const v = values[name];
      if (v === undefined) throw new TypeError(`param(${JSON.stringify(name)}) can't be undefined; use null for NULL`);
      row[i] = v;
    }
    return [row];
  }
}

/**
 * The rows of a prepared `insertMany()` or `updateMany()`: objects with exactly the `given` keys
 * (TypeScript names, in the statement's column order). The first `keys` fields are the primary key of an update.
 */
class ManyRows {
  readonly names: Set<string>;

  constructor(
    private readonly name: string,
    private readonly given: readonly string[],
    private readonly keys: number,
  ) {
    this.names = new Set([name]);
  }

  rows(values: Record<string, unknown>): unknown[][] {
    const rows = values[this.name];
    if (!Array.isArray(rows)) throw new TypeError(`param(${JSON.stringify(this.name)}) takes an array of rows`);
    const { given } = this;
    const n = given.length;
    const cells: unknown[][] = new Array(rows.length);
    for (let i = 0; i < rows.length; i++) {
      const row = rows[i] as Record<string, unknown> | null;
      if (row === null || typeof row !== "object" || Array.isArray(row)) throw new TypeError(`${this.name}[${i}] must be an object`);
      const cell: unknown[] = new Array(n);
      for (let j = 0; j < n; j++) {
        const v = row[given[j]!];
        if (v === undefined) this.shape(row, i);
        cell[j] = v;
      }
      if (Object.keys(row).length !== n) this.shape(row, i);
      cells[i] = cell;
    }
    if (this.keys) this.checkKeys(cells);
    return cells;
  }

  /** Throws for a row with a missing or an extra key. */
  private shape(row: Record<string, unknown>, i: number): never {
    const missing = this.given.filter((k) => row[k] === undefined);
    if (missing.length) throw new TypeError(`${this.name}[${i}] has no ${missing.join(", ")}${k_undefined(row, missing)}`);
    const extra = Object.keys(row).filter((k) => !this.given.includes(k));
    throw new TypeError(`${this.name}[${i}] has the key ${extra.join(", ")}, which the prepared write does not set`);
  }

  private checkKeys(cells: unknown[][]): void {
    const n = this.keys;
    const label = this.given.slice(0, n).join(", ");
    const seen = new Set<string>();
    cells.forEach((cell, i) => {
      const id = cell.slice(0, n);
      if (id.some((v) => v === null)) throw new TypeError(`${this.name}[${i}] has no ${label}`);
      const k = id.map((v) => (typeof v === "number" ? BigInt(v) : v)).map((v) => `${typeof v}:${String(v)}`).join("\u0000");
      if (seen.has(k)) throw new TypeError(`updateMany: ${label}=${id.map(String).join(", ")} appears twice`);
      seen.add(k);
    });
  }
}

/** Says `; use null for NULL` for a key that is present but `undefined`. */
function k_undefined(row: Record<string, unknown>, missing: readonly string[]): string {
  return missing.some((k) => k in row) ? "; use null for NULL" : "";
}

/** The held statement of a batch write for each engine and set of scope names. */
class RowsCompiled {
  readonly params: unknown[];
  readonly slots: [number, string, ((v: unknown) => unknown) | undefined][] = [];
  readonly names: Set<string>;
  readonly #declared: (string | null)[];
  readonly #natives = new WeakMap<object, Map<string, NativePreparedRows>>();

  constructor(
    readonly kind: "insert" | "update_many",
    readonly model: string,
    readonly fields: string[],
    readonly options: Record<string, unknown>,
    readonly filters: unknown[] | null,
    params: SlotParams,
    readonly returning: boolean,
    readonly batch: number | null,
  ) {
    this.params = [...params];
    params.forEach((p, i) => {
      if (p instanceof Slot) this.slots.push([i, p.name, p.transform]);
    });
    this.names = slotNames(params);
    this.#declared = params.map((p) => (p instanceof Slot ? p.type ?? null : null));
  }

  bind(values: Record<string, unknown>, scope: Readonly<Record<string, unknown>>): unknown[] {
    const params = [...this.params];
    for (const [i, name, transform] of this.slots) {
      const v = values[name];
      if (v === undefined) throw new TypeError(`param(${JSON.stringify(name)}) can't be undefined; use null for NULL`);
      params[i] = transform && v !== null ? transform(v) : v;
    }
    for (const k in scope) params.push(scope[k]);
    return params;
  }

  /** The options with the indexes of the scope values. */
  scoped(scope: readonly string[]): Record<string, unknown> {
    const options = { ...this.options };
    const index = Object.fromEntries(scope.map((n, i) => [n, this.params.length + i]));
    if (this.kind === "insert") {
      if (scope.length) options["scope"] = index;
    } else {
      options["filters"] = scope.length ? { filters: this.filters, scope: index } : this.filters;
    }
    return options;
  }

  native(engine: NativeInspector, scope: readonly string[]): NativePreparedRows {
    let byScope = this.#natives.get(engine);
    if (byScope === undefined) this.#natives.set(engine, (byScope = new Map()));
    const key = scope.join("\0");
    let held = byScope.get(key);
    if (held === undefined) {
      const declared = [...this.#declared, ...scope.map(() => null)];
      const options = JSON.stringify(this.scoped(scope));
      held = call(() =>
        this.kind === "insert"
          ? engine.prepareInsert(this.model, this.fields, options, declared.length, declared, this.returning, this.batch)
          : engine.prepareUpdateMany(this.model, this.fields, options, declared.length, declared, this.returning, this.batch),
      );
      byScope.set(key, held);
    }
    return held;
  }
}

class RowsWrite implements Held {
  readonly names: Set<string>;

  constructor(
    private readonly qs: AnyQuerySet,
    private readonly plan: OneRow | ManyRows,
    private readonly compiled: RowsCompiled,
    private readonly shape: WriteShape,
    private readonly key: readonly unknown[],
  ) {
    this.names = new Set([...plan.names, ...compiled.names]);
    try {
      compiled.native(inspector(qs.meta, qs.state.db), Object.keys(currentScope())); // a statement that cannot be held fails here
    } catch (e) {
      if (!(e instanceof QueryError) || !e.message.includes("which is not set")) throw e; // a scope not set yet waits for the first call
    }
  }

  async run(values: Record<string, unknown> | undefined): Promise<unknown> {
    const [rest, scope] = splitScope(values);
    known(this.names, rest);
    const rows = this.plan.rows(rest);
    if (!rows.length) return this.shape === "list" ? [] : 0;
    const c = this.compiled;
    const params = c.bind(rest, scope);
    const db = this.qs.db();
    const held = c.native(db.engine, Object.keys(scope));
    const allowed = allowedWrites();
    const res = await db.send((tx, trace) => held.run(rows, params, tx, allowed, trace), { key: this.key });
    if (this.shape === "count") return res;
    const list = new Builder(db.registry, this.qs.state.db).returned(res as NativeReturned);
    if (this.shape === "list") return list;
    return this.shape === "one" ? list[0]! : list[0] ?? null;
  }

  inspect(values: Record<string, unknown> | undefined): QueryInspection {
    const [rest, scope] = splitScope(values);
    known(this.names, rest);
    const rows = this.plan.rows(rest);
    if (!rows.length) return emptyInspection();
    const c = this.compiled;
    const params = c.bind(rest, scope);
    const engine = inspector(this.qs.meta, this.qs.state.db);
    const options = c.scoped(Object.keys(scope));
    const returning = this.shape !== "count";
    const text =
      c.kind === "insert"
        ? engine.inspectInsert(c.model, c.fields, rows, JSON.stringify(options), params, c.batch, returning)
        : engine.inspectUpdateMany(c.model, c.fields, rows, JSON.stringify(options["filters"]), params, c.batch, returning, JSON.stringify(options["without_defaults"] ?? false), (options["model_fields"] as string[] | undefined) ?? null);
    return call(() => inspection(text));
  }

  sql(): string {
    throw new TypeError("sql() is for a prepared update or delete; use inspect() for a batch write");
  }
}

function batchSize(size: number | null | undefined): number | null {
  if (size === undefined || size === null) return null;
  if (!(Number.isInteger(size) && size >= 1)) throw new TypeError("batchSize must be an integer of at least 1");
  return size;
}

/** The plain columns of `param(name, { fields })`, as `[TypeScript name, IR name]`. */
function rowFields(meta: ModelMeta, rows: RowsRef<string, readonly Column<any, any, any, any>[]>, what: string): FieldMeta[] {
  return rows.fields.map((c) => {
    if (!(c instanceof Column) || c.root !== meta || c.path.length) {
      throw new TypeError(`${what}: param(${JSON.stringify(rows.name)}, { fields }) takes columns of ${meta.name}, got ${String(c)}`);
    }
    const f = c.field;
    if (f.nested !== undefined) throw new TypeError(`${what}: ${meta.name}.${f.name} is a nested field; fields takes plain columns`);
    if (!(meta.inputFields.get(f.name) === f || meta.generated.has(f.name))) throw new TypeError(`${what}: ${meta.name}.${f.name} is not a field that a write sets`);
    return f;
  });
}

/** The held `insert(values)`, or an upsert of one row with `conflict`. */
export function prepareInsertOne<R, P>(qs: AnyQuerySet, values: object, conflict: Conflict, shape: WriteShape, names: readonly string[] | undefined): PreparedWrite<R, P> {
  const meta = qs.meta;
  const t = prepareInsertTemplate(meta, values);
  const refs: [number, string][] = [];
  t.cells.forEach((cell, i) => {
    if (cell !== null && typeof cell === "object" && "name" in cell && "slot" in cell) refs.push([i, (cell as { name: string }).name]);
  });
  const template = t.cells.map((cell, i) => (refs.some(([j]) => j === i) ? undefined : cell));
  return held(qs, new OneRow(template, refs), "insert", t.fields, t.provided, conflict, shape, names, null, [], null);
}

/** The held `insertMany(param("rows", { fields }))`. */
export function prepareInsertMany<R, P>(
  qs: AnyQuerySet,
  rows: RowsRef<string, readonly Column<any, any, any, any>[]>,
  conflict: Conflict,
  shape: WriteShape,
  names: readonly string[] | undefined,
  size: number | null | undefined,
): PreparedWrite<R, P> {
  const meta = qs.meta;
  const cols = rowFields(meta, rows, "insertMany");
  const has = new Set(cols.map((f) => f.ir));
  requireFields(meta, (ir) => has.has(ir));
  const fields = meta.fieldList.map((f) => f.ir).filter((ir) => has.has(ir));
  const given = fields.map((ir) => meta.fieldByIr.get(ir)!.name);
  return held(qs, new ManyRows(rows.name, given, 0), "insert", fields, new Set(fields), conflict, shape, names, batchSize(size), [], null);
}

/** The held `updateMany(param("rows", { fields }))`; the primary key is among the fields. */
export function prepareUpdateManyRows<R, P>(
  qs: AnyQuerySet,
  rows: RowsRef<string, readonly Column<any, any, any, any>[]>,
  shape: WriteShape,
  names: readonly string[] | undefined,
  size: number | null | undefined,
): PreparedWrite<R, P> {
  const meta = qs.meta;
  const cols = rowFields(meta, rows, "updateMany");
  const pk = meta.key.map((f) => f.ir);
  const has = new Set(cols.map((f) => f.ir));
  const label = meta.key.map((f) => f.name).join(", ");
  if (pk.some((f) => !has.has(f))) throw new TypeError(`updateMany: param(${JSON.stringify(rows.name)}, { fields }) needs the primary key (${label})`);
  if (has.size === pk.length) throw new TypeError(`updateMany rows need a field to set besides ${label}`);
  const fields = [...pk, ...meta.fieldList.map((f) => f.ir).filter((ir) => has.has(ir) && !pk.includes(ir))];
  const given = fields.map((ir) => meta.fieldByIr.get(ir)!.name);
  const params = new SlotParams();
  const ir = qs.mutationIr("update", params, undefined, false);
  if ("with" in ir) throw new Error("updateMany() filters can't read CTEs");
  return held(qs, new ManyRows(rows.name, given, pk.length), "update_many", fields, new Set(fields), undefined, shape, names, batchSize(size), ir["filters"] as unknown[], params);
}

function held<R, P>(
  qs: AnyQuerySet,
  plan: OneRow | ManyRows,
  kind: "insert" | "update_many",
  fields: string[],
  provided: ReadonlySet<string>,
  conflict: Conflict,
  shape: WriteShape,
  names: readonly string[] | undefined,
  size: number | null,
  filters: unknown[],
  updateParams: SlotParams | null,
): PreparedWrite<R, P> {
  const returning = shape !== "count";
  const without_defaults = qs.state.withoutDefaults;
  const model = qs.meta.name;
  let compiled: RowsCompiled;
  let key: unknown[];
  if (kind === "insert") {
    const params = new SlotParams();
    const c = qs.conflictPlan(fields, provided, conflict, params);
    const options = { conflict: c.conflict, update: c.update, set: c.set ? JSON.parse(c.set) : [], indexes: c.conflictIndexes, without_defaults, model_fields: returning ? names : undefined };
    compiled = new RowsCompiled("insert", model, fields, options, null, params, returning, size);
    key = ["insert", model, fields, c.conflict];
  } else {
    const options = { model_fields: returning ? names : undefined, without_defaults };
    compiled = new RowsCompiled("update_many", model, fields, options, filters, updateParams!, returning, size);
    key = ["update_many", model, fields];
  }
  return new PreparedWrite(new RowsWrite(qs, plan, compiled, shape, key));
}

export type { CallScope };
