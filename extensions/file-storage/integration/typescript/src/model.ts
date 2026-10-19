/** Selected model setup, shared by generated and runtime-defined hosts. */
import { Pending, registerFieldAdapter, type FieldAdapter, type ModelMeta } from "orm";
import { FileField, FileFieldError, Reference, Registry, Upload } from "./index.js";
import { prepareDecoder } from "./decoder.js";

/** The async field adapter of one file field: `validate` and `prepare` write, `decode` reads. */
export class FileAdapter implements FieldAdapter {
  constructor(readonly field: FileField, private readonly registry: () => Registry | undefined) {}
  validate(value: unknown): unknown {
    if (!(value instanceof Upload)) return this.field.decode(value)?.toJSON() ?? null;
    new Reference({ v: 1, storage: this.field.storage, key: "validation",
      ...(value.options.filename !== undefined ? { filename: value.options.filename } : {}),
      ...(value.options.contentType !== undefined ? { content_type: value.options.contentType } : {}) });
    const limit = value.options.maxSize ?? Number.MAX_SAFE_INTEGER;
    if (!Number.isSafeInteger(limit) || limit < 0) throw new FileFieldError("maxSize must be a nonnegative safe integer");
    this.provider();
    return new Pending(value, new Reference({ v: 1, storage: this.field.storage, key: "preflight" }).toJSON());
  }
  async prepare(value: unknown): Promise<unknown> {
    return (await this.provider().upload(this.field.storage, (value as Upload).source, (value as Upload).options)).toJSON();
  }
  decode(stored: unknown): Reference | null { return this.field.decode(stored); }
  private provider() {
    const registry = this.registry();
    if (!registry) throw new FileFieldError("configure file storage before preparing uploads");
    registry.operator(this.field.storage);
    return registry;
  }
}

export class ModelAdapter {
  readonly fields: ReadonlyMap<string, FileField>;
  constructor(fields: ReadonlyMap<string, FileField>, private registry?: Registry) { this.fields = new Map(fields); }
  configure(registry: Registry): void { this.registry = registry; }
  currentRegistry(): Registry | undefined { return this.registry; }
  decoder(publicFields: readonly (readonly [string, number])[]) { return prepareDecoder(this.fields, publicFields); }
  client(): Registry {
    if (!this.registry) throw new FileFieldError("configure file storage before file operations");
    return this.registry;
  }
}

export function installModel(prototype: object, fields: ReadonlyMap<string, FileField>, registry?: Registry, host?: ModelMeta): ModelAdapter {
  const adapter = new ModelAdapter(fields, registry);
  const methods = new Map<string, (this: Record<string, unknown>, options?: { expiresIn?: number }) => unknown>();
  for (const [name, field] of fields) {
    methods.set(`${name}SignedUrl`, function(options = {}) {
      const reference = field.reference(this);
      return adapter.client().signedUrl(reference, options);
    });
    methods.set(`${name}Open`, function() {
      const reference = field.reference(this);
      return adapter.client().open(reference);
    });
  }
  for (const name of methods.keys()) if (name in prototype) throw new FileFieldError(`${name}: file method collision`);
  for (const [name, value] of methods) Object.defineProperty(prototype, name, { value });
  if (host) for (const [name, field] of fields) registerFieldAdapter(host, name, new FileAdapter(field, () => adapter.currentRegistry()));
  return adapter;
}
