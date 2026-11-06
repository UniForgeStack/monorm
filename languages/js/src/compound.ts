/**
 * `union()` and `unionAll()` of `select()` queries: one statement, rows of the chosen columns.
 *
 * ```ts
 * const rows = await Post.objects
 *   .select({ id: Post.id, name: Post.title })
 *   .union(Page.objects.select({ id: Page.id, name: Page.title }))
 *   .orderBy("-id")
 *   .slice(0, 10);
 * ```
 *
 * Each side keeps its own filters, order and slice. `orderBy()`, `limit()`, `offset()` and
 * `slice()` of the compound apply to the whole result; `orderBy()` takes output column names.
 * Both sides select the same names in the same order, with the same types (a column may be
 * nullable on one side only). `union` drops duplicate rows; `unionAll` keeps them.
 */

import { Builder } from "./build.js";
import { inspection, inspector, type QueryInspection } from "./inspection.js";
import { resolve, withScope, type Database } from "./db.js";
import { DoesNotExist, QueryError } from "./errors.js";
import type { IR } from "./expr.js";
import { call, type NativeSelect } from "./native.js";
import { NestedColumn } from "./nested.js";
import { addCut, cachedRows, type QuerySet, type RequiredQuery, type RequiredResult, type PreserveRequired } from "./query.js";
import { OneSelectRow, Select, type OnlyValue, type OneColumn } from "./select.js";
import type { ModelSpec } from "./meta.js";

type Operand = Select<any, any, any, any, any, any, any> | CompoundSelect<any>;

/** Two column values that one set-operation column can hold: one is the other, ignoring `null`. */
type Related<A, B> = [NonNullable<A>] extends [NonNullable<B>] ? true : [NonNullable<B>] extends [NonNullable<A>] ? true : false;

/** The names whose value types cannot share one column. */
type Clash<L, R> = { [K in keyof L & keyof R]: Related<L[K], R[K]> extends true ? never : K }[keyof L & keyof R];

/** `[]` when both sides select the same names with values that fit one column; otherwise an argument nobody can pass. */
export type SameColumns<L, R> = [keyof L] extends [keyof R]
  ? [keyof R] extends [keyof L]
    ? [Clash<L, R>] extends [never]
      ? []
      : [error: "both queries of a set operation must select values of the same type in each column"]
    : [error: "both queries of a set operation must select the same columns"]
  : [error: "both queries of a set operation must select the same columns"];

/** The row type of a set operation: each column may hold what either side gives. */
export type Widened<L, R> = { [K in keyof L]: L[K] | R[K & keyof R] };

/** A name `orderBy()` takes: an output column, `-name` for descending. */
export type CompoundOrder<Row> = (keyof Row & string) | `-${keyof Row & string}`;

interface State {
  readonly order: readonly string[];
  readonly limit: number | undefined;
  readonly offset: number | undefined;
  readonly cuts: readonly string[];
  readonly required: boolean;
}

function operand(item: unknown): void {
  if (item instanceof CompoundSelect) {
    if (item.state.required) throw new QueryError("a set operation can't take a require()d query; call require() on the result");
    return;
  }
  if (!(item instanceof Select)) throw new TypeError(`a set operation takes select(...) queries, got ${String(item)}`);
  const qs = item.qs;
  qs.assertStandalone();
  if (qs.state.required) throw new QueryError("a set operation can't take a require()d query; call require() on the result");
  if (qs.state.lock) throw new QueryError("lock() can't be used in a set operation");
  if (qs.state.from || qs.state.joins.length) throw new QueryError("a set operation can't use a CTE");
  for (const [, node] of item.items) {
    if (node === null) throw new QueryError("a set operation takes columns and expressions, not a model");
    if (node instanceof NestedColumn) throw new QueryError("a set operation can't return a nested column");
  }
}

function number(n: number | null, what: string): number | undefined {
  if (n === null) return undefined;
  if (!Number.isSafeInteger(n) || n < 0) throw new RangeError(`${what}() takes a non-negative integer, got ${String(n)}`);
  return n;
}

/**
 * `select(...).union(other)`: rows of two `select()` queries in one statement. Every method
 * returns a new query; nothing runs until it is awaited, and awaiting the same query again
 * gives the same rows.
 */
export class CompoundSelect<Row extends object> {
  /** @internal */
  declare readonly "~row"?: Row;

  /** @internal */
  constructor(
    private readonly operator: "union" | "union_all",
    private readonly left: Operand,
    private readonly right: Operand,
    /** @internal */
    readonly state: State = { order: [], limit: undefined, offset: undefined, cuts: [], required: false },
  ) {
    operand(left);
    operand(right);
    const a = left.names();
    const b = right.names();
    if (a.length !== b.length || a.some((name, i) => name !== b[i])) {
      throw new QueryError(
        `${operator === "union" ? "union" : "unionAll"}() needs both queries to select the same columns in the same order, got (${a.join(", ")}) and (${b.join(", ")})`,
      );
    }
    this.target(false);
  }

  private with(changes: Partial<State>): never {
    return new CompoundSelect(this.operator, this.left, this.right, { ...this.state, ...changes }) as never;
  }

  /** @internal */
  names(): string[] {
    return this.left.names();
  }

  private leaves(): Select<any, any, any, any, any, any, any>[] {
    return [this.left, this.right].flatMap((side) => (side instanceof CompoundSelect ? side.leaves() : [side]));
  }

  /** The query set that routes and runs the whole statement; every side must agree on registry and database. */
  private target(resolved: boolean): QuerySet<ModelSpec, unknown, string, unknown, string> {
    const leaves = this.leaves();
    const registry = leaves[0]!.qs.meta.registry;
    if (leaves.some((leaf) => leaf.qs.meta.registry !== registry)) {
      throw new QueryError("the queries of a set operation must use the same model registry");
    }
    const dbs = leaves.map((leaf) => (resolved ? resolve(leaf.qs.state.db) : leaf.qs.state.db)).filter((db): db is Database => db !== undefined);
    if (dbs.some((db) => db !== dbs[0])) {
      throw new QueryError("the queries of a set operation must run on the same database; call using() on each query before union()");
    }
    return (leaves.find((leaf) => leaf.qs.state.db !== undefined) ?? leaves[0]!).qs;
  }

  // -- building ---------------------------------------------------------------------------------

  /** Rows of both sides without duplicates; each side keeps its own filters, order and slice. */
  union<R2 extends object, P2 = {}, X2 extends string = never>(
    other: Select<ModelSpec, R2, string, P2, X2, unknown> | CompoundSelect<R2>,
    ...check: SameColumns<Row, R2>
  ): CompoundSelect<Widened<Row, R2>> {
    void check;
    return new CompoundSelect("union", this, other as never) as never;
  }

  /** Rows of both sides, duplicates kept. */
  unionAll<R2 extends object, P2 = {}, X2 extends string = never>(
    other: Select<ModelSpec, R2, string, P2, X2, unknown> | CompoundSelect<R2>,
    ...check: SameColumns<Row, R2>
  ): CompoundSelect<Widened<Row, R2>> {
    void check;
    return new CompoundSelect("union_all", this, other as never) as never;
  }

  /** Replace the ordering of the whole result. Names are output columns; `"-name"` is descending. */
  orderBy(...names: CompoundOrder<Row>[]): PreserveRequired<this, CompoundSelect<Row>> {
    for (const name of names) {
      if (typeof name !== "string" || !name.replace(/^-/, "")) {
        throw new TypeError(`a set operation orders by output column names such as 'id' or '-id', got ${String(name)}`);
      }
    }
    return this.with({ order: names });
  }

  private cut(kind: string): readonly string[] {
    return addCut(this.state.cuts, kind, "query");
  }

  /** At most `n` rows of the whole result. */
  limit(n: number | null): PreserveRequired<this, CompoundSelect<Row>> {
    const limit = number(n, "limit");
    return this.with({ limit, cuts: this.cut("limit") });
  }

  /** Skip `n` rows of the whole result. */
  offset(n: number | null): PreserveRequired<this, CompoundSelect<Row>> {
    const offset = number(n, "offset") || undefined;
    return this.with({ offset, cuts: this.cut("offset") });
  }

  /** Rows `start` to `end` (exclusive) of the whole result, like `Array.slice`. */
  slice(start: number, end?: number): PreserveRequired<this, CompoundSelect<Row>> {
    const cuts = this.cut("slice");
    if (!Number.isInteger(start) || start < 0 || (end !== undefined && (!Number.isInteger(end) || end < 0))) {
      throw new RangeError("slice() takes non-negative integers");
    }
    return this.with({ offset: start || undefined, limit: end === undefined ? undefined : Math.max(end - start, 0), cuts });
  }

  /** A new query with an empty result cache: `await q.all()` queries afresh. */
  all(): this {
    return this.with({});
  }

  /** Reject a result without rows. It applies to the final result only. */
  require(): this & RequiredQuery {
    return this.with({ required: true });
  }

  // -- IR ---------------------------------------------------------------------------------------

  /** @internal */
  node(params: unknown[]): IR {
    const side = (s: Operand): IR => (s instanceof CompoundSelect ? s.node(params) : s.leafIr(params));
    const node: IR = { t: "set", operator: this.operator, left: side(this.left), right: side(this.right) };
    if (this.state.order.length) node["order"] = [...this.state.order];
    for (const [key, value] of [["limit", this.state.limit], ["offset", this.state.offset]] as const) {
      if (value !== undefined) {
        params.push(value);
        node[key] = { t: "param", i: params.length - 1 };
      }
    }
    return node;
  }

  private ir(params: unknown[], terminal = "read"): IR {
    return { op: "compound", terminal, expr: this.node(params) };
  }

  // -- inspection -------------------------------------------------------------------------------

  /** The statement this runs, parameters inlined (for debugging). */
  sql(): string {
    const params: unknown[] = [];
    const ir = this.ir(params);
    return call(() => this.target(false).meta.registry.native().sql(...withScope(JSON.stringify(ir), params)));
  }

  /** Inspect the generated statement structure without running it. */
  inspect(): QueryInspection {
    const params: unknown[] = [];
    const ir = this.ir(params);
    const qs = this.target(false);
    return call(() => inspection(inspector(qs.meta, qs.state.db).inspect(...withScope(JSON.stringify(ir), params))));
  }

  /** The database's plan for this statement; see {@link QuerySet.explain}. */
  async explain(options: { readonly analyze?: boolean } = {}): Promise<string> {
    const bound: unknown[] = [];
    return this.target(true).explainIr(this.ir(bound), bound, options.analyze ?? false);
  }

  // -- execution --------------------------------------------------------------------------------

  /** @internal */
  async read(): Promise<Row[]> {
    const qs = this.target(true);
    const params: unknown[] = [];
    const res = (await qs.db().run(this.ir(params), params)) as NativeSelect;
    const rows = new Builder(qs.db().registry, qs.state.db).select(res, this.names(), []) as Row[];
    if (this.state.required && !rows.length) throw new DoesNotExist("the query returned no row");
    return rows;
  }

  /** `await q`: the rows, queried on the first `await` and reused by later ones. */
  then<A = Row[], B = never>(
    onfulfilled?: ((rows: Row[]) => A | PromiseLike<A>) | null,
    onrejected?: ((reason: unknown) => B | PromiseLike<B>) | null,
  ): Promise<A | B> {
    return cachedRows(this, () => this.read()).then(onfulfilled, onrejected);
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<Row, void, undefined> {
    yield* await cachedRows(this, () => this.read());
  }

  private noCuts(what: string): void {
    if (this.state.cuts.length) throw new QueryError(`${what} can't be used on a query with a limit, offset or slice`);
  }

  /** The first row (by `orderBy`; unordered otherwise), or `null`. Lazy: nothing runs until it is awaited. */
  first(): OneSelectRow<RequiredResult<this, Row>> {
    this.noCuts("first()");
    return new OneSelectRow(this.slice(0, 1) as never) as never;
  }

  /** The single row or `null`; more than one row throws. */
  get(): OneSelectRow<RequiredResult<this, Row>> {
    this.noCuts("get()");
    return new OneSelectRow(this.limit(2) as never, true) as never;
  }

  /** The single column of the first row, or `null` without rows. */
  async scalar(...check: OneColumn<Row>): Promise<OnlyValue<Row> | null> {
    void check;
    this.oneColumn("scalar");
    const rows = await (this.slice(0, 1) as CompoundSelect<Row>).read();
    return rows.length ? ((rows[0] as Record<string, unknown>)[this.names()[0]!] as OnlyValue<Row>) : null;
  }

  /** The single column of every row. */
  async scalars(...check: OneColumn<Row>): Promise<OnlyValue<Row>[]> {
    void check;
    this.oneColumn("scalars");
    const key = this.names()[0]!;
    return (await this.read()).map((r) => (r as Record<string, unknown>)[key] as OnlyValue<Row>);
  }

  private oneColumn(what: string): void {
    if (this.names().length !== 1) throw new QueryError(`${what}() needs a query selecting one column, this one selects ${this.names().length}`);
  }

  /** The number of rows of the result: `SELECT count(*) FROM (<union>)`. */
  async count(): Promise<number> {
    const params: unknown[] = [];
    return (await this.target(true).db().run(this.ir(params, "count"), params)) as number;
  }

  /** Whether the result has a row. */
  async exists(): Promise<boolean> {
    const params: unknown[] = [];
    return (await this.target(true).db().run(this.ir(params, "exists"), params)) as boolean;
  }

  toString(): string {
    return `CompoundSelect(${this.operator})`;
  }
}
