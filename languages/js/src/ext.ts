/** The host surface for extension language code, next to the public `orm` API. */
import type { ModelMeta } from "./model.js";
import type { ModelSpec } from "./meta.js";
import { QuerySet } from "./query.js";
import { rowQuery } from "./model.js";

export { hardDeleteStatement } from "./query.js";

/** The query set of the one row of `instance`: its primary key, no defaults, its database. */
export const rowQueryOf: (instance: object) => QuerySet<ModelSpec> = rowQuery as never;

export const LANGUAGE_CONTRACT = 1;

/** The base of a query-set mixin: a constructor with one rest parameter, as TypeScript requires. */
export type QuerySetCtor<Q extends QuerySet<ModelSpec> = QuerySet<ModelSpec>> = new (...args: any[]) => Q;

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

/** The column that a behavior record names in its `field` key. */
export function behaviorField(meta: ModelMeta, record: Readonly<Record<string, unknown>>): ReturnType<ModelMeta["column"]> {
  const field = meta.fields.get(String(record["field"]));
  if (field === undefined) throw new TypeError(`${meta.name} has no field ${String(record["field"])}`);
  return meta.column(field);
}
