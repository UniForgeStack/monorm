# File fields

A file field is a nullable `Json` column that holds a storage reference.
The ORM uploads `Upload` values before the SQL statement and reads the column back as a `Reference`.
The files live in an [OpenDAL](https://opendal.apache.org/) service (local directory, S3 and others) that the application configures.

**Enable:** nothing to install.
The extension `monorm.file-storage` ships inside the one `monorm` Python wheel (module `monorm_file_storage`) and the one `monorm` npm package (`monorm/extensions/file-storage`).
The storage library is an optional peer: Python `opendal>=0.47,<0.48`, TypeScript `opendal` `^0.49.0`.
Without the package, `monorm generate` rejects `@storage.file` with a namespace error.
See [Installable packages](../contracts/optional-packaging.md#installable-packages).
The host has no file-specific code.
How the extension works: [extensions/file-storage/README.md](../../extensions/file-storage/README.md).

## Schema

```prisma
model Report {
  id   Int   @id @default(autoincrement())
  file Json? @storage.file(storage: "reports")
}
```

`@storage.file(storage:)` is a field attribute.
It has one argument.

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `storage` | string, required | none | The name of the storage. The application registers an OpenDAL operator under this name at run time. |

The pass refuses:

| Case | Error text (after the prefix) |
|---|---|
| The field is not a scalar `Json` field (a list, an enum, another type) | `scalar Json field required` |
| The field is a key, or it has a database default | `file fields cannot be keys or have defaults` |
| The storage name is empty or missing | `nonempty storage identity required` |
| Two file attributes on one field | `duplicate file-field adapter` |
| A generated member (`<field>_open`, `<field>_signed_url`) collides with a member of the model | the collision message of the host |

The prefix is `extension monorm.file-storage: pass monorm.file-storage.fields: <file>:<line>:<column>: @storage.file: `.
The column type is the one of `Json`: `jsonb` on PostgreSQL, `TEXT` on SQLite.
The extension adds no SQL, so a migration shows an ordinary `Json` column.
A `@@proxy.of` view of the model gets the file field of its root.

## Use

A generated module installs the file adapter and exports `configure_file_storage(registry)` (TypeScript: `configureFileStorage(registry)`):

```python
import opendal
from monorm_file_storage import Registry, Upload
from app.models import Report, configure_file_storage

configure_file_storage(Registry({"reports": opendal.AsyncOperator("fs", root="/srv/reports")}))
report = await Report.objects.using(db).insert(file=Upload(data, filename="report.xlsx"))
report.file                                     # monorm_file_storage.Reference
async for chunk in report.file_open(): ...      # streams the stored file
url = await report.file_signed_url(expires_in=300)  # needs a service with presign; `fs` raises CapabilityError
await Report.objects.using(db).filter(Report.id == report.id).update(file=Upload(new_data))
```

```ts
import { Registry, Upload } from "monorm/extensions/file-storage";
import { Operator } from "opendal";
import { Report, configureFileStorage } from "./models.js";

configureFileStorage(new Registry(new Map([["reports", new Operator("fs", { root: "/srv/reports" })]])));
const report = await Report.objects.using(db).insert({ file: new Upload(data, { filename: "report.xlsx" }) });
report.file;                                           // Reference
for await (const chunk of report.fileOpen()) { /* ... */ }
const url = await report.fileSignedUrl({ expiresIn: 300 }); // needs a service with presign; `fs` throws CapabilityError
await Report.objects.using(db).filter(Report.id.eq(report.id)).update({ file: new Upload(newData) });
```

With `monorm.loads` (no generated module), install the adapter yourself:

```python
from monorm_file_storage import FileField
from monorm_file_storage.model import install_model

install_model(Report, {"file": FileField("file", "reports", True)}, registry)  # FileField(name, storage, nullable)
```

```ts
import { FileField } from "monorm/extensions/file-storage";
import { installModel } from "monorm/extensions/file-storage/model";

installModel(Report._meta.Row.prototype, new Map([["file", new FileField("file", "reports", true)]]), registry, Report._meta);
```

Call the install function once for each model, before the first write.

### `Upload`

| Python | TypeScript | Default | Meaning |
|---|---|---|---|
| `Upload(source, ...)` `source` | `new Upload(source, options)` `source` | required | The bytes: `bytes`, `bytearray`, `memoryview`, a binary file object or an async iterator of `bytes` (TypeScript: `Uint8Array` or an async iterable of `Uint8Array`). |
| `filename=` | `options.filename` | none | Metadata only. The ORM never uses it as a path. |
| `content_type=` | `options.contentType` | `application/octet-stream` | Goes to the service. The ORM does not infer it from the filename. |
| `max_size=` | `options.maxSize` | 2^53-1 | Upload limit in bytes. The check runs while the source streams, so memory does not grow with the file size. Over the limit the write raises `AdapterWriteError`. Its cause is an `UploadError`, whose cause is `SizeLimitError`. |
| none | `options.signal` | none | An `AbortSignal` that cancels the upload. |

Creating an `Upload` makes no I/O call.

### Writes

- `insert`, `insert_many`, `update_many` and `update` accept `Upload`.
  Input and statement checks run before any upload.
- `insert_many` and `update_many` run up to 16 uploads at the same time.
- A filtered `update` runs one upload and writes the same reference to every matching row.
  It uploads one object even when no row matches.
- Upserts, expressions and `insert_many(copy=True)` reject `Upload` before any upload.
- A `Reference` value and `None` write as ordinary JSON, with no upload.
- An application write rule that refuses a write prevents its file upload.

### Reads

- The field loads as a `Reference` (or `None`).
  Reading it makes no storage request.
- `<field>_open()` (TypeScript: `<field>Open()`) streams the object.
  `<field>_signed_url(expires_in=300)` (TypeScript: `<field>SignedUrl({ expiresIn })`) gives a presigned URL for 1 to 604800 seconds.
- A loaded `NULL` raises `MissingFile` before any storage request.
- In Python, a row from `select(...)` is not a model instance. It has no `<field>_open` and no `<field>_signed_url`.
  `FileNotLoaded` is the error of the file field when its snapshot has no value for the field.
- Both methods need a configured registry: without one, `FileFieldError: configure file storage before file operations`.

## Registry

`Registry` maps a storage name to an OpenDAL operator.
The application owns the operator, its credentials and its lifetime.
The ORM creates no operator.

```python
import opendal
from monorm_file_storage import Registry

registry = Registry({"reports": opendal.AsyncOperator("fs", root="/srv/reports")})
stored = await registry.upload("reports", report_bytes, filename="report.xlsx")
# stored is a Reference; the ORM persists stored.to_dict() in the column.
async for chunk in registry.open(stored): ...
url = await registry.signed_url(stored, expires_in=300)  # CapabilityError without presign
```

```ts
import { Registry } from "monorm/extensions/file-storage";
import { Operator } from "opendal";

const registry = new Registry(new Map([["reports", new Operator("fs", { root: "/srv/reports" })]]));
const stored = await registry.upload("reports", reportBytes, { filename: "report.xlsx" });
for await (const chunk of registry.open(stored)) { /* ... */ }
const url = await registry.signedUrl(stored, { expiresIn: 300 });
```

You can upload with the registry first and write the `Reference` yourself.
This gives you full control over the order of the upload and the SQL.

- Keys are random UUID4 hex. A caller filename is metadata, never a path.
- Content type defaults to `application/octet-stream`.
- The service version is read with `stat()` after the write, only when the service reports `read_with_version`.
  It is stored in the reference. `open` passes it to the service. In Python, `signed_url` passes it too. In TypeScript, `signedUrl` does not.
- A failed upload raises `UploadError`. Its `reference` is the candidate reference.
- A name of a storage that the registry does not hold raises `StorageError: unconfigured storage: <name>`.
- Retries, multipart upload and local-file safety belong to OpenDAL.

## Reference wire contract

A reference is JSON in the column:

```json
{"v":1,"storage":"reports","key":"unique-object-key","version":"optional-provider-version","filename":"report.xlsx","content_type":"application/octet-stream","size":42}
```

- Optional properties are omitted, never null.
- Unknown properties and versions fail validation.
- `size` is an integer from 0 to 2^53-1, for exact serialization in both languages.
- A reference holds no credentials, client or expiring URL.
- A storage name must keep pointing at the same object namespace.
  Changing a configured bucket does not migrate old objects.
- A reference gives location, not authorization.
  Authorize a reference that comes from outside input before you attach or read it.

Python: `Reference.to_dict()`, `to_json()`, `from_dict(value)`, `from_json(text)`.
TypeScript: `reference.toJSON()`, `Reference.fromJSON(value)`.
In TypeScript the content type property is `contentType`.
`extensions/file-storage/fixtures/references.json` is the fixture set that both languages test.

## Errors

| Error | When | Fix |
|---|---|---|
| `FileFieldError: configure file storage before preparing uploads` | An `Upload` is written before the registry is configured | Call `configure_file_storage(registry)` once at start |
| `StorageError: unconfigured storage: <name>` | The registry has no operator for the name in the schema | Add the operator under that name |
| `FileNotLoaded` | The file field gets a snapshot with no value for the field | Load the field |
| `MissingFile` | The same call on a `NULL` field | Check for `None` first |
| `FileFieldError: <field> requires storage <name>` | A `Reference` of another storage goes into the field | Use a reference of the storage in the schema |
| `SizeLimitError` | The source is larger than `max_size`. In a write it is the cause of an `UploadError`, which is the cause of an `AdapterWriteError`. `registry.upload` raises it as the cause of `UploadError` too | Raise the limit, or reject the file earlier |
| `ValueError: expires_in must be an integer from 1 to 604800 seconds` | A signed URL with an expiry outside the range | Pass 1 to 604800 |
| `CapabilityError` | The service does not support signed URLs | Use `open`, or a service with presign |
| `UploadError` | The upload to the service failed | Read `error.reference` and the cause; the object may exist at that key |
| `AdapterWriteError` | An upload or the SQL statement failed in a write | See "Failed writes" |
| `AdapterWriteCancelled` (Python) | The write was cancelled | See "Failed writes". It is an `asyncio.CancelledError` |

## Failed writes

A failed or cancelled upload is final: the ORM does not replay a consumed source.
A failed SQL statement keeps the uploaded files.
The ORM never deletes a stored file.

`AdapterWriteError` holds:

- `operation`: the prepared write, with the uploads that finished.
- `stored`: the stored references. A single write keys them by field. A batch write keys them by `(row index, field)` in Python and by `"<row index>.<field>"` in TypeScript.
- The original failure as the cause. An `UploadError` cause holds the candidate reference.

After every upload finished, `await error.operation.execute()` runs the SQL statement again with no new upload.
No retry loop runs by itself.

The ORM does not delete after a statement success either.
An outer transaction rollback, a savepoint rollback or an unclear commit leaves the objects in place.
The application chooses the cleanup and the retention.
There is no object registry, no cleanup queue and no lifecycle worker.

## Limits

- No delete: replacing or removing a row never removes the object.
- One storage name for each field, set in the schema.
- `Upload` needs the generated module or `install_model`. A model with no adapter treats a file field as plain `Json`.
- Presign needs a service that supports it.
- `monorm pull` reads the column as an ordinary field (`String` on SQLite) and writes no `@storage.file`.

The design of the adapter: [extensions/file-storage/integration/README.md](../../extensions/file-storage/integration/README.md).
