/** Protobuf message fields on a Bytes column: the codec of a message type, found from its API or registered. */
import { createRequire } from "node:module";
import { FieldAdapterError } from "monorm";

export class ProtoError extends FieldAdapterError {}

/** The functions of one message type. The JSON pair is optional; `is` is the class check when the type has a runtime form. */
export interface Codec {
  encode(message: unknown): Uint8Array;
  decode(bytes: Uint8Array): unknown;
  toJson?(message: unknown): unknown;
  fromJson?(json: unknown): unknown;
  is?(value: unknown): boolean;
}

const registry = new Map<unknown, Codec>();

/** Register the codec of a message type that no known protobuf library made. `target` is the exported value of the type (or its protobuf-es schema). It wins over detection. */
export function protoCodec(target: object, codec: Codec): void {
  registry.set(target, codec);
}

type Bufbuild = {
  toBinary(schema: unknown, message: unknown): Uint8Array;
  fromBinary(schema: unknown, bytes: Uint8Array): unknown;
  toJson(schema: unknown, message: unknown): unknown;
  fromJson(schema: unknown, json: unknown): unknown;
};
// An optional peer package, loaded on first use and synchronously: a top-level await would stop a synchronous require() of this module.
let bufbuildLoaded: Bufbuild | undefined | null = null;
function loadBufbuild(): Bufbuild | undefined {
  if (bufbuildLoaded === null) {
    try { bufbuildLoaded = createRequire(import.meta.url)("@bufbuild/protobuf") as Bufbuild; } catch { bufbuildLoaded = undefined; }
  }
  return bufbuildLoaded;
}

type Table = Record<string, unknown>;
const fn = (target: Table, name: string): ((...args: unknown[]) => unknown) | undefined => (typeof target[name] === "function" ? (target[name] as (...args: unknown[]) => unknown).bind(target) : undefined);

/** The codec of the message `symbol` in `module`: the registered one, else the one its API shows; an error that names `protoCodec` when there is none. */
export function codecFor(module: object, symbol: string): Codec {
  const table = module as Table;
  const bufbuild = loadBufbuild();
  const schema = table[`${symbol}Schema`];
  const target = table[symbol];
  for (const key of [target, schema]) {
    const registered = key === undefined ? undefined : registry.get(key);
    if (registered) return registered;
  }
  // protobuf-es v2: the type has no runtime form; `<Name>Schema` holds its descriptor.
  if (schema && typeof schema === "object" && "typeName" in schema && bufbuild) {
    const typeName = (schema as { typeName: string }).typeName;
    return {
      encode: m => bufbuild.toBinary(schema, m),
      decode: b => bufbuild.fromBinary(schema, b),
      toJson: m => bufbuild.toJson(schema, m),
      fromJson: j => bufbuild.fromJson(schema, j),
      is: v => typeof v === "object" && v !== null && (v as { $typeName?: unknown }).$typeName === typeName,
    };
  }
  // ts-proto and protobufjs: `encode` and `decode` on the exported value.
  if (target && (typeof target === "object" || typeof target === "function")) {
    const t = target as Table;
    const encode = fn(t, "encode");
    const decode = fn(t, "decode");
    if (encode && decode) {
      const toJson = fn(t, "toJSON");
      const fromJson = fn(t, "fromJSON");
      return {
        // The writer of ts-proto and protobufjs ends with `finish()`.
        encode: m => (encode(m) as { finish(): Uint8Array }).finish(),
        decode: b => decode(b),
        ...(toJson && fromJson ? { toJson: (m: unknown) => toJson(m), fromJson: (j: unknown) => fromJson(j) } : {}),
        ...(typeof target === "function" ? { is: (v: unknown) => v instanceof (target as new () => object) } : {}),
      };
    }
  }
  const hint = schema && !bufbuild ? "; the package @bufbuild/protobuf is not installed" : "";
  throw new ProtoError(`no codec for ${symbol}: it has no known protobuf API${hint}; register one with protoCodec(target, { encode, decode })`);
}
