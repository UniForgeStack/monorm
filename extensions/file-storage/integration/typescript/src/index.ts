import { Reference, Registry, type Source, type UploadOptions } from "@orm/storage";

export class FileFieldError extends Error {}
export class FileNotLoaded extends FileFieldError {}
export class MissingFile extends FileFieldError {}
export class Upload {
  readonly options: Readonly<UploadOptions>;
  constructor(readonly source: Source, options: Readonly<UploadOptions> = {}) {
    if (!(source instanceof Uint8Array) && (!source || typeof source[Symbol.asyncIterator] !== "function")) throw new TypeError("Upload requires bytes or a byte stream");
    this.options = Object.freeze({ ...options });
  }
}
export class FileField {
  constructor(readonly name: string, readonly storage: string, readonly nullable = false) {}
  decode(value: unknown): Reference | null {
    if (value === null) {
      if (!this.nullable) throw new MissingFile(`${this.name} is not nullable`);
      return null;
    }
    const reference = value instanceof Reference ? value : Reference.fromJSON(value);
    if (reference.storage !== this.storage) throw new FileFieldError(`${this.name} requires storage ${this.storage}`);
    return reference;
  }
  reference(snapshot: Readonly<Record<string, unknown>>): Reference {
    if (!Object.hasOwn(snapshot, this.name)) throw new FileNotLoaded(`${this.name} was not selected`);
    const reference = this.decode(snapshot[this.name]);
    if (reference === null) throw new MissingFile(`${this.name} has no file`);
    return reference;
  }
  async signedUrl(snapshot: Readonly<Record<string, unknown>>, registry: Registry, options: { expiresIn?: number } = {}): Promise<string> {
    const reference = this.reference(snapshot);
    return registry.resolve(reference).signedUrl(reference, options);
  }
  open(snapshot: Readonly<Record<string, unknown>>, registry: Registry): AsyncIterable<Uint8Array> {
    const reference = this.reference(snapshot);
    return registry.resolve(reference).open(reference);
  }
}
