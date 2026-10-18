import type { BoundQuerySet, ModelSpec, QuerySet } from "orm";
import type { BoundQuerySetCtor, QuerySetCtor } from "orm/ext";

type AnyQuerySet = QuerySet<ModelSpec>;

export function taggedQuerySet<B extends QuerySetCtor>(Base: B) {
  return class Tagged extends Base {
    everything<T extends AnyQuerySet>(this: T): T {
      return this.withoutDefaults();
    }
  };
}

export function taggedBoundQuerySet<B extends BoundQuerySetCtor>(Base: B) {
  return class TaggedBound extends Base {
    everything<T extends BoundQuerySet<ModelSpec, unknown>>(this: T): T {
      return this.withoutDefaults() as T;
    }
  };
}

export class TaggedInstance {
  label(this: { readonly pk: unknown }): string {
    return String(this.pk);
  }
}
