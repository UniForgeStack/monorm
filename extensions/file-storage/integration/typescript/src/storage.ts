/** OpenDAL-backed storage: the reference codec, the operator map and the streaming upload. */
import { randomUUID } from "node:crypto";
import type { Operator } from "opendal";

export const CHUNK_SIZE = 64 * 1024;
export const DEFAULT_CONTENT_TYPE = "application/octet-stream";
export class StorageError extends Error {}
export class CapabilityError extends StorageError {}
export class SizeLimitError extends StorageError {}
export interface ReferenceData {
  readonly v: 1;
  readonly storage: string;
  readonly key: string;
  readonly version?: string;
  readonly filename?: string;
  readonly content_type?: string;
  readonly size?: number;
}
export class Reference {
  readonly storage: string;
  readonly key: string;
  readonly version?: string;
  readonly filename?: string;
  readonly contentType?: string;
  readonly size?: number;
  constructor(data: ReferenceData) {
    const allowed = ["v", "storage", "key", "version", "filename", "content_type", "size"];
    if (data.v !== 1 || Object.keys(data).some(k => !allowed.includes(k))) throw new TypeError("invalid reference version or properties");
    for (const name of ["storage", "key"] as const) {
      if (typeof data[name] !== "string" || !data[name] || data[name].includes("\0")) throw new TypeError(`invalid ${name}`);
    }
    for (const name of ["version", "filename", "content_type"] as const) {
      if (Object.hasOwn(data, name) && (typeof data[name] !== "string" || data[name]!.includes("\0"))) throw new TypeError(`invalid ${name}`);
    }
    for (const value of [data.storage, data.key, data.version, data.filename, data.content_type]) {
      if (value !== undefined && /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(value)) throw new TypeError("reference strings must contain valid Unicode scalars");
    }
    if (Object.hasOwn(data, "size") && (!Number.isSafeInteger(data.size) || data.size! < 0)) throw new TypeError("size must be a nonnegative safe integer");
    this.storage = data.storage; this.key = data.key; this.version = data.version;
    this.filename = data.filename; this.contentType = data.content_type; this.size = data.size;
    Object.freeze(this);
  }
  toJSON(): ReferenceData {
    return { v: 1, storage: this.storage, key: this.key,
      ...(this.version !== undefined ? { version: this.version } : {}),
      ...(this.filename !== undefined ? { filename: this.filename } : {}),
      ...(this.contentType !== undefined ? { content_type: this.contentType } : {}),
      ...(this.size !== undefined ? { size: this.size } : {}) };
  }
  static fromJSON(value: string | unknown): Reference {
    const data: unknown = typeof value === "string" ? JSON.parse(value) : value;
    if (!data || typeof data !== "object" || Array.isArray(data)) throw new TypeError("reference must be an object");
    return new Reference(data as ReferenceData);
  }
}
/** An upload failed. The object may exist at `reference` (the candidate), with no size. */
export class UploadError extends StorageError {
  constructor(readonly reference: Reference, cause: unknown) {
    super(`upload failed; candidate key=${reference.key}`, { cause });
  }
}
export type Source = Uint8Array | AsyncIterable<Uint8Array>;
export interface UploadOptions { filename?: string; contentType?: string; maxSize?: number; signal?: AbortSignal }
export function checkExpiry(expiresIn: number): void {
  if (!Number.isInteger(expiresIn) || expiresIn < 1 || expiresIn > 604800) throw new RangeError("expiresIn must be an integer from 1 to 604800 seconds");
}
export async function* chunks(source: Source, options: UploadOptions = {}): AsyncGenerator<Uint8Array> {
  const limit = options.maxSize ?? Number.MAX_SAFE_INTEGER;
  if (!Number.isSafeInteger(limit) || limit < 0) throw new RangeError("maxSize must be a nonnegative safe integer");
  let size = 0;
  // Do not call return() on caller iterators: it may close a caller-owned stream.
  const iterator = source instanceof Uint8Array ? undefined : source[Symbol.asyncIterator]();
  let offset = 0;
  while (true) {
    options.signal?.throwIfAborted();
    let chunk: Uint8Array;
    if (source instanceof Uint8Array) {
      if (offset >= source.length) break;
      chunk = source.subarray(offset, offset + CHUNK_SIZE); offset += chunk.length;
    } else {
      const next = await iterator!.next();
      options.signal?.throwIfAborted();
      if (next.done) break;
      chunk = next.value;
    }
    if (!(chunk instanceof Uint8Array)) throw new TypeError("upload streams must yield Uint8Array");
    size += chunk.length;
    if (size > limit) throw new SizeLimitError(`upload exceeds ${limit} bytes`);
    for (let i = 0; i < chunk.length; i += CHUNK_SIZE) {
      options.signal?.throwIfAborted();
      yield chunk.subarray(i, i + CHUNK_SIZE);
    }
  }
}
/** The application's map from storage name to OpenDAL operator. The ORM creates no operators. */
export class Registry {
  private readonly operators: ReadonlyMap<string, Operator>;
  constructor(operators: ReadonlyMap<string, Operator>) {
    for (const name of operators.keys()) new Reference({ v: 1, storage: name, key: "validation" });
    this.operators = new Map(operators);
  }
  operator(storage: string): Operator {
    const operator = this.operators.get(storage);
    if (!operator) throw new StorageError(`unconfigured storage: ${storage}`);
    return operator;
  }
  async upload(storage: string, source: Source, options: UploadOptions = {}): Promise<Reference> {
    const operator = this.operator(storage);
    const data: ReferenceData = { v: 1, storage, key: randomUUID().replaceAll("-", ""),
      ...(options.filename !== undefined ? { filename: options.filename } : {}),
      content_type: options.contentType || DEFAULT_CONTENT_TYPE };
    const candidate = new Reference(data);
    let size = 0;
    try {
      // Never close the writer after an error: closing commits the partial object.
      const writer = await operator.writer(candidate.key, { contentType: data.content_type });
      for await (const chunk of chunks(source, options)) {
        await writer.write(Buffer.from(chunk.buffer, chunk.byteOffset, chunk.byteLength));
        size += chunk.length;
      }
      options.signal?.throwIfAborted();
      await writer.close();
      const version = operator.capability().readWithVersion ? (await operator.stat(candidate.key)).version : undefined;
      return new Reference({ ...data, ...(version ? { version } : {}), size });
    } catch (error) {
      throw new UploadError(candidate, error);
    }
  }
  async *open(reference: Reference): AsyncGenerator<Uint8Array> {
    const reader = await this.operator(reference.storage).reader(reference.key, reference.version === undefined ? undefined : { version: reference.version });
    yield* reader.createReadStream() as AsyncIterable<Uint8Array>;
  }
  async signedUrl(reference: Reference, options: { expiresIn?: number } = {}): Promise<string> {
    const expiry = options.expiresIn ?? 300;
    checkExpiry(expiry);
    const operator = this.operator(reference.storage);
    if (!operator.capability().presignRead) throw new CapabilityError(`storage ${reference.storage} does not support signed URLs`);
    try { return (await operator.presignRead(reference.key, expiry)).url; }
    catch (error) {
      if (error instanceof Error && /unsupported/i.test(error.message)) throw new CapabilityError(`storage ${reference.storage} does not support signed URLs`, { cause: error });
      throw error;
    }
  }
}
