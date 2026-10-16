# Documentation

The ORM has one core and optional feature extensions.
The core is in every build.
An extension is a Rust crate that you select when you build the native library ([build-time-extensions.md](build-time-extensions.md)).
A schema that uses an extension attribute needs a build with that extension.

## Core

| Doc | What |
|---|---|
| [schema.md](schema.md) | The schema language, SQLite, client defaults, protected writes, catalog extensions, code generation, migrations |
| [prisma-syntax.md](prisma-syntax.md) | Why the schema uses Prisma syntax, and the mapping from the old `.orm` syntax |
| [python-api.md](python-api.md) | The Python API: models, queries, pagination, `select()`, subqueries, windows, CTEs, writes, connections, transactions, protected writes, locks, the N+1 finder, hooks, errors |
| [typescript-api.md](typescript-api.md) | The TypeScript API, with the same sections |
| [async-reference-loading.md](async-reference-loading.md) | Explicit to-one loaders `load_<relation>()` / `load<Relation>()` (on by default in the bindings) |
| [optional-packaging.md](optional-packaging.md) | Native profiles (`postgres`, `sqlite`, `combined`, `tooling`, `custom`) and exact source builds |

## Feature extensions

| Feature | Crate | Attributes | Doc |
|---|---|---|---|
| proxy-models | `extensions/proxy` (`orm-proxy`) | `@@proxy.of`, `@@proxy.fields` | [proxy-models.md](proxy-models.md) |
| query-defaults | `extensions/query-defaults/` (`orm-query-defaults`) | `@@query.*`, `@query.selectOut` | [selection-and-defaults.md](selection-and-defaults.md) |
| model-composition | `model-composition/` (`orm-model-composition`) | `@@composition.model` | [model-composition.md](model-composition.md) |
| file-storage | `storage/orm-extension` (`orm-file-storage-extension`) | `@storage.file` | [file-storage.md](file-storage.md) |
| generic-relations | `extensions/generic` (`orm-generic`) | `@generic.relation`, `@@generic.relation`, `@generic.reverse` | [generic-identities.md](generic-identities.md) |

[build-time-extensions.md](build-time-extensions.md#select-feature-extensions) shows how to select them, build, and package the result.
The same doc describes the extension contract: manifests, passes, native behavior profiles.
Protected writes, client defaults, cursor pagination and the N+1 finder are core features, not extensions.

## Performance

| Doc | What |
|---|---|
| [performance.md](performance.md) | The cost gate of build-time extensions, and measured engine opportunities |
| [../bench/README.md](../bench/README.md) | The benchmarks and how to run them |
| [../bench/RESULTS.md](../bench/RESULTS.md) | Recorded results |

## Example

[`examples/blog`](../examples/blog) has the blog schema, its generated models and migrations, and `demo.py` / `demo.ts`.
The demos run every core feature, and each extension when the build has it.
