/** The adapter of a protobuf field: a message is encoded to the bytes of the Bytes column and decoded when it loads. */
import { registerFieldAdapter, type FieldAdapter, type ModelMeta } from "monorm";
import { ProtoError, codecFor, type Codec } from "./index.js";

/** The field adapter of one proto field. The codec resolves at first use, so a model module can load before its messages. */
export class ProtoAdapter implements FieldAdapter {
  #codec: Codec | undefined;
  constructor(readonly model: string, readonly field: string, readonly message: string, private readonly module: object, private readonly symbol: string) {}
  get codec(): Codec {
    if (!this.#codec) {
      try { this.#codec = codecFor(this.module, this.symbol); }
      catch (error) { throw new ProtoError(`${this.model}.${this.field}: ${(error as Error).message}`); }
    }
    return this.#codec;
  }
  validate(value: unknown): unknown {
    if (value === null) return null;
    const { codec } = this;
    if (typeof value !== "object" || (codec.is && !codec.is(value))) throw new ProtoError(`${this.model}.${this.field} takes ${this.message}, not ${typeof value === "object" ? (value as object).constructor.name : typeof value}`);
    return codec.encode(value);
  }
  prepare(): Promise<unknown> { return Promise.reject(new ProtoError(`${this.model}.${this.field}: a proto value never needs prepare`)); }
  decode(stored: unknown): unknown {
    if (stored === null) return null;
    try { return this.codec.decode(stored as Uint8Array); }
    catch (error) {
      if (error instanceof ProtoError) throw error;
      throw new ProtoError(`${this.model}.${this.field}: the stored bytes are not a ${this.message}: ${(error as Error).message}`, { cause: error });
    }
  }
  adminMode(): "json" | "text" {
    try { return this.codec.toJson && this.codec.fromJson ? "json" : "text"; }
    catch { return "text"; }
  }
  adminToView(stored: Uint8Array): unknown {
    const { codec } = this;
    if (!codec.toJson) throw new ProtoError(`${this.model}.${this.field}: the codec has no JSON functions`);
    const message = this.decode(stored);
    try { return codec.toJson(message); }
    catch (error) { throw new ProtoError(`${this.model}.${this.field}: not a JSON message: ${(error as Error).message}`, { cause: error }); }
  }
  adminFromView(value: unknown): unknown {
    const { codec } = this;
    if (!codec.fromJson) throw new ProtoError(`${this.model}.${this.field}: the codec has no JSON functions`);
    try { return codec.fromJson(value); }
    catch (error) { throw new ProtoError(`${this.model}.${this.field}: not a ${this.message}: ${(error as Error).message}`, { cause: error }); }
  }
}

/** The generated-code entry for the `proto.message.v1` adapter: `fields` maps a name to its options and nullability. */
export function install(model: { readonly _meta: ModelMeta }, fields: ReadonlyMap<string, { options: { message: string; module: object; symbol: string }; nullable: boolean }>): void {
  for (const [name, { options }] of fields) registerFieldAdapter(model._meta, name, new ProtoAdapter(model._meta.name, name, options.message, options.module, options.symbol));
}
