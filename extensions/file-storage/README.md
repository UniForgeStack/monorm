# monorm.file-storage

Stores a file in an [OpenDAL](https://opendal.apache.org/) service and keeps a reference to it in a `Json` column.
It adds the field attribute `@storage.file(storage: "<name>")`.

## Install

Nothing to install.
The extension ships inside the one `monorm` wheel (Python module `monorm_file_storage`) and the one `monorm` npm package (`monorm/extensions/file-storage`).
The Rust crate is `monorm-file-storage` (`publish = false`).
The storage library `opendal` is an optional peer: Python `opendal>=0.47,<0.48`, TypeScript `opendal` `^0.49.0`.
The extension loads it on first use.

## Smallest example

```prisma
model Report {
  id   Int   @id @default(autoincrement())
  file Json? @storage.file(storage: "reports")
}
```

Run `monorm generate`.
The generated module installs the adapter and exports `configure_file_storage` (TypeScript: `configureFileStorage`).

```python
import opendal
from monorm_file_storage import Registry, Upload
from app.models import Report, configure_file_storage

configure_file_storage(Registry({"reports": opendal.AsyncOperator("fs", root="/srv/reports")}))
report = await Report.objects.using(db).insert(file=Upload(data, filename="report.xlsx"))
print(report.file.key)  # a Reference: the ORM wrote the bytes to the service before the SQL statement
```

```ts
import { Registry, Upload } from "monorm/extensions/file-storage";
import { Operator } from "opendal";
import { Report, configureFileStorage } from "./models.js";

configureFileStorage(new Registry(new Map([["reports", new Operator("fs", { root: "/srv/reports" })]])));
const report = await Report.objects.using(db).insert({ file: new Upload(data, { filename: "report.xlsx" }) });
console.log(report.file?.key);
```

## How it works

**Pass.** `monorm.file-storage.fields` runs in the `behavior` phase.
Its effect is `behavior.field_adapters.file-storage`.
For each `@storage.file` the pass writes one field adapter record: adapter `file-storage.reference.v1` and option `storage` (the storage name).
A view of the model (`@@proxy.of`) gets a copy of the record.

**What the pass checks.**
It refuses a field that is not a scalar `Json` field, a key, a field with a database default, an empty storage name, a second adapter on the same field, and a generated member name (`<field>_open`, `<field>_signed_url`) that collides with a member of the model.
The user doc lists each message.

**SQL.** None.
The column is an ordinary `Json` column (`jsonb` on PostgreSQL, `TEXT` on SQLite).

**Runtime.** The adapter checks an `Upload` or a `Reference` without I/O, uploads each `Upload` before the SQL statement, and reads the column back as a `Reference`.
A failed SQL statement keeps the uploaded files.
The ORM never deletes a stored file.

**Rust and bindings.** The Rust crate checks the schema and writes the record.
It has no host code and no storage code.
The `Reference` codec, `Upload`, `Registry` and the adapter are in the bindings: `integration/python/src/monorm_file_storage` and `integration/typescript/src`.
The generated models import `install` from `monorm_file_storage.model` (TypeScript: `monorm/extensions/file-storage/model`).

**Other extensions.** `monorm.proxy` views get the record of their root.

## Layout

```
extensions/file-storage/
  Cargo.toml                          crate monorm-file-storage
  monorm-extension.toml               manifest: one pass, one attribute, two field adapters
  src/lib.rs                          the pass and its unit tests
  integration/python/src/             Reference, Upload, Registry and the adapter
  integration/python/tests/           test_decoder.py, test_prepared.py, test_storage.py
  integration/typescript/src/         the same in TypeScript
  integration/typescript/test/        decoder, prepared and storage tests
  integration/tests/                  native_python.py and native_node.mjs, end to end
  examples/                           generated_report.py and generated-report.ts
  fixtures/references.json            Reference fixtures shared by both languages
```

The extension has no `tests/public`.

```bash
cargo test -q -p monorm-file-storage
python -m unittest discover -s extensions/file-storage/integration/python/tests
scripts/check-extension-packages.sh   # the end-to-end tests, in fresh Python, Node and Bun consumers, on SQLite and PostgreSQL
```

[integration/README.md](integration/README.md) explains the adapter design.

## Why OpenDAL

A local benchmark (measured, one run per library, median of 3 rounds, RustFS on localhost) compared our earlier own adapters with OpenDAL and, in Python, `obstore` (the Python binding of Rust `object_store`).
Treat differences under about 20% as noise.

| metric | own adapters (aiobotocore / AWS SDK v3) | opendal | obstore (Python only) |
|---|---|---|---|
| Python small put (ops/s) | 347 | 852 | 973 |
| Python 64 MiB put (MiB/s) | 142 | 192 | 374 |
| Python presign (ops/s) | 13913 | 36631 | 40338 |
| Node small put (ops/s) | 320 | 1009 | n/a |
| Node presign (ops/s) | 7873 | 72199 | n/a |

- Localhost has almost no latency, so client CPU dominates the small-object numbers. On real S3 (about 20-50 ms per request, estimated) that gap is smaller. Presign uses only CPU, so its gap stays.
- The 64 MiB put gain of `obstore` comes from concurrent parts (158 MiB/s at one part, 350 MiB/s at 12), not from a faster client.
- Peak memory was not measured.

**Plan:** `object_store` was faster than OpenDAL in Python in this benchmark but has no Node binding.
We use OpenDAL in both languages. We will consider `object_store` when it has a Node binding.

## Limits

- The ORM has no delete. A replaced row, a rollback or a failed statement leaves the old object. The application owns cleanup.
- A reference gives location, not authorization. Authorize a reference that comes from outside input before you attach or read it.
- A filtered `update` uploads one object and writes the same reference to every matching row.
- Upserts, expressions and `insert_many(copy=True)` reject `Upload`.
- A storage name must keep pointing at the same object namespace. Changing a configured bucket does not migrate old objects.
- A signed URL needs a service that supports presign (`CapabilityError` otherwise).
- A schema error starts with `extension monorm.file-storage: pass monorm.file-storage.fields:`.

## More

Usage, the `Registry` API, the reference wire contract, upload errors and recovery: [docs/extensions/file-storage.md](../../docs/extensions/file-storage.md).
