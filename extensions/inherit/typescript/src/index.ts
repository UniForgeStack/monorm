import { registry as defaultRegistry } from "@ufstack/monorm";
import type { ModelClass, ModelSpec, QuerySet, Registry } from "@ufstack/monorm";
import { behaviorValue } from "@ufstack/monorm/ext";
import type { QuerySetCtor } from "@ufstack/monorm/ext";

/** No members: the extension changes the schema, not the query set. */
export function inheritQuerySet<B extends QuerySetCtor>(Base: B) {
  return class Inherit extends Base {};
}

/** No members: the extension changes the schema, not the instance. */
export class InheritInstance {}

/** The model class that is active for `name`: the model that overrides it, else `name` itself. */
export function modelFor(name: string, registry: Registry = defaultRegistry): ModelClass<ModelSpec> & Record<string, unknown> {
  const overrides = (behaviorValue("model_overrides", registry) ?? {}) as Readonly<Record<string, string>>;
  return registry.get(Object.hasOwn(overrides, name) ? (overrides[name] as string) : name).model;
}

export type { QuerySet };
