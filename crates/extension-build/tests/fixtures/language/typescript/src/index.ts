import type { ModelSpec, QuerySet } from "orm";
import type { QuerySetCtor } from "orm/ext";

type AnyQuerySet = QuerySet<ModelSpec>;

export function taggedQuerySet<B extends QuerySetCtor>(Base: B) {
  return class Tagged extends Base {
    everything<T extends AnyQuerySet>(this: T): T {
      return this.withoutDefaults();
    }
  };
}

export class TaggedInstance {
  label(this: { readonly pk: unknown }): string {
    return String(this.pk);
  }
}
