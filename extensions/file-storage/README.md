# File-storage package

The `orm.file-storage` package adds `@storage.file(storage: "name")` to a `Json` field.
The ORM stores a reference (JSON) in the column and the application keeps the bytes in an [OpenDAL](https://opendal.apache.org/) service.
The extension has two parts:

- `orm-extension.toml` and `src/`: the Rust crate `orm-file-storage`, a `cdylib` with the ABI macro.
  It validates the schema and contributes the field adapter metadata.
  It has no host code.
- `integration/python` and `integration/typescript`: the runtime sources.
  They hold the `Reference` codec v1, `Upload`, `Registry`, and the field adapter that registers with the public async field adapter API.

`orm-extension-package` builds them into one wheel `orm-file-storage` and one npm package `@orm/file-storage`; both use `opendal`.
Install the package; there is no crate to select.

See [docs/extensions/file-storage.md](../../docs/extensions/file-storage.md) for use, and [integration/README.md](integration/README.md) for write failures and recovery.

## Registry

`Registry` maps a storage name to an OpenDAL operator.
The application owns the operator, its credentials and its lifetime.

```python
import opendal
from orm_file_storage import Registry

registry = Registry({"reports": opendal.AsyncOperator("fs", root="/srv/reports")})
stored = await registry.upload("reports", report_bytes, filename="report.xlsx")
# stored is a Reference; the ORM persists stored.to_dict() in the column.
async for chunk in registry.open(stored): ...
url = await registry.signed_url(stored, expires_in=300)  # CapabilityError without presign
```

```typescript
import { Registry } from "@orm/file-storage";
import { Operator } from "opendal";

const registry = new Registry(new Map([["reports", new Operator("fs", { root: "/srv/reports" })]]));
const stored = await registry.upload("reports", reportBytes, { filename: "report.xlsx" });
```

- Keys are random UUID4 hex. Caller filenames are metadata, never paths.
- Content type defaults to `application/octet-stream` and goes to the service. It is not inferred from a filename.
- `max_size` / `maxSize` applies while the source streams. Memory does not scale with the file size.
- The service version is read with `stat()` after the write, only when the service reports `read_with_version`. It is stored in the reference and passed to `open` and to the signed URL (Python only).
- A failed upload raises `UploadError`. Its `reference` is the candidate reference. The extension has no delete: the application decides on cleanup.
- Retries, multipart and local-file safety belong to OpenDAL.

## Durable wire contract

```json
{"v":1,"storage":"reports","key":"unique-object-key","version":"optional-provider-version","filename":"report.xlsx","content_type":"application/octet-stream","size":42}
```

Optional properties are omitted, never null. Unknown properties and versions fail validation.
`size` is an integer from 0 to 2^53-1, for exact cross-language serialization.
A reference holds no credentials, client or expiring URL.
A storage name must keep pointing at the same object namespace: changing a configured bucket does not migrate old objects.
A reference gives location, not authorization. Authorize a reference from outside input before you attach or read it.
`fixtures/references.json` is the shared cross-language fixture set.

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

## Tests

- `cargo test -p orm-file-storage` tests the extension crate.
- `scripts/install-extension-packages.sh` builds and installs the package.
  Then run the Python unit tests with `python -m unittest discover -s extensions/file-storage/integration/python/tests`.
- End to end: `integration/tests/native_python.py` and `native_node.mjs` (Node and Bun) run on the installed package, on SQLite and PostgreSQL.
- `scripts/check-extension-packages.sh` runs all of them in fresh Python, Node and Bun consumers, and checks the error without the package (`tests/public/disabled.py`, `disabled.mjs`).
