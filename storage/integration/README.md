# File-field binding preparation

`python/` and `typescript/` contain separately packaged selected binding helpers.
They depend on the independent storage package. They do not import ORM internals,
provider SDKs, or install a runtime native callback registry. They use only the
public ORM hooks: Python `orm.hooks` (`prepare_insert`, `prepare_update`,
`decode_field`) and TypeScript `QuerySet.prepareInsert`, `QuerySet.prepareUpdate`
and `ModelMeta.addRowDecoder`. The engine proves a unique-row update.

`install_model` / `installModel` decode loaded file fields to `Reference` and route
`Upload` writes through `PreparedFileWrite`. Annotated file fields need a native
artifact built with the `file-storage` feature; selecting `../orm-extension/` in
`orm-extension-build` turns it on. Installing these helpers alone does not.

Declare a file field (see also [docs/file-storage.md](../../docs/file-storage.md)):

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

## Prepared operation

```python
from orm_file_storage import FileField, PreparedFileWrite, Upload

operation = PreparedFileWrite(
    {"file": Upload(report_bytes, filename="report.xlsx")},
    {"file": FileField("file", "reports")},
    storage_registry,
)
report = await operation.execute(lambda values: Report.objects.insert(**values))
# operation.references remains inspectable after the enclosing transaction fails.
```

```typescript
import { FileField, PreparedFileWrite, Upload } from "@orm/file-storage";

const operation = new PreparedFileWrite(
  { file: new Upload(reportBytes, { filename: "report.xlsx" }) },
  new Map([["file", new FileField("file", "reports")]]),
  storageRegistry,
);
const report = await operation.execute(values => Report.objects.insert(values));
```

These examples persist ordinary JSON explicitly. With `install_model` /
`installModel`, `insert` and unique-row `update` also take `Upload` directly
(see "Selected native model integration"). Deferred Upload construction makes no I/O calls. All input and
write-shape checks precede uploads. The trusted adapter must prove a unique-row
update before passing `unique_update`; an arbitrary filter is insufficient.
Bulk, multi-row update, upsert, conflict-ignore and expression shapes reject Upload
before provider I/O. Existing Reference/null values retain normal SQL semantics.

`.prepare()` uploads once and returns JSON inputs; `.execute(statement)` passes
those inputs to an awaited statement. A failed/cancelled preparation is terminal,
so consumed sources are never automatically replayed. Completed preparations can
be reused for explicit SQL retries without another upload. Concurrent execution
of the same operation is rejected. There is no automatic SQL retry loop.

`references` retains completed uploaded references, including when a later upload
fails. `recovery` retains S3 incomplete/uncertain upload details. FileWriteError
retains `operation` and original failure as cause. Python cancellation remains an
asyncio.CancelledError subclass (FileWriteCancelled), also retaining the operation.
TypeScript failures/cancellation use FileWriteError with the original AbortError
or statement failure in cause.

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
(`storage/integration/tests/native_python.py`, `native_node.mjs`).
Limitation: the native artifact's `adapters` list is empty, so packaging does not
select these modules by itself; the application (or its generated module) calls
`install_model` / `installModel` once at initialization.
