/** Bound query sets: reads of rows named by objects the caller already holds. */
import { Column, Labeled, or, type Expression } from "./expr.js";
import { resolve, type Database } from "./db.js";
import { QueryError } from "./errors.js";
import { merge } from "./loading.js";
import { DB, fieldValue, invalidateReference, type Instance, type ModelMeta, type Row } from "./model.js";
import { prefetch, registerBound, QuerySet, type Allowed, type LoadAll, type LoadItem, type LockOptions, type Narrow, type RootNames } from "./query.js";
import type { QueryInspection } from "./inspection.js";
import { key } from "./value-key.js";
import type { ModelSpec } from "./meta.js";

type Cond<M extends ModelSpec> = Expression<boolean | null, Allowed<M["name"]>, unknown>;

/**
 * A query set filtered to the primary keys of objects you hold. `await` fills each matched
 * object in place (selected fields are overwritten, other loaded fields keep their value,
 * a relation whose key changed is dropped) and gives the matched objects in input order.
 * Objects whose row does not match stay unchanged. Each `await` queries afresh.
 *
 * ```ts
 * const posts = await of(held).filter(Post.published).load(Post.title);
 * ```
 *
 * It has no ordering, slicing or one-row methods: the result is the objects you gave.
 */
export class BoundQuerySet<M extends ModelSpec, R = Instance<M>> implements PromiseLike<R[]> {
  declare readonly "~bound"?: [M, R];

  /** @internal */
  constructor(
    private readonly meta: ModelMeta,
    private readonly objs: readonly Row[],
    private readonly inner: QuerySet<ModelSpec>,
    private readonly items: readonly LoadItem<ModelSpec>[] = [],
  ) {}

  private next(inner: QuerySet<ModelSpec>, items = this.items): never {
    return new BoundQuerySet(this.meta, this.objs, inner, items) as never;
  }

  /** Keep the objects whose row also matches all `conditions`. */
  filter(...conditions: readonly Cond<M>[]): BoundQuerySet<M, R> {
    return this.next((this.inner.filter as (...c: unknown[]) => QuerySet<ModelSpec>)(...conditions));
  }

  /** Drop the objects whose row matches all `conditions`. */
  exclude(...conditions: readonly Cond<M>[]): BoundQuerySet<M, R> {
    return this.next((this.inner.exclude as (...c: unknown[]) => QuerySet<ModelSpec>)(...conditions));
  }

  /** Columns and relations to read, as {@link QuerySet.load}; the objects keep the fields not named. */
  load<const C extends readonly LoadItem<M>[]>(...items: C): BoundQuerySet<M, Narrow<R, M, RootNames<C>> & LoadAll<C>> {
    return this.next((this.inner.load as (...i: unknown[]) => QuerySet<ModelSpec>)(...items), [...this.items, ...items] as never);
  }

  /** `FOR UPDATE` (or `FOR SHARE`) on the rows read. */
  lock(options?: LockOptions): BoundQuerySet<M, R> {
    return this.next((this.inner.lock as (o?: LockOptions) => QuerySet<ModelSpec>)(options));
  }

  /** Read from `db` instead of the database of the objects. */
  using(db?: Database): BoundQuerySet<M, R> {
    return this.next(this.inner.using(db));
  }

  /** Skip the model's query defaults (all, or the named ones). */
  withoutDefaults(...names: string[]): BoundQuerySet<M, R> {
    return this.next((this.inner.withoutDefaults as (...n: string[]) => QuerySet<ModelSpec>)(...names));
  }

  /** Skip the relations that `load()` and the model's defaults would add. */
  withoutRelated(): BoundQuerySet<M, R> {
    return this.next(this.inner.withoutRelated());
  }

  /** How many of the objects' rows match. */
  count(): Promise<number> {
    return this.keyed().count() as Promise<number>;
  }

  /** Whether at least one of the objects' rows matches. */
  exists(): Promise<boolean> {
    return this.keyed().exists() as Promise<boolean>;
  }

  /** The SQL of the root read. */
  sql(): string {
    return this.keyed().sql();
  }

  /** The SQL and plan of the root read. */
  inspect(): QueryInspection {
    return this.keyed().inspect();
  }

  then<A = R[], B = never>(onfulfilled?: ((rows: R[]) => A | PromiseLike<A>) | null, onrejected?: ((reason: unknown) => B | PromiseLike<B>) | null): Promise<A | B> {
    return this.run().then(onfulfilled, onrejected);
  }

  /** The inner query set with the key filter of the distinct objects. */
  private keyed(): QuerySet<ModelSpec> {
    const { meta } = this;
    const seen = new Set<string>();
    const values: unknown[] = [];
    for (const o of this.objs) {
      const id = rowKey(meta, o);
      if (seen.has(id)) continue;
      seen.add(id);
      values.push(o["pk"]);
    }
    const filter = meta.key.length === 1
      ? meta.column(meta.key[0]!).in(values as never)
      : or(...values.map((v) => meta.keyMatch(meta.key, v)) as [never]);
    return this.inner.using(this.database()).filter(filter as never);
  }

  private database(): Database {
    const { objs } = this;
    const given = this.inner.state.db;
    const db = resolve(given ?? (objs[0]![DB] as Database | undefined));
    if (given === undefined && objs.some((o) => resolve(o[DB] as Database | undefined) !== db)) {
      throw new QueryError("of() takes objects from one database; pass using() to read another");
    }
    return db;
  }

  private async run(): Promise<R[]> {
    const { meta, objs } = this;
    const st = this.inner.state;
    const db = this.database();
    const loads = st.loads ?? [];
    // no root column to read: the keys come from the objects, as prefetch() does
    if (loads.length && !st.filters.length && st.lock === undefined && !loads.some((i) => i instanceof Labeled || (i instanceof Column && !i.path.length))) {
      await prefetch(objs, ...this.items, { using: db });
      return [...objs] as R[];
    }
    const fresh = (await (this.keyed() as unknown as { fetch(): Promise<Row[]> }).fetch());
    const byKey = new Map(fresh.map((r) => [rowKey(meta, r), r]));
    const out: Row[] = [];
    for (const o of objs) {
      const row = byKey.get(rowKey(meta, o));
      if (row === undefined) continue;
      merge(o, row, true, invalidateReference);
      out.push(o);
    }
    return out as R[];
  }
}

function rowKey(meta: ModelMeta, row: Row): string {
  return meta.key.map((f) => { const k = key(fieldValue(row, f.name)); return `${typeof k}:${String(k)}`; }).join("\0");
}

registerBound((qs, objs) => {
  const meta = qs.meta;
  const where = (qs as { relation?: unknown }).relation !== undefined || qs.state.via !== undefined ? "a relation query set" : undefined;
  if (where) throw new TypeError(`of() does not take ${where}; use ${meta.name}.objects.of()`);
  const s = qs.state;
  const extra = s.order.length ? "ordering" : s.limit !== undefined || s.offset !== undefined ? "a slice" : s.joins.length ? "a join" : s.from !== undefined ? "a source other than the table" : undefined;
  if (extra) throw new TypeError(`of() does not take a query set with ${extra}`);
  if (!objs.length) throw new TypeError("of() takes at least one object");
  for (const o of objs) {
    if ((o as { constructor?: { meta?: ModelMeta } }).constructor?.meta !== meta) throw new TypeError(`of() takes objects of one model: ${meta.name}`);
    if ((o as Row)["pk"] == null) throw new TypeError(`of() takes objects with a primary key: ${meta.name}`);
  }
  return new BoundQuerySet(meta, objs as Row[], qs);
});

/** A bound query set for `objs`, objects of one model: `of(posts).load(Post.author)`. */
export function of<R extends { qs(): BoundQuerySet<ModelSpec, unknown> }>(objs: R | Iterable<R>): R extends { qs(): BoundQuerySet<infer M, unknown> } ? BoundQuerySet<M, R> : never {
  const list = typeof objs === "object" && objs !== null && Symbol.iterator in objs ? [...(objs as Iterable<R>)] : [objs as R];
  if (!list.length) throw new TypeError("of() takes at least one object");
  const meta = (list[0] as { constructor?: { meta?: ModelMeta } }).constructor?.meta;
  if (meta === undefined) throw new TypeError("of() takes model instances");
  return (meta.objects as QuerySet<ModelSpec> & { of(o: readonly R[]): unknown }).of(list) as never;
}
