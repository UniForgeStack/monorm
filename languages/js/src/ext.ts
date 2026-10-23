/** The host surface for extension language code, next to the public `orm` API. */
import { DB, registry as defaultRegistry, type ModelMeta, type Registry } from "./model.js";
import type { ModelSpec } from "./meta.js";
import { QuerySet } from "./query.js";
import type { BoundQuerySet } from "./bound.js";
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

/** Run `op` on the query set of the rows of `bound`, fill its objects from the returned rows and give the changed objects in input order. */
export function boundWrite<R>(bound: BoundQuerySet<ModelSpec, R>, op: (qs: QuerySet<ModelSpec>) => PromiseLike<unknown>): Promise<R[]> {
  return (bound as unknown as { write(op: (qs: QuerySet<ModelSpec>) => PromiseLike<Row[]>): Promise<R[]> }).write(op as never);
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
