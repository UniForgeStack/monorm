# File fields

A file field is a nullable `Json` column that holds a storage reference.
The ORM uploads `Upload` values before the SQL statement and reads the column back as a `Reference`.
The files live in an [OpenDAL](https://opendal.apache.org/) service (local directory, S3 and others) that the application configures.

**Enable:** install the package `orm-file-storage` (Python) or `@orm/file-storage` (Node).
It has the `orm.file-storage` extension and the runtime module, and it installs `opendal`.
Without the package, `orm generate` rejects `@storage.file` with a namespace error.
See [Installable packages](../contracts/optional-packaging.md#installable-packages).
The extension requires the `async-field-adapter.v1` capability; the host has no file-specific code.

## Schema

```prisma
model Report {
  id   Int   @id @default(autoincrement())
  file Json? @storage.file(storage: "reports")
}
```

`@storage.file(storage:)` names a storage that the application registers at run time.
The pass rejects a non-JSON field, a key field, a field with a database default, and generated member names that collide.

## Use

A generated module installs the file adapter and exports `configure_file_storage(registry)` (TypeScript: `configureFileStorage(registry)`):

```python
import opendal
from orm_file_storage import Registry, Upload
from app.models import Report, configure_file_storage

configure_file_storage(Registry({"reports": opendal.AsyncOperator("fs", root="/srv/reports")}))
report = await Report.objects.insert(file=Upload(data, filename="report.xlsx"))
report.file                                     # orm_file_storage.Reference
async for chunk in report.file_open(): ...      # streams the stored file
await Report.objects.filter(Report.id == report.id).update(file=Upload(new_data))
```

With `orm.loads` (no generated module), install the adapter yourself:
`orm_file_storage.model.install_model(Report, {"file": FileField("file", "reports", True)}, registry)`.

- `insert`, `insert_many`, `update_many` and `update` accept `Upload`.
  `insert_many` and `update_many` run up to 16 uploads at the same time.
  A filtered `update` runs one upload and writes the same reference to every matching row. It uploads one object even when no row matches.
  Upserts, expressions and `insert_many(copy=True)` reject `Upload` before any upload.
  In a batch, `error.stored` names a value `(row index, field)` in Python and `"<row index>.<field>"` in TypeScript.
- A failed SQL statement keeps the uploaded files. The error (`AdapterWriteError`) holds the prepared `operation`, and `error.stored` the stored references; retry with `operation.execute()`. The ORM never deletes a stored file.
- `Reference` values and `None` write as ordinary JSON.

The reference format, upload errors, recovery and the TypeScript API are in [extensions/file-storage/README.md](../../extensions/file-storage/README.md) and [extensions/file-storage/integration/README.md](../../extensions/file-storage/integration/README.md).
