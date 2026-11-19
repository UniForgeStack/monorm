# Protobuf fields

A proto field is a `Bytes` column that holds one encoded protobuf message.
The pass adds one attribute, `@proto.message("<full name>")`, and no SQL: the column is an ordinary `Bytes` column (`bytea` on PostgreSQL, `BLOB` on SQLite).
The field may be `Bytes` or `Bytes?`. The adapter encodes a message instance, or `None` for a nullable column.
The generated model types the field as your own message class.
The ORM encodes a message before the SQL statement and decodes the stored bytes when you read the field.

**Enable:** nothing to install. The extension `monorm.proto` is built into the `monorm` Python wheel (runtime module `monorm_proto`) and the `monorm` npm package (`monorm/extensions/proto`).
Its adapter is `proto.message.v1`. The Python module is `monorm_proto.model`, the TypeScript module is `monorm/extensions/proto/model`.
Without the package, `monorm generate` rejects `@proto.message` with a namespace error.
The host has no protobuf code.
The protobuf library of your messages is an optional peer: Python `monorm[protobuf]` (`protobuf>=5`), TypeScript `@bufbuild/protobuf` `^2.0.0`.
How it works, the layout and the tests: [`extensions/proto/README.md`](../../extensions/proto/README.md).

## Schema

```prisma
model Event {
  id      Int    @id @default(autoincrement())
  payload Bytes? @proto.message("acme.Note")
}
```

`@proto.message("<full name>")` has one required positional argument and no other argument.
It takes the full name of the message: at least two identifiers with dots, a package and a name (`acme.Note`, `acme.Outer.Inner` for a nested message).
Each identifier starts with a letter or `_` and has only letters, digits and `_`.
A view (`@@proxy.of`) of the model gets the same field and the same adapter.

The schema load refuses a proto field in these cases:

| Cause | Message |
|---|---|
| The field is not `Bytes` | `a Bytes field is required` |
| A list (`Bytes[]`) | The host refuses first: `Bytes[] is not supported; use a Json array of base64 text or a related model` |
| A field inside a nested `type` | `a proto field inside a nested type is not supported` |
| `@id` or `@@id` names it | `a proto field cannot be a key` |
| `@unique`, `@index`, or an index that names it (also in `include`) | `a proto field cannot have @index or @unique: the encoding of a message is not canonical` |
| `@@unique` or `@@check` that names it | `a proto field cannot have @unique or @check` |
| `@check` on the field | `a proto field cannot have @check` |
| A default | `a proto field cannot have a default`. A literal default on `Bytes` is refused by the host first: `a Bytes field takes no literal default` |
| The field is a key of a relation | `a proto field cannot be a key of a relation` |
| A name such as `Note`, `acme.` or `acme..Note` | `the full message name is required, for example "pkg.Message"` |
| Two `@proto.message` on one field | `duplicate field adapter` |

The reason for the index, unique and check rules is that the encoding of a message is not canonical: two equal messages can have different bytes.
Each message has the prefix `extension monorm.proto: pass monorm.proto.fields: <file>:<line>:<column>: @proto.message:`.

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
A message whose package has no entry is an error of `monorm generate`: ``Event.payload: acme.Note has no entry in `proto.python`; add the module of its package`` (TypeScript: `proto.typescript`).
Only `monorm generate` reads the settings. The generated module holds the import.

## Codecs

The adapter finds the codec from the API of the class:

| Library | Detected by | JSON |
|---|---|---|
| Google protobuf (Python) | `SerializeToString` and `FromString` | `json_format` |
| protobuf-es v2 (TypeScript) | the exported `<Name>Schema` that has `typeName` | `toBinary`, `fromBinary`, `toJson` and `fromJson` of `@bufbuild/protobuf` |
| ts-proto and protobufjs | `encode(message).finish()` and `decode` on the exported value | `toJSON` and `fromJSON` |

For any other class, register a codec, or the field raises an error that names `proto_codec` (Python) or `protoCodec` (TypeScript):

```python
from monorm_proto import proto_codec

proto_codec(Thing, encode=lambda m: m.dump(), decode=Thing.load, to_json=Thing.to_dict, from_json=Thing.from_dict)
```

TypeScript has `protoCodec(target, { encode, decode, toJson, fromJson, is })` in `monorm/extensions/proto`. `toJson`, `fromJson` and `is` are optional. `target` is the exported value of the message, or its `<Name>Schema`.
A registered codec wins over detection.
Without a codec the error is `no codec for <symbol>: it has no known protobuf API; register one with protoCodec(target, { encode, decode })` in TypeScript, and `no codec for <module>.<Class>: it has no known protobuf API; register one with monorm_proto.proto_codec(cls, encode, decode)` in Python.
If `@bufbuild/protobuf` is not installed, the TypeScript message adds `; the package @bufbuild/protobuf is not installed`.
The optional peer dependencies are Google `protobuf` (Python extra `monorm[protobuf]`) and `@bufbuild/protobuf` v2 (TypeScript).
The extension loads them on first use, so a project that does not use a proto field does not need them.
ts-proto emits an interface and a plain object, so the adapter cannot check the class of a value at run time with ts-proto.

## Use

Python:

```python
from acme.note_pb2 import Note
from models import Event  # written by `python -m monorm generate`

event = await Event.objects.using(db).insert(payload=Note(title="first"))
event.payload.title                                          # "first": a Note, not bytes
await Event.objects.using(db).insert(payload=None)           # a nullable column takes None
rows = await Event.objects.using(db).filter(Event.payload.is_null()).all()
```

TypeScript (`npx monorm --schema schema.prisma generate typescript -o models.ts`):

```ts
import { create } from "@bufbuild/protobuf";
import { NoteSchema } from "./gen/acme/note_pb.js";
import { Event } from "./models.js";

const event = await Event.objects.using(db).insert({ payload: create(NoteSchema, { title: "first" }) });
event.payload?.title;                                        // "first"
```

A `Bytes` field that is not nullable needs a message on insert, like any required field.
A value that is not an instance of the message class raises `ProtoError: Event.payload takes acme.Note, not str` (Python; the TypeScript text names the received type).
Without the generated module, `monorm.loads` gives the plain `Bytes` field: the adapter is installed by the generated code.

The migration is that of a `Bytes` column (PostgreSQL, from `sqlmigrate`):

```sql
CREATE TABLE "event" (
    "id" integer GENERATED BY DEFAULT AS IDENTITY NOT NULL,
    "payload" bytea,
    CONSTRAINT "event_pkey" PRIMARY KEY ("id")
);
```

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

The admin shows a proto field as JSON and edits it as JSON, through the JSON functions of the codec.
The detail page and the list show the JSON of the message. The form has a monospace box for the JSON.
The host parses the JSON, builds the message and stores its bytes.
Python uses `to_json` and `from_json` of the codec, and TypeScript uses `toJson` and `fromJson`.

- Bad JSON, a field that the message does not have and a value of the wrong type answer `422` with the name of the field. They are never a `500`.
- A codec with no JSON functions shows `N bytes` on the detail, the form and the list. The form field is read only, and a request that sets it answers `422`.
- A stored value that does not decode shows as `N bytes`. Another column of that row saves, and the row can be deleted. A JSON value for the field replaces the bad bytes.
- The version check (`409`) compares the stored bytes, so a change of the message by another user is a conflict.
- A JSON key is sorted. The admin does not keep the order of the keys of the form.
- The admin refuses an edit that would drop data: the host encodes the stored message again through its JSON view, and if the bytes change, the save answers `422` on the field with `the stored value has fields this version cannot show; edit it with a newer version`. An unknown field of a newer schema is the usual cause. Google `protobuf` and protobuf-es keep unknown fields, so they trigger this check. ts-proto and protobufjs drop unknown fields on decode, so they never do.
- The check compares the bytes that the codec encodes for the message and for its JSON round trip. A codec that encodes the entries of a map in an unstable order could refuse a save that has no unknown field. Google `protobuf` did not differ in a test with a six-key map.
- A message with `fromJSON` of ts-proto is lenient: `{"titel": "x"}` saves an empty message. protobuf-es and Google `protobuf` are strict and answer `422` for an unknown key. A typo reaches the database only with a lenient codec.
- A model that the admin only shows as an inline has no `views` setting. Its proto field shows as base64 and a form value answers `422`, as before.
- A sort on a proto column orders the stored bytes, which has no meaning for the user.
- The `json` widget now pretty-prints every `json` field of every app, not only a proto field.

A custom field adapter turns this on with three optional hooks:

| Python (`AdminFieldView`) | TypeScript | Meaning |
|---|---|---|
| `admin_mode()` | `adminMode()` | `"json"` (show and edit the JSON) or `"text"` (show `N bytes`, refuse an edit). |
| `admin_to_view(stored)` | `adminToView(stored)` | The JSON of the stored bytes. It raises an error when the bytes do not decode. |
| `admin_from_view(value)` | `adminFromView(value)` | The field value that a JSON value stands for. It raises a `FieldAdapterError` for a bad value. |

The adapter of a field with `admin_mode` makes the resource use `host_behavior`: the host function `admin.view` converts the cells, and `admin.write` writes the form.
Only a `Bytes` field can have a view. See [the admin](../apps/admin.md#fields-with-a-view) for the settings.

## Limits

- Only `IS NULL` and `IS NOT NULL` work in a filter.
- A proto field has no key, index, unique, check, default or list.
- A change to a decoded message in place does not reach the database. Assign the message back.
- `monorm pull` reads the column as a plain `Bytes` field and writes no `@proto.message`.
- The admin edits the field as JSON only when the codec has JSON functions.
- A message class with no known protobuf API needs a registered codec.
