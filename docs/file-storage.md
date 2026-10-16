# File fields

A file field is a nullable `Json` column that holds a storage reference.
The ORM uploads `Upload` values before the SQL statement and reads the column back as a `Reference`.
The files live in a storage backend (local directory or S3) from the independent `extensions/file-storage/` packages.

**Enable:** select the `orm-file-storage-extension` crate (`extensions/file-storage`) in your build.
See [build-time-extensions.md](build-time-extensions.md#select-feature-extensions).
Install the binding helpers too: Python `orm-file-storage` (`extensions/file-storage/integration/python`) and `orm-storage` (`extensions/file-storage/python`);
TypeScript `@orm/file-storage` (`extensions/file-storage/integration/typescript`) and `@orm/storage` (`extensions/file-storage/typescript`).

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
from orm_storage import LocalStorage, Registry
from orm_file_storage import Upload
from app.models import Report, configure_file_storage

configure_file_storage(Registry({"reports": LocalStorage("reports", "/srv/reports")}))
report = await Report.objects.insert(file=Upload(data, filename="report.xlsx"))
report.file                                     # orm_storage.Reference
async for chunk in report.file_open(): ...      # streams the stored file
await Report.objects.filter(Report.id == report.id).update(file=Upload(new_data))
```

With `orm.loads` (no generated module), install the adapter yourself:
`orm_file_storage.model.install_model(Report, {"file": FileField("file", "reports", True)}, registry)`.

- `insert` and a unique-row `update` (an equality on the primary key or a unique field) accept `Upload`.
  `insert_many`, a filtered `update` of many rows, upserts and expressions reject `Upload` before any upload.
- A failed SQL statement keeps the uploaded files. The error (`FileWriteError`) holds the prepared `operation`; retry with `operation.execute(...)`. The ORM never deletes a stored file.
- `Reference` values and `None` write as ordinary JSON.

The storage packages, S3 setup, recovery and the TypeScript API are in [extensions/file-storage/README.md](../extensions/file-storage/README.md) and [extensions/file-storage/integration/README.md](../extensions/file-storage/integration/README.md).
