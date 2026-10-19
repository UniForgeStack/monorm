# File-field binding preparation

`python/` and `typescript/` contain separately packaged selected binding helpers.
They depend on `opendal`, not on the project's storage library. They keep the Reference
codec v1, `Upload`, the `Registry` (storage name to OpenDAL operator), `max_size` while
streaming and the upload error with the candidate reference. They do not import ORM
internals or install a runtime native callback registry. They use only the
public async field adapter API: Python `orm.adapters.register_field_adapter`
and TypeScript `registerFieldAdapter` from `orm`. The extension requires the
`async-field-adapter.v1` capability.

`install_model` / `installModel` register one `FileAdapter` per file field: its
`validate` checks an `Upload` or a `Reference` without I/O, `prepare` uploads, and
`decode` turns a loaded field into a `Reference`. Annotated file fields need a native
artifact that selects this extension in `orm-extension-build` (the manifest requires
`async-field-adapter.v1`). Installing these helpers alone does not.

Declare a file field (see also [docs/extensions/file-storage.md](../../../docs/extensions/file-storage.md)):

```prisma
model Report {
  id Int @id @default(autoincrement())
  file Json? @storage.file(storage: "reports")
}
```

The compiler contributes a fixed `file-storage.reference.v1` field adapter plus
logical storage identity/reference contract metadata. It rejects non-scalar JSON,
database-defaulted/key fields and generated member collisions. Credentials and
configured providers belong to the application's Registry.

## Write failures

`insert` and a unique-row `update` take `Upload` directly. Deferred Upload
construction makes no I/O calls. All input and write-shape checks precede uploads.
Bulk, multi-row update, upsert, conflict-ignore and expression shapes reject Upload
before provider I/O. Existing Reference/null values retain normal SQL semantics.

A failed or cancelled upload is terminal, so consumed sources are never
automatically replayed. A completed upload is reused for explicit SQL retries
with `operation.execute()`, without another upload. There is no automatic SQL
retry loop.

`AdapterWriteError` retains `operation` (`operation.stored` holds the completed
references, also when a later upload fails) and the original failure as cause; an
`UploadError` cause carries `recovery` with the candidate reference and the storage error.
Python cancellation is `AdapterWriteCancelled`, an `asyncio.CancelledError`
subclass that also retains the operation.

The operation remains caller-owned after statement success: outer transaction
failure, rollback, savepoint rollback or uncertain acknowledgement never triggers
physical deletion. No object ownership registry, deletion compensation, savepoint
tracking, cleanup outbox or lifecycle worker is implied. Applications explicitly
choose recovery and retention. Preparing/uploading then writing ordinary JSON
remains available for complete application control.

FileField's explicit signed URL/open operations distinguish omitted public fields
from loaded NULL, raising FileNotLoaded/MissingFile before provider access. Decode
and Reference access make no storage requests. FileField helper errors must map to
the selected ORM's public missing/unloaded error classes in final integration.

## Selected native model integration

The native `file-storage` Cargo feature includes the durable reference validator,
resolves logical JSON field positions at definition, and validates ordinary
insert/update/bulk-update references without provider calls. Both bindings expose
selected insert preflight that converts and plans all inputs without executing
SQL. No storage dependency or prepared file layout exists in disabled native core.

Selected code generation uses the fixed `file-storage.reference.v1` adapter:
Python imports `orm_file_storage.model.install_model`; TypeScript imports
`installModel` from `@orm/file-storage/model`. Generated modules expose
`configure_file_storage(registry)` / `configureFileStorage(registry)` for the
application's storage Registry. Methods delegate signing/opening explicitly;
reference-only writes normalize without uploading. Generated single insert and
unique-row update wrappers prepare Upload outside native SQL execution and retain
completed references after SQL failure. Unique updates require a direct equality
on a prepared primary-key/unique field, possibly within AND; arbitrary OR, bulk,
conflict and expression uploads reject before I/O.

Python retains the prepared operation on the selected statement's `operation`.
TypeScript's selected query exposes `prepareFileInsert(values)` returning
`{ operation, execute }` when the caller needs recovery after a successful
statement followed by outer rollback. Convenience errors in both bindings retain
the operation. Retry explicitly with its `.execute()`; no upload replay or
physical cleanup is automatic.

Dynamic hosts use the same setup function and fixed decoder. For TypeScript pass
`meta` as the fourth installModel argument to install selected query statements;
Python install_model installs statements when passed an ORM model class. The
host must stage this setup on candidate models before publishing its definition.
Selected descriptor maps use Python field names / TypeScript camelCase names.

Models with `install_model` / `installModel` read file fields as `Reference`
(`extensions/file-storage/integration/tests/native_python.py`, `native_node.mjs`).
Limitation: the native artifact's `adapters` list is empty, so packaging does not
select these modules by itself; the application (or its generated module) calls
`install_model` / `installModel` once at initialization.
