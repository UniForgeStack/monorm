// The package build exposes this module; the fixture tests compile it to CommonJS.
import { registerFieldAdapter, type FieldAdapter, type ModelMeta } from "orm";

export const manifestHash = "f64633107faaef36344c6d4847832c644650e70218e95f1e0995d7c241b8a73d";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}

/** What an `acme.outside.wrapped.v1` field shows: the stored value with a label. */
export class Wrapped {
  constructor(readonly label: string, readonly payload: unknown) {}
}

class Runtime {
  prefix = "";
  configure(prefix: string): void { this.prefix = prefix; }
}

class WrappedAdapter implements FieldAdapter {
  constructor(private readonly runtime: Runtime, private readonly prefix: string) {}
  validate(value: unknown): unknown { return value instanceof Wrapped ? value.payload : value; }
  async prepare(value: unknown): Promise<unknown> { return value; }
  decode(stored: unknown): unknown { return stored === null || stored === undefined ? null : new Wrapped(this.runtime.prefix + this.prefix, stored); }
}

/** The generated-code entry for the `acme.outside.wrapped.v1` adapter. */
export function install(model: { readonly _meta: ModelMeta }, fields: ReadonlyMap<string, { options: { prefix: string }; nullable: boolean }>): Runtime {
  const runtime = new Runtime();
  for (const [name, { options }] of fields) {
    registerFieldAdapter(model._meta, name, new WrappedAdapter(runtime, options.prefix), {
      [`${name}Unwrap`](this: Record<string, unknown>) { return (this[name] as Wrapped).payload; },
    });
  }
  return runtime;
}
