# monorm.proto

Stores one protobuf message in a `Bytes` column and gives your own message class on read and write.
It adds the field attribute `@proto.message("<full name>")`.

## Install

Nothing to install.
The extension ships inside the one `monorm` wheel (Python module `monorm_proto`) and the one `monorm` npm package (`monorm/extensions/proto`).
The Rust crate is `monorm-proto` (`publish = false`).
The protobuf library of your messages is an optional peer: Python extra `monorm[protobuf]` (`protobuf>=5`), TypeScript `@bufbuild/protobuf` `^2.0.0`.
The extension loads the peer on first use.

## Smallest example

```prisma
model Event {
  id      Int    @id @default(autoincrement())
  payload Bytes? @proto.message("acme.Note")
}
```

The generator needs to know where the message class lives.
Add the package map to `monorm.toml`, then run `python -m monorm generate`:

```toml
[proto.python]
"acme" = "acme.note_pb2"

[proto.typescript]
"acme" = "./gen/acme/note_pb.js"
```

```python
from acme.note_pb2 import Note
from models import Event  # written by `monorm generate`

await Event.objects.using(db).insert(payload=Note(title="first"))
event = await Event.objects.using(db).get(Event.id == 1)
print(event.payload.title)
```

```ts
import { create } from "@bufbuild/protobuf";
import { NoteSchema } from "./gen/acme/note_pb.js";
import { Event } from "./models.js"; // written by `npx monorm --schema schema.prisma generate typescript -o models.ts`

await Event.objects.using(db).insert({ payload: create(NoteSchema, { title: "first" }) });
const event = await Event.objects.using(db).get(Event.id.eq(1));
console.log(event.payload?.title);
```

## How it works

**Pass.** `monorm.proto.fields` runs in the `behavior` phase.
Its effect is `behavior.field_adapters.proto`.
For each `@proto.message` the pass writes one field adapter record: adapter `proto.message.v1`, option `message` (the full name), and `null_only: true`.
A view of the model (`@@proxy.of`) gets a copy of the record.

**What the pass checks.**
It refuses a field that is not `Bytes`, a list, a field inside a nested type, a key, an `@index` or `@unique`, a `@check`, a default, a key of a relation, a bad message name, and a second adapter on the same field.
The reason for the index, unique and check rules is that two equal messages can have different bytes.
The user doc lists each message.

**SQL.** None.
The column is an ordinary `Bytes` column (`bytea` on PostgreSQL, `BLOB` on SQLite).
`null_only: true` tells the query builder that the field accepts `IS NULL` and `IS NOT NULL` only.

**Runtime.** The adapter encodes a message before the SQL statement and decodes the bytes on read.
It finds the codec from the API of the class (Google protobuf, protobuf-es v2, ts-proto, protobufjs), or from a codec that you register with `proto_codec` (Python) or `protoCodec` (TypeScript).

**Rust and bindings.** The Rust crate checks the schema and writes the record.
The codec work is in the bindings: `integration/python/src/monorm_proto/model.py` and `integration/typescript/src/model.ts`.
The generated models import your message class, from the package map in `monorm.toml`.

**Other extensions.** `monorm.proxy` views get the record of their root.
The admin shows the stored bytes as base64 and refuses a form value for the field.

## Layout

```
extensions/proto/
  Cargo.toml                 crate monorm-proto
  monorm-extension.toml      manifest: one pass, one attribute, two field adapters
  src/lib.rs                 the pass and its unit tests
  integration/python/src/monorm_proto/     codecs, the adapter and proto_codec
  integration/typescript/src/              codecs, the adapter and protoCodec
  fixtures/                  .proto files and the script that builds the generated test classes
```

The extension has no `tests/public`.
The end-to-end tests are in `languages/python/tests/test_proto_field.py` and `languages/js/test/proto-field.test.ts` and `proto-field-sqlite.test.ts`.

```bash
cargo test -q -p monorm-proto
python -m pytest -q languages/python/tests/test_proto_field.py
(cd languages/js && npm test)   # runs every test; the files are proto-field.test.ts and proto-field-sqlite.test.ts
extensions/proto/fixtures/generate.sh   # rebuilds the committed generated test classes
```

## Limits

- Only `IS NULL` and `IS NOT NULL` work in a filter. Any other expression raises at build time.
- No key, index, unique, check, default or list.
- A change to a decoded message in place does not reach the database. Assign it back.
- The admin does not edit the field.
- A class with no known protobuf API needs a registered codec.
- A schema error starts with `extension monorm.proto: pass monorm.proto.fields:`.

## More

Usage, the package map, the codecs and every error: [docs/extensions/proto.md](../../docs/extensions/proto.md).
