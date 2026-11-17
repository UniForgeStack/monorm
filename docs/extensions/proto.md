# Protobuf fields

A proto field is a nullable `Bytes` column that holds one encoded protobuf message.
The generated model types the field as your own message class.
The ORM encodes a message before the SQL statement and decodes the stored bytes when you read the field.

**Enable:** nothing to install. The extension is built into the `monorm` Python wheel and the `monorm` npm package.
It has the `monorm.proto` extension and the runtime module.
Without the package, `monorm generate` rejects `@proto.message` with a namespace error.
The host has no protobuf code.

## Schema

```prisma
model Event {
  id      Int    @id @default(autoincrement())
  payload Bytes? @proto.message("acme.Note")
}
```

`@proto.message("<full name>")` takes the full name of the message: a package and a name, with dots (`acme.Outer.Inner` for a nested message).
The schema load refuses these on a proto field:
`@index`, `@unique`, `@check`, a default, a key, a list of proto fields, a proto field inside a nested `type`, and a field that is not `Bytes`.
The reason is that the encoding of a message is not canonical: two equal messages can have different bytes.

## The package map

The generator imports each message class from your generated protobuf code.
The map has one entry for each proto package and language, in `monorm.toml`, `[tool.monorm]` in `pyproject.toml`, or `"monorm"` in `package.json`:

```toml
[proto.python]
"acme" = "acme.note_pb2"
"acme.billing" = "acme.billing.invoice_pb2"

[proto.typescript]
"acme" = "./gen/acme/note_pb.js"
```

The longest matching package prefix wins.
A message whose package has no entry is a schema error that names the package.
Only `monorm generate` reads the settings. The generated module holds the import.

## Codecs

The adapter finds the codec from the API of the class:

| Library | Detected by | JSON |
|---|---|---|
| Google protobuf (Python) | `SerializeToString` and `FromString` | `json_format` |
| protobuf-es v2 (TypeScript) | `toBinary` and `fromBinary` | `toJson` and `fromJson` |
| ts-proto and protobufjs | `encode` and `decode` | `toJSON` and `fromJSON` |

For any other class, register a codec, or the field raises an error that names `proto_codec`:

```python
from monorm_proto import proto_codec

proto_codec(Thing, encode=lambda m: m.dump(), decode=Thing.load, to_json=Thing.to_dict, from_json=Thing.from_dict)
```

TypeScript has `protoCodec(cls, { encode, decode, toJson, fromJson })` in `monorm/extensions/proto`.
A registered codec wins over detection.
The optional peer dependencies are Google `protobuf` (Python extra `monorm[protobuf]`) and `@bufbuild/protobuf` v2 (TypeScript).
The extension loads them on first use, so a project that does not use a proto field does not need them.
ts-proto emits an interface and a plain object, so the adapter cannot check the class of a value at run time with ts-proto.

## Queries

A proto field takes `IS NULL` and `IS NOT NULL` only.
Any other expression on it (`=`, `IN`, an order, a function) raises at build time, because the bytes of equal messages can differ.
Select the whole field to read it: `select(Event.payload)` returns the stored bytes, not a decoded message.
Writes take a message instance. A value of another class raises an error that names the expected message.
Unknown fields in stored bytes are kept or dropped by the protobuf library that you use; the ORM does not decide this.

## Reading rows

A row decodes the field once for each loaded value, in Python and in TypeScript.
A change to the decoded message in place stays on that row object and does not reach the database: assign the message back and save.
`load(reload=True)` replaces the stored bytes, so the next read decodes again.

## Admin

The admin shows the stored bytes as base64 text and saves changes to the other fields of the row.
A form value for the proto field is base64 bytes, which the adapter refuses: the admin answers `422` with the reason.
Viewing and editing a message as JSON through the codec is planned for a later admin job.
