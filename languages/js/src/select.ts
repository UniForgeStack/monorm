/**
 * `select()`: queries giving chosen columns and aggregates instead of instances.
 *
 * ```ts
 * const rows = await Post.objects
 *   .filter(Post.published)
 *   .select({ authorId: Post.authorId, posts: func.count(), views: func.sum(Post.views) })
 *   .groupBy(Post.authorId)
 *   .having(func.count().gt(2))
 *   .all();                                   // { authorId: bigint; posts: bigint; views: bigint | null }[]
 *
 * await Post.objects.select({ top: func.max(Post.views) }).scalar();   // number | null
 * await Post.objects.select({ id: Post.id }).scalars();                // bigint[]
 * await Post.objects.select({ post: Post, comments: func.count(Post.comments) }).all();
 * ```
 */

import { inspection, inspector, type QueryInspection } from "./inspection.js";
import { Builder } from "./build.js";
import { NestedColumn } from "./nested.js";
import { withScope, type Database } from "./db.js";
import { DoesNotExist, MultipleObjectsReturned, QueryError, TransactionRequired } from "./errors.js";
import {
  AGGREGATES,
  Column,
  Condition,
  Expression,
  Func,
  IRContext,
  Labeled,
  Ordering,
  ParamRef,
  ScalarSubquery,
  SlotParams,
  and,
  orderings,
  resolveNullRule,
  type Ctes,
  type IR,
  type Node,
  type ParamValues,
  type ResolveOuter,
} from "./expr.js";
import type { ModelSpec } from "./meta.js";
import { fieldValue, modelMeta, type ModelClass, type ModelMeta } from "./model.js";
import { call, type NativeSelect } from "./native.js";
import { internalLoop } from "./debug.js";
import { after, checkPaging, decodeCursor, encodeValues, fingerprint, keyset, type Key, type Page, type PageOptions } from "./pagination.js";
import {
  Compiled,
  OneRowQuery,
  PreparedOneRowQuery,
  QuerySet,
  checkRunnable,
  registerSelect,
  type Allowed,
  type AllowedOne,
  type CteOptions,
  type FieldOrder,
  type OuterRefs,
  type ParamsOfAll,
  type Awaitable,
  type PreparedRows,
  type Runnable,
  type Values,
  type RequiredQuery,
  type RequiredResult,
  type PreserveRequired,
  cachedRows,
  type ScopesOf,
  type UnionToIntersection,
  type Via,
} from "./query.js";
import { makeCte, type Cte } from "./cte.js";

/** What `select()` takes: expressions over the query's sources, or the model itself. */
export type SelectItems<M extends ModelSpec, S extends string> = {
  readonly [key: string]: Expression<unknown, AllowedOne<S>, unknown> | ModelClass<M>;
};

/** The row an item set gives. */
export type SelectRow<I> = {
  -readonly [K in keyof I]: I[K] extends Expression<infer T, string, unknown> ? T : I[K] extends ModelClass<infer M> ? M["row"] : never;
};

export type ItemsParams<I> = UnionToIntersection<
  { [K in keyof I]: I[K] extends Expression<unknown, string, infer P> ? P : {} }[keyof I]
>;

export type ItemsOuter<I, Own extends string> = {
  [K in keyof I]: I[K] extends Expression<unknown, infer S, unknown> ? OuterRefs<S, Own> : never;
}[keyof I];

/** The columns of a CTE made from a `select()`: a selected model gives its fields. */
export type CteColumnsOf<I> = UnionToIntersection<
  {
    [K in keyof I]: I[K] extends Expression<infer T, string, unknown>
      ? { readonly [P in K]: T }
      : I[K] extends ModelClass<infer M>
        ? M["data"]
        : never;
  }[keyof I]
>;

/** The value of the only column of row type `R` (`never` if it has several). */
export type OnlyValue<R> = OnlyKey<R, keyof R>;
type OnlyKey<R, K extends keyof R> = K extends unknown ? ([Exclude<keyof R, K>] extends [never] ? R[K] : never) : never;

type OneColumn<R> = [OnlyValue<R>] extends [never] ? [error: "this needs a select() of exactly one column"] : [];

/**
 * A query giving rows of chosen columns. Built by `QuerySet.select()`; every method
 * returns a new `Select`, and nothing runs until a terminal method is called.
 *
 * `C` is the columns a CTE made from it has.
 */
export class Select<M extends ModelSpec, Row extends object, S extends string, P, X extends string, C = Row, V = null> {
  /** @internal */
  declare readonly "~exists"?: [X, P];
  /** @internal The rows, parameters and relation of a labeled select in `load()`. */
  declare readonly "~rel"?: [Row, P, V];
  /** @internal */
  declare readonly "~column"?: [OnlyValue<Row>] extends [never] ? "not one column" : [OnlyValue<Row>, ResolveOuter<X>, P];

  /** @internal */
  constructor(
    readonly qs: QuerySet<M, unknown, string, unknown, string>,
    readonly items: readonly [string, Node | null][],
    private readonly state: {
      readonly group: readonly Node[];
      readonly having: readonly Node[];
      readonly distinct: boolean;
      readonly distinctOn: readonly Column<unknown, string>[];
      readonly label?: string;
    } = { group: [], having: [], distinct: false, distinctOn: [] },
  ) {}

  private with(qs: QuerySet<M, unknown, string, unknown, string>, state = this.state): never {
    return new Select(qs, this.items, state) as never;
  }

  /** Like `with()`, typed as this select so that internal reads can use it. */
  private derive(qs: QuerySet<M, unknown, string, unknown, string>): Select<M, Row, S, P, X, C, V> {
    return new Select(qs, this.items, this.state) as never;
  }

  // -- building (WHERE / ORDER BY / LIMIT go to the query set) --------------------------------

  /** Reject queries returning no rows, while allowing NULL column values. */
  require(): this & RequiredQuery {
    return this.with(this.qs.require()) as this & RequiredQuery;
  }

  filter<const Cs extends readonly Expression<boolean | null, Allowed<S>, unknown>[]>(
    ...conditions: Cs
  ): PreserveRequired<this, Select<M, Row, S, P & ParamsOfAll<Cs>, X | OuterRefs<ScopesOf<Cs>, S>, C, V>> {
    return this.with(this.qs.filter(...(conditions as unknown as never[])) as never);
  }

  exclude<const Cs extends readonly Expression<boolean | null, Allowed<S>, unknown>[]>(
    ...conditions: Cs
  ): PreserveRequired<this, Select<M, Row, S, P & ParamsOfAll<Cs>, X | OuterRefs<ScopesOf<Cs>, S>, C, V>> {
    return this.with(this.qs.exclude(...(conditions as unknown as never[])) as never);
  }

  orderBy<const Cs extends readonly (Expression<unknown, AllowedOne<S>, unknown> | Ordering<AllowedOne<S>, unknown> | FieldOrder<M>)[]>(
    ...items: Cs
  ): PreserveRequired<this, Select<M, Row, S, P & ParamsOfAll<Cs>, X | OuterRefs<ScopesOf<Cs>, S>, C, V>> {
    return this.with(this.qs.orderBy(...(items as unknown as never[])) as never);
  }

  limit<N extends string>(n: ParamRef<N>): PreserveRequired<this, Select<M, Row, S, P & ParamValues<N, number | bigint>, X, C, V>>;
  limit(n: number | null): PreserveRequired<this, Select<M, Row, S, P, X, C, V>>;
  limit(n: number | null | ParamRef<string>): unknown {
    return this.with(this.qs.limit(n as never) as never);
  }

  offset<N extends string>(n: ParamRef<N>): PreserveRequired<this, Select<M, Row, S, P & ParamValues<N, number | bigint>, X, C, V>>;
  offset(n: number | null): PreserveRequired<this, Select<M, Row, S, P, X, C, V>>;
  offset(n: number | null | ParamRef<string>): unknown {
    return this.with(this.qs.offset(n as never) as never);
  }

  /** Rows `start` to `end` (exclusive), like `Array.slice`. */
  slice(start: number, end?: number): PreserveRequired<this, Select<M, Row, S, P, X, C, V>> {
    return this.with(this.qs.slice(start, end) as never);
  }

  using(db: Database | undefined): PreserveRequired<this, Select<M, Row, S, P, X, C, V>> {
    return this.with(this.qs.using(db) as never);
  }

  /** `GROUP BY`: aggregates in the select list then summarize each group. The model
   * itself (`groupBy(Post)`) groups by its primary key. */
  groupBy<const Cs extends readonly (Expression<unknown, AllowedOne<S>, unknown> | ModelClass<M>)[]>(
    ...exprs: Cs
  ): PreserveRequired<this, Select<M, Row, S, P & ParamsOfAll<Cs>, X | OuterRefs<ScopesOf<Cs>, S>, C, V>> {
    const out = exprs.flatMap((e): Node[] => {
      const meta = modelMeta(e);
      if (meta) {
        if (meta !== this.qs.meta) {
          throw new TypeError(`groupBy() takes ${this.qs.meta.name} itself or expressions`);
        }
        return meta.key.map((f) => meta.column(f) as Node);
      }
      if (!(e instanceof Expression)) {
        throw new TypeError(`groupBy() takes columns and expressions, got ${String(e)}`);
      }
      return [e];
    });
    return this.with(this.qs, { ...this.state, group: [...this.state.group, ...out] });
  }

  /** Keep groups matching all `conditions`: `having(func.count().gt(2))`. */
  having<const Cs extends readonly Expression<boolean | null, Allowed<S>, unknown>[]>(
    ...conditions: Cs
  ): PreserveRequired<this, Select<M, Row, S, P & ParamsOfAll<Cs>, X | OuterRefs<ScopesOf<Cs>, S>, C, V>> {
    if (!conditions.length) {
      return this as never;
    }
    const condition = resolveNullRule(and(...conditions));
    return this.with(this.qs, { ...this.state, having: [...this.state.having, condition] });
  }

  /** `SELECT DISTINCT`; with columns, Postgres' `DISTINCT ON (...)`: the first row (by
   * `orderBy`) of each distinct value. */
  distinct(...on: readonly Column<unknown, S>[]): PreserveRequired<this, Select<M, Row, S, P, X, C, V>> {
    return this.with(this.qs, { ...this.state, distinct: true, distinctOn: on });
  }

  /**
   * `JOIN <cte> ON <on>` (`LEFT JOIN` with `outer`), as {@link QuerySet.join}: the CTE's
   * columns (`cte.c.<name>`) work in `groupBy()`, `having()` and `orderBy()`. To select
   * them, join before `select()`.
   */
  join<N extends string, Cc, const On extends Expression<boolean | null, Allowed<S | N>, unknown>>(
    cte: Cte<N, Cc, ModelSpec | null>,
    on: On,
    options: { readonly outer?: boolean } = {},
  ): PreserveRequired<this, Select<M, Row, S | N, P & ParamsOfAll<[On]>, X | OuterRefs<ScopesOf<[On]>, S | N>, C, V>> {
    return this.with(this.qs.join(cte as never, on as never, options) as never);
  }

  /** Reads the rows from `cte` instead of the model's table, as {@link QuerySet.from}. */
  from<N extends string>(cte: Cte<N, unknown, M>): PreserveRequired<this, Select<M, Row, S | N, P, X, C, V>> {
    return this.with(this.qs.from(cte as never) as never);
  }

  /**
   * Stores the rows in the plain attribute `name` of each parent. A select of a relation
   * query set (`User.posts.objects.select(...)`) needs it to go to `load()`.
   */
  label<const A extends string>(
    name: A,
  ): PreserveRequired<this, Select<M, Row, S, P, X, C, V extends Via<infer H, string | undefined, infer O, infer S0> ? Via<H, A, O, S0> : never>> {
    if (!this.qs.state.via) {
      throw new TypeError(`label() applies to a select of a relation query set such as User.posts.objects.select(...), not ${this.qs.meta.name}.objects`);
    }
    if (typeof name !== "string" || !/^[A-Za-z$][\w$]*$/.test(name)) {
      throw new TypeError(`label ${JSON.stringify(name)} must be an identifier not starting with '_'`);
    }
    return this.with(this.qs, { ...this.state, label: name });
  }

  /** @internal The relation query set that `load()` reads this labeled select through. */
  relationSet(): QuerySet<ModelSpec, unknown, string, unknown, never> {
    const label = this.state.label;
    if (!this.qs.state.via) {
      throw new TypeError(`load() takes a select of a relation query set such as ${this.qs.meta.name}.<relation>.objects.select(...), not ${this.qs.meta.name}.objects`);
    }
    if (label === undefined) throw new TypeError("a select() in load() needs .label(name)");
    if (this.items.some(([, node]) => node instanceof NestedColumn)) {
      throw new TypeError("a select() in load() can't select a nested model");
    }
    return this.qs.withRows(this as never, label) as never;
  }

  /** @internal The `IN (...)` query of a relation query set: no standalone check, no prefetch. */
  relationIr(params: unknown[]): [IR, IRContext] {
    return this.build(params, undefined, undefined, "select", true);
  }

  // -- IR -------------------------------------------------------------------------------------

  private build(params: unknown[], outer?: IRContext, ctes?: Ctes, op = "select", relation = false): [IR, IRContext] {
    if (!relation) this.qs.assertStandalone();
    const [ir, ctx] = this.qs.queryIr(op, params, outer, ctes, !relation);
    delete ir["annotations"];
    ir["columns"] = this.items.map(([name, node]) => (node === null ? { t: "model" } : { t: "expr", expr: node.ir(ctx), name }));
    if (this.state.group.length) {
      ir["group_by"] = this.state.group.map((g) => g.ir(ctx));
    }
    if (this.state.having.length) {
      ir["having"] = this.state.having.map((h) => h.ir(ctx));
    }
    if (this.state.distinctOn.length) {
      ir["distinct_on"] = this.state.distinctOn.map((c) => c.ir(ctx));
    } else if (this.state.distinct) {
      ir["distinct"] = true;
    }
    ctx.addWindows(ir);
    return [ir, ctx];
  }

  /** @internal */
  ir(params: unknown[], op = "select"): IR {
    const [ir, ctx] = this.build(params, undefined, undefined, op);
    return ctx.finish(ir);
  }

  /** @internal */
  subqueryIr(ctx: IRContext, what: string): IR {
    if (what !== "exists()" && (this.items.length !== 1 || this.items[0]![1] === null)) {
      throw new QueryError(`${what} takes a query that selects exactly one column`);
    }
    if (this.qs.state.lock) {
      throw new QueryError("a subquery can't lock rows");
    }
    return this.build(ctx.params, ctx)[0];
  }

  /** @internal */
  cteIr(params: unknown[], ctes: Ctes): IR {
    return this.build(params, undefined, ctes)[0];
  }

  /**
   * This one-column query as a value in another query: `(SELECT ...)`. It must give at
   * most one row (slice it with `.limit(1)`); no row is `NULL`. Correlate it with
   * `outer()`.
   */
  asScalar(...check: OneColumn<Row>): ScalarSubquery<OnlyValue<Row> | null, ResolveOuter<X>, P> {
    void check;
    this.oneColumn("asScalar");
    return new ScalarSubquery(this);
  }

  /** This query as a CTE (`WITH <name> AS (...)`) whose columns are the selected items
   * (`cte.c.<key>`; a selected model gives its fields). */
  cte<const N extends string>(
    name: N,
    options: CteOptions<N, C, P, X> = {},
  ): Cte<N, C, [Extract<Row[keyof Row], M["row"]>] extends [never] ? null : M> {
    return makeCte(name, this as never, options as never) as never;
  }

  /** The SELECT this runs, parameters inlined (for debugging). */
  sql(...check: Runnable<P, X>): string {
    void check;
    const params: unknown[] = [];
    const ir = this.ir(params);
    return call(() => this.qs.meta.registry.native().sql(...withScope(JSON.stringify(ir), params)));
  }

  /** Inspect the selected query and its prefetch templates without execution. */
  inspect(...check: Runnable<P, X>): QueryInspection {
    void check;
    const params: unknown[] = [];
    const ir = this.ir(params);
    return call(() => inspection(inspector(this.qs.meta, this.qs.state.db).inspect(...withScope(JSON.stringify(ir), params))));
  }

  /** The database's plan for this query's SELECT; see {@link QuerySet.explain}. */
  async explain(options: { readonly analyze?: boolean } = {}): Promise<string> {
    const bound: unknown[] = [];
    return this.qs.explainIr(this.ir(bound), bound, options.analyze ?? false);
  }

  /**
   * This query compiled once, to run many times with `param()` values:
   *
   * ```ts
   * const byAuthor = Post.objects.filter(Post.authorId.eq(param("author"))).select({ id: Post.id }).prepare();
   * const rows = await byAuthor.all({ author: 3n });
   * ```
   */
  prepare(...check: [X] extends [never] ? [] : [error: "a prepared query can't read outer() columns"]): PreserveRequired<this, PreparedSelect<Row, P>> {
    void check;
    this.qs.assertStandalone();
    return new PreparedSelect(this as never) as never;
  }

  // -- execution ------------------------------------------------------------------------------

  /** @internal The decoded rows of a result. */
  decode(res: NativeSelect): Row[] {
    const decoders = this.items.map(([, node]) => (node instanceof NestedColumn ? (raw: unknown) => node.decode(raw) : undefined));
    return new Builder(this.qs.db().registry, this.qs.state.db).select(res, this.items.map(([k]) => k), decoders) as Row[];
  }

  /** @internal The rows of a read: non-empty when the select is required. */
  finish(rows: Row[]): Row[] {
    if (this.qs.state.required && !rows.length) throw new DoesNotExist("the query returned no row");
    return rows;
  }

  private async fetch(): Promise<Row[]> {
    const params: unknown[] = [];
    const ir = this.ir(params);
    const db = this.qs.db();
    if (this.qs.state.lock && db.tx() === null) {
      throw new TransactionRequired(
        "lock() outside a transaction would release the locks as soon as the query ends; run it inside `db.transaction(...)`",
      );
    }
    return this.decode((await db.run(ir, params)) as NativeSelect);
  }

  /** @internal */
  async read(): Promise<Row[]> {
    return this.finish(await this.fetch());
  }

  /** The rows, queried afresh (unlike `await sel`, which reuses its first result). */
  all(...check: Runnable<P, X>): Promise<Row[]> {
    void check;
    return this.read();
  }

  /** `await sel`: the rows, queried on the first `await` and reused by later ones. */
  then<A = Row[], B = never>(
    this: Select<M, Row, S, P, X, C> & Awaitable<P, X>,
    onfulfilled?: ((rows: Row[]) => A | PromiseLike<A>) | null,
    onrejected?: ((reason: unknown) => B | PromiseLike<B>) | null,
  ): Promise<A | B> {
    return cachedRows(this, () => this.read()).then(onfulfilled, onrejected);
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<Row, void, undefined> {
    yield* await cachedRows(this, () => this.read());
  }

  private one<T, Q>(sel: Select<M, Row, S, P, X, C>, unique = false): OneSelectRow<T, Q & P, X> {
    return new OneSelectRow(sel as never, unique) as never;
  }

  /** The first row (by `orderBy`; unordered otherwise), or `null`. Lazy: no query runs
   * until the result is awaited. */
  first(): OneSelectRow<RequiredResult<this, Row>, P, X> {
    this.qs["noCuts"]("first()");
    return this.one(this.with(this.qs.slice(0, 1) as never));
  }

  /** The order that decides which row is first or last: `orderBy()`, else (without
   * groups or distinct) the model's default order. */
  private edgeOrder(what: string): readonly Ordering<string, unknown>[] {
    if (this.qs.state.order.length) return this.qs.state.order;
    if (this.state.group.length || this.state.distinct || this.aggregates()) {
      throw new TypeError(`${what} needs orderBy() on a grouped, aggregate or distinct select`);
    }
    return this.qs.defaultOrder();
  }

  /** Whether an item of the select list is an aggregate call: without `groupBy()` it folds all rows. */
  private aggregates(): boolean {
    return this.items.some(([, node]) => {
      const expr = node instanceof Labeled ? node.expr : node;
      return expr instanceof Func && expr.rel === undefined && AGGREGATES.has(expr.name);
    });
  }

  /** The last row by the reversed `orderBy()`, or `null`. A grouped, aggregate or distinct select
   * needs `orderBy()`. */
  last(): OneSelectRow<RequiredResult<this, Row>, P, X> {
    this.qs["noCuts"]("last()");
    const edge = this.edgeOrder("last()");
    if (this.state.distinctOn.length && edge.length > this.state.distinctOn.length) {
      throw new TypeError("last() can't reverse an order that goes past the distinct(on) columns: it would change the kept row");
    }
    const order = edge.map((o) => o.reversed());
    return this.one(this.with(this.qs["firstRows"](0, order) as never));
  }

  /**
   * The row at position `n` of the order, or `null`; `n` may be a `param()` in a
   * prepared query. `at(-1)` is `last()`. A grouped, aggregate or distinct select needs `orderBy()`.
   */
  at<N extends string>(n: ParamRef<N>): OneSelectRow<RequiredResult<this, Row>, P & ParamValues<N, number | bigint>, X>;
  at(n: number): OneSelectRow<RequiredResult<this, Row>, P, X>;
  at(n: number | ParamRef<string>): unknown {
    if (!(n instanceof ParamRef)) {
      if (!Number.isInteger(n) || n < -1) throw new RangeError("at() takes a non-negative integer, or -1");
      if (n === -1) return this.last();
    }
    this.qs["noCuts"]("at()");
    return this.one(this.with(this.qs["firstRows"](n, this.edgeOrder("at()")) as never));
  }

  /** The single matching row or null; multiple matches always throw.
   * `require().get()` also throws `DoesNotExist` when absent. */
  get<const Cs extends readonly Expression<boolean | null, Allowed<S>, unknown>[]>(
    ...conditions: Cs
  ): OneSelectRow<RequiredResult<this, Row>, P & ParamsOfAll<Cs>, X | OuterRefs<ScopesOf<Cs>, S>> {
    this.qs["noCuts"]("get()");
    const filtered = this.with(this.qs.filter(...(conditions as unknown as never[])).limit(2) as never);
    return this.one(filtered, true);
  }

  /** The single column of the first row, or `null` without rows. */
  async scalar(...check: [...OneColumn<Row>, ...Runnable<P, X>]): Promise<OnlyValue<Row> | null> {
    void check;
    this.oneColumn("scalar");
    const rows = await this.derive(this.qs.slice(0, 1) as never).read();
    return rows.length ? ((rows[0] as Record<string, unknown>)[this.items[0]![0]] as OnlyValue<Row>) : null;
  }

  /** The single column of every row. */
  async scalars(...check: [...OneColumn<Row>, ...Runnable<P, X>]): Promise<OnlyValue<Row>[]> {
    void check;
    this.oneColumn("scalars");
    const key = this.items[0]![0];
    return (await this.read()).map((r) => (r as Record<string, unknown>)[key] as OnlyValue<Row>);
  }

  /** The number of rows this select returns: `SELECT count(*) FROM (<select>)`, so
   * groups, `having()`, `distinct()` and slices decide the rows counted. */
  async count(...check: Runnable<P, X>): Promise<number> {
    void check;
    const params: unknown[] = [];
    return (await this.qs.db().run(this.ir(params, "count"), params)) as number;
  }

  /** Whether the select returns a row: `SELECT EXISTS (<select>)`. */
  async exists(...check: Runnable<P, X>): Promise<boolean> {
    void check;
    const params: unknown[] = [];
    return (await this.qs.db().run(this.ir(params, "exists"), params)) as boolean;
  }

  // -- paging ---------------------------------------------------------------------------------

  /** The keyset columns of a page and the full order of its read. The order is unique by
   * its group keys or `distinct(on)` columns, else by the primary key. */
  private pageKeys(): [Key[], Ordering<string, unknown>[]] {
    const { meta, state: qsState } = this.qs;
    const own = (c: unknown): c is Column<unknown, string> => c instanceof Column && !c.path.length && c.root === meta;
    if (this.state.distinct && !this.state.distinctOn.length) {
      throw new QueryError("paginate() can't be used with distinct(); use distinct(on) with the columns that make a row unique");
    }
    if (this.state.distinctOn.length) {
      const on = this.state.distinctOn;
      const order = qsState.order.length ? qsState.order : on.map((c) => c.asc());
      const lead = order.slice(0, on.length);
      if (lead.length < on.length || lead.some((o, i) => !own(o.expr) || o.expr.field !== on[i]!.field)) {
        throw new QueryError("paginate() needs orderBy() to start with the distinct(on) columns");
      }
      return [keyset(meta, lead, false), [...order]];
    }
    if (this.state.group.length) {
      for (const g of this.state.group) {
        if (!own(g)) throw new QueryError(`paginate() groups by columns of ${meta.name} itself, not by ${String(g)}`);
      }
      const named = new Set(qsState.order.flatMap((o) => (own(o.expr) ? [o.expr.field] : [])));
      const extra = (this.state.group as readonly Column<unknown, string>[]).filter((g) => !named.has(g.field)).map((g) => g.asc());
      const keys = keyset(meta, [...qsState.order, ...extra], false);
      return [keys, keys.map(([, o]) => o)];
    }
    const keys = keyset(meta, this.qs.defaultOrder());
    return [keys, keys.map(([, o]) => o)];
  }

  /** For each key column, how to read its value from a row; the column must be in the select list. */
  private keyGetters(keys: readonly Key[]): ((row: Record<string, unknown>) => unknown)[] {
    const meta: ModelMeta = this.qs.meta;
    return keys.map(([f, o]) => {
      for (const [name, node] of this.items) {
        if (node === null) return (row) => fieldValue(row[name] as object, f.name);
        if (node instanceof Column && !node.path.length && node.root === meta && node.field === f) return (row) => row[name];
      }
      throw new QueryError(`paginate() reads the cursor from the rows: select ${String(o.expr)} too`);
    });
  }

  /**
   * A page of rows by keyset, as {@link QuerySet.paginate}. The order must be unique:
   * `orderBy()` plus the `groupBy()` columns (added when missing) or the `distinct(on)`
   * columns, else the primary key. Every order column is a column of the model itself and
   * is in the select list; `distinct()` without columns and aggregate order terms are
   * rejected. `{ last }` can't read back over `distinct(on)` with order terms after the
   * `on` columns.
   */
  paginate(options: PageOptions, ...check: Runnable<P, X>): Promise<Page<Row>> {
    void check;
    return this.pageOf(options);
  }

  private async pageOf(options: PageOptions): Promise<Page<Row>> {
    const { size, forward } = checkPaging(options);
    const { meta, state } = this.qs;
    if (state.limit !== undefined || state.offset !== undefined) {
      throw new QueryError("paginate() can't be used on a sliced select");
    }
    const [keys, order] = this.pageKeys();
    if (!forward && order.length > keys.length) {
      throw new QueryError("paginate({ last }) over distinct(on) needs an order of the distinct(on) columns only");
    }
    const getters = this.keyGetters(keys);
    const fp = fingerprint(meta, keys);
    const pageOrder = order.map((o) => (forward ? o : o.reversed()));
    const cursor = (forward ? options.after : options.before) ?? null;
    let qs = this.qs;
    if (cursor !== null) {
      qs = qs.filter(after(meta, pageOrder.slice(0, keys.length), decodeCursor(meta, cursor, fp, keys)) as never) as never;
    }
    const fetched = await this.derive(qs.orderBy(...(pageOrder as never[])).limit(size + 1) as never).fetch();
    const more = fetched.length > size;
    const items = forward ? fetched.slice(0, size) : fetched.slice(0, size).reverse();
    const cursorOf = (row: Row) => encodeValues(fp, keys, getters.map((get) => get(row as Record<string, unknown>)));
    return {
      items,
      hasNext: forward ? more : cursor !== null,
      hasPrevious: forward ? cursor !== null : more,
      nextCursor: items.length ? cursorOf(items[items.length - 1]!) : null,
      previousCursor: items.length ? cursorOf(items[0]!) : null,
    };
  }

  /**
   * The rows in arrays of `size`, read page by page with {@link paginate} (same order
   * rules), so memory stays flat.
   */
  async *batches(size = 1000, ...check: Runnable<P, X>): AsyncGenerator<Row[], void, undefined> {
    void check;
    if (!Number.isInteger(size) || size < 1) {
      throw new RangeError("batch size must be at least 1");
    }
    let cursor: string | null = null;
    const seen = new Set<string>();
    for (;;) {
      const page: Page<Row> = await internalLoop(seen, () => this.pageOf({ first: size, after: cursor }));
      if (page.items.length) {
        yield page.items;
      }
      if (!page.hasNext) {
        return;
      }
      cursor = page.nextCursor;
    }
  }

  /** Every row, fetched `batchSize` at a time (see {@link batches}). */
  async *iterate(batchSize = 1000, ...check: Runnable<P, X>): AsyncGenerator<Row, void, undefined> {
    void check;
    for await (const batch of this.batches(batchSize, ...(check as Runnable<P, X>))) {
      yield* batch;
    }
  }

  /** @internal */
  oneColumn(what: string): void {
    if (this.items.length !== 1) {
      throw new QueryError(`${what}() needs a query selecting one column, this one selects ${this.items.length}`);
    }
  }

  toString(): string {
    return `Select(${this.qs.meta.name}: ${this.items.map(([k]) => k).join(", ")})`;
  }
}

/**
 * `first()`, `last()`, `at(n)` and `get()` of a select: one row, or `null`. Like
 * {@link OneRowQuery}, it runs once, on the first `then`.
 */
export class OneSelectRow<T, P = {}, X extends string = never> extends OneRowQuery<T, P, X> {
  readonly #select: Select<ModelSpec, object, string, {}, never, unknown>;
  readonly #pick: (rows: unknown[]) => unknown;

  /** @internal */
  constructor(select: Select<ModelSpec, object, string, {}, never, unknown>, unique = false) {
    const pick = (rows: unknown[]) => {
      if (unique && rows.length > 1) throw new MultipleObjectsReturned("the query returned more than one row");
      return rows[0] ?? null;
    };
    const rows = { fetch: () => select.read(), checkRows: (r: unknown[]) => r, sql: () => select.sql(), inspect: () => select.inspect() };
    super(() => rows, pick, unique);
    this.#select = select;
    this.#pick = pick;
  }

  /** A select's row can't be loaded per parent. */
  override label(): never {
    throw new TypeError("the row of a select can't be loaded per parent");
  }

  override prepare(...check: [X] extends [never] ? [] : [error: "a prepared query can't read outer() columns"]): PreparedOneRowQuery<T, P> {
    void check;
    return new PreparedOneRowQuery(new PreparedSelect(this.#select as never) as PreparedRows<P>, this.#pick);
  }
}

/**
 * A select compiled by {@link Select.prepare}. Each call binds the `param()` values:
 * `await q.all({ author: 3n })`, or `first`, `get`, `scalar`, `scalars`, `count`, `exists`.
 */
export class PreparedSelect<Row extends object, P> implements PreparedRows<P> {
  private readonly compiled = new Map<string, Compiled>();
  /** The names of the values each call takes. */
  readonly params: ReadonlySet<string>;
  private readonly select: Select<ModelSpec, Row, string, P, string, unknown>;

  /** @internal */
  constructor(select: Select<ModelSpec, Row, string, P, string, unknown>) {
    this.select = select;
    this.params = this.statement("select").names;
  }

  private statement(kind: string): Compiled {
    let c = this.compiled.get(kind);
    if (!c) {
      let sel = this.select;
      let op = "select";
      if (kind === "get") {
        sel.qs["noCuts"]("get()");
        sel = sel.limit(2) as never;
      } else if (kind === "first" || kind === "scalar") {
        if (kind === "scalar") sel.oneColumn("scalar");
        sel.qs["noCuts"]("first()");
        sel = sel.slice(0, 1) as never;
      } else if (kind === "count" || kind === "exists") {
        op = kind;
      }
      const params = new SlotParams();
      c = new Compiled(sel.ir(params, op), params);
      this.compiled.set(kind, c);
    }
    return c;
  }

  private start(kind: string, values: Record<string, unknown> | undefined): Promise<unknown> {
    const c = this.statement(kind);
    const params = c.bind(values ?? {}, this.params as Set<string>);
    checkRunnable(this.select.qs.meta, this.select.qs.state);
    return this.select.qs.db().runJson(c.json, params);
  }

  private async rows(kind: string, values: Record<string, unknown> | undefined): Promise<Row[]> {
    return this.select.finish(this.select.decode((await this.start(kind, values)) as NativeSelect));
  }

  /** Requires at least one row from each prepared read. */
  require(): this & RequiredQuery {
    return new PreparedSelect(this.select.require() as never) as this & RequiredQuery;
  }

  async all(...[values]: Values<P>): Promise<Row[]> {
    return this.rows("select", values);
  }

  async get(...[values]: Values<P>): Promise<RequiredResult<this, Row>> {
    const rows = await this.rows("get", values);
    if (rows.length > 1) throw new MultipleObjectsReturned("the query returned more than one row");
    return (rows[0] ?? null) as RequiredResult<this, Row>;
  }

  async first(...[values]: Values<P>): Promise<RequiredResult<this, Row>> {
    return ((await this.rows("first", values))[0] ?? null) as RequiredResult<this, Row>;
  }

  /** The single column of the first row, or `null` without rows. */
  async scalar(...args: [...OneColumn<Row>, ...Values<P>]): Promise<OnlyValue<Row> | null> {
    const rows = await this.rows("scalar", args[0] as Record<string, unknown> | undefined);
    return rows.length ? ((rows[0] as Record<string, unknown>)[this.select.items[0]![0]] as OnlyValue<Row>) : null;
  }

  /** The single column of every row. */
  async scalars(...args: [...OneColumn<Row>, ...Values<P>]): Promise<OnlyValue<Row>[]> {
    this.select.oneColumn("scalars");
    const key = this.select.items[0]![0];
    return (await this.rows("select", args[0] as Record<string, unknown> | undefined)).map((r) => (r as Record<string, unknown>)[key] as OnlyValue<Row>);
  }

  async count(...[values]: Values<P>): Promise<number> {
    return (await this.start("count", values)) as number;
  }

  async exists(...[values]: Values<P>): Promise<boolean> {
    return (await this.start("exists", values)) as boolean;
  }

  /** Inspect the prepared SELECT with these bound values. */
  inspect(...[values]: Values<P>): QueryInspection {
    const c = this.statement("select");
    const params = c.bind(values ?? {}, this.params as Set<string>);
    const { meta, state } = this.select.qs;
    return call(() => inspection(inspector(meta, state.db).inspect(...withScope(c.json, params))));
  }

  /** The SELECT for these values, parameters inlined (for debugging). */
  sql(...[values]: Values<P>): string {
    const c = this.statement("select");
    const params = c.bind(values ?? {}, this.params as Set<string>);
    return call(() => this.select.qs.meta.registry.native().sql(...withScope(c.json, params)));
  }
}

/** @internal */
export function makeSelect(qs: QuerySet<ModelSpec, unknown, string, unknown, string>, items: object): Select<ModelSpec, object, string, {}, never, unknown> {
  const entries = Object.entries(items);
  if (!entries.length) {
    throw new TypeError("select() needs at least one column");
  }
  if (qs.state.related.length || qs.state.prefetch.length) {
    throw new QueryError("select() can't be combined with load() of relations");
  }
  const out: [string, Node | null][] = entries.map(([key, item]) => {
    const meta = modelMeta(item);
    if (meta) {
      if (meta !== qs.meta) {
        throw new TypeError(`select() takes ${qs.meta.name} itself or expressions, not ${meta.name}`);
      }
      return [key, null];
    }
    if (!(item instanceof Expression)) {
      throw new TypeError(`select() takes columns and expressions, got ${String(item)} for ${key}`);
    }
    return [key, item instanceof Condition ? resolveNullRule(item, false, false) : item];
  });
  return new Select(qs, out);
}

registerSelect(makeSelect);

export { orderings };
