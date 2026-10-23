/** The host surface for extension language code, next to the public `orm` API. */
import { DB, loadedValue as loadedOf, registry as defaultRegistry, type ModelMeta, type Registry } from "./model.js";
import type { ModelSpec } from "./meta.js";
import { QuerySet } from "./query.js";
import { rowKey, type BoundQuerySet } from "./bound.js";
import type { Row } from "./model.js";
import type { Database } from "./db.js";

export { hardDeleteStatement } from "./query.js";

export const LANGUAGE_CONTRACT = 1;

/** The base of a query-set mixin: a constructor with one rest parameter, as TypeScript requires. */
export type QuerySetCtor<Q extends QuerySet<ModelSpec> = QuerySet<ModelSpec>> = new (...args: any[]) => Q;

/** The base of a bound-query-set mixin. */
export type BoundQuerySetCtor = new (...args: any[]) => BoundQuerySet<ModelSpec, unknown>;

/** The model of a bound query set. */
export const boundMeta: (bound: object) => ModelMeta = ((bound: { meta: ModelMeta }) => bound.meta) as never;

/** The database that loaded `instance`; `undefined` when no database did. */
export const databaseOf = (instance: object): Database | undefined => (instance as { [DB]?: Database })[DB];

/** The objects of a bound query set, grouped by primary key; a group has its objects in input order. */
export function boundGroups(bound: object): readonly (readonly Row[])[] {
  const b = bound as { meta: ModelMeta; objs: readonly Row[] };
  const groups = new Map<string, Row[]>();
  for (const o of b.objs) {
    const id = rowKey(b.meta, o);
    const group = groups.get(id);
    if (group === undefined) groups.set(id, [o]); else group.push(o);
  }
  return [...groups.values()];
}

/** The loaded value of `field`, a field of the model of `object`; throws `NotLoaded` when it is not loaded. */
export function loadedValue(object: object, field: { readonly name: string }): unknown {
  return loadedOf(object as Row, field);
}

/** Whether `bound` is the set of an instance write: it matches the loaded guard values of its row already, so a bound-write extension adds no such check. */
export const boundGuarded: (bound: object) => boolean = ((bound: { guarded: boolean }) => bound.guarded) as never;

/** The own properties of an object; a merge changes the plain objects under its symbol keys in place, so copy them. */
function snapshot(o: object) {
  const own = Object.getOwnPropertyDescriptors(o);
  for (const key of Object.getOwnPropertySymbols(own)) {
    const value = own[key as never]?.value;
    if (value !== null && typeof value === "object" && Object.getPrototypeOf(value) === Object.prototype) own[key as never] = { ...own[key as never], value: { ...value } } as never;
  }
  return own;
}

/**
 * Run `fn` in a transaction (a savepoint in one). When it throws, its writes roll back, and the
 * objects of `bound` get the state that they had before.
 */
export async function boundAtomic<T>(bound: object, fn: () => Promise<T>): Promise<T> {
  const b = bound as { objs: readonly object[]; database(): Database };
  const before = b.objs.map((o) => [o, snapshot(o)] as const);
  try {
    return await b.database().transaction(fn);
  } catch (e) {
    for (const [o, own] of before) {
      for (const key of Reflect.ownKeys(o)) if (!(key in own)) delete (o as Record<PropertyKey, unknown>)[key];
      Object.defineProperties(o, own);
    }
    throw e;
  }
}

/**
 * Each of `objs` whose row the filters of `bound` keep, with a new instance of that row that has
 * `columns` loaded and nothing else. The `objs` stay as they are.
 */
export async function boundFresh(bound: object, objs: readonly object[], ...columns: readonly object[]): Promise<(readonly [object, object])[]> {
  const b = bound as { meta: ModelMeta; inner: QuerySet<ModelSpec>; database(): Database; keyFilter(first: readonly Row[]): never };
  const rows = objs as readonly Row[];
  const first = [...new Map(rows.map((o) => [rowKey(b.meta, o), o])).values()];
  const base = (b.inner as unknown as { clone(c: object): QuerySet<ModelSpec> }).clone({ loads: [] }).using(b.database());
  const read = (base.filter as (...c: unknown[]) => QuerySet<ModelSpec>)(b.keyFilter(first));
  const loaded = (read.load as (...i: unknown[]) => unknown)(...columns) as PromiseLike<Row[]>;
  const fresh = new Map((await loaded).map((r) => [rowKey(b.meta, r), r]));
  return rows.flatMap((o) => { const row = fresh.get(rowKey(b.meta, o)); return row === undefined ? [] : [[o, row] as const]; });
}

/** Run `op` on the query set of the rows of `bound`, fill its objects from the returned rows and give the changed objects in input order. */
export function boundWrite<R>(bound: BoundQuerySet<ModelSpec, R>, op: (qs: QuerySet<ModelSpec>) => PromiseLike<unknown>): Promise<R[]> {
  return (bound as unknown as { _write(op: (qs: QuerySet<ModelSpec>) => PromiseLike<Row[]>): Promise<R[]> })._write(op as never);
}

/**
 * The base of an instance class that overrides a host member: `H` types the host members,
 * and `super` reaches the definition below the extension on the model.
 */
export const InstanceBase = class {} as unknown as new <H extends object = {}>() => H;

/** The prepared behavior records of `kind` for the model of `qs`. */
export function behaviorRecords(qs: QuerySet<ModelSpec>, kind: string): readonly Readonly<Record<string, unknown>>[] {
  const meta = qs.meta;
  return meta.registry.behaviorRecords(kind, meta.name);
}

/** The prepared behavior record of `kind` that is not a list of per-model records, `undefined` if absent. */
export function behaviorValue(kind: string, registry: Registry = defaultRegistry): unknown {
  return registry.behaviorValue(kind);
}

/** The field that a behavior record names in its `field` key (an IR name): its language name and its column. */
export function behaviorField(meta: ModelMeta, record: Readonly<Record<string, unknown>>): { readonly name: string; readonly column: ReturnType<ModelMeta["column"]> } {
  const field = meta.fieldByIr.get(String(record["field"]));
  if (field === undefined) throw new TypeError(`${meta.name} has no field ${String(record["field"])}`);
  return { name: field.name, column: meta.column(field) };
}
