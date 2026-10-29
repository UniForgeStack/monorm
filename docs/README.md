# Documentation

The ORM has one core and optional feature extensions.
The core is in every build.
An extension is an installable package of a Rust library and runtime modules ([optional-packaging.md](contracts/optional-packaging.md#extension-contracts)).
A schema that uses an extension attribute needs the installed package of that extension.

Start with the runnable [quickstart](../examples/quickstart) (Python and TypeScript, in-memory SQLite).

## Core

| Doc | What |
|---|---|
| [schema.md](schema/schema.md) | The schema language, SQLite, client defaults, protected writes, catalog extensions, code generation, migrations |
| [prisma-syntax.md](schema/prisma-syntax.md) | Why the schema uses Prisma syntax, and the mapping from the old `.orm` syntax |
| [python-api.md](api/python-api.md) | The Python API: models, queries, pagination, `select()`, subqueries, windows, CTEs, writes, connections, transactions, protected writes, locks, the N+1 finder, hooks, errors |
| [typescript-api.md](api/typescript-api.md) | The TypeScript API, with the same sections |
| [query-set-kinds.md](api/query-set-kinds.md) | The kinds of query set, and the methods each kind leaves out |
| [optional-packaging.md](contracts/optional-packaging.md) | One package per registry, outside extensions and exact source builds |

## Guides and contracts

| Doc | What |
|---|---|
| [loading-instances.md](guides/loading-instances.md) | `post.load(...)` on an instance: partial shapes, `reload`, `NotLoaded`, the reference cache and shared queries |
| [reference-loading.md](contracts/reference-loading.md) | The `reference-loading` Cargo feature, hidden helper keys and the adapter rules |

## Feature extensions

| Feature | Crate | Attributes | Doc |
|---|---|---|---|
| proxy-models | `extensions/proxy` (`monorm/extensions/proxy` on npm) | `@@proxy.of`, `@@proxy.fields` | [proxy-models.md](extensions/proxy-models.md) |
| model-composition | `extensions/model-composition/` (`monorm/extensions/model-composition` on npm) | `@@composition.model` | [model-composition.md](extensions/model-composition.md) |
| file-storage | `extensions/file-storage` (`monorm/extensions/file-storage` on npm) | `@storage.file` | [file-storage.md](extensions/file-storage.md) |
| generic | `extensions/generic` (`monorm/extensions/generic` on npm) | `@generic.relation`, `@@generic.relation`, `@generic.reverse` | [generic-identities.md](schema/generic-identities.md) |
| timestamps | `extensions/timestamps` (`monorm/extensions/timestamps` on npm) | `@@timestamps.updated_at(name, mode:, upsert:)` | [schema-extensions.md](extensions/schema-extensions.md) |
| soft-delete | `extensions/soft-delete` (`monorm/extensions/soft-delete` on npm) | `@@soft_delete.deleted_at(name, mode:)` | [schema-extensions.md](extensions/schema-extensions.md) |
| locking | `extensions/locking` (`monorm/extensions/locking` on npm) | `@@locking.version(name, upsert:)` | [schema-extensions.md](extensions/schema-extensions.md) |
| inherit | `extensions/inherit` (`monorm/extensions/inherit` on npm) | `@@inherit.abstract`, `@@inherit.extends`, `@@inherit.override` | [inherit.md](extensions/inherit.md) |
| query-defaults | `extensions/query-defaults/` (`monorm/extensions/query-defaults` on npm) | `@@query.*`, `@query.selectOut` | [selection-and-defaults.md](schema/selection-and-defaults.md) |

[optional-packaging.md](contracts/optional-packaging.md#installable-packages) shows how to install them.
The same doc describes the extension contract: manifests, passes, hooks.
Protected writes, client defaults, cursor pagination and the N+1 finder are core features, not extensions.

## Apps

An app is a schema that you import with `import "app:<name>"`, and a package for each language ([schema.md](schema/schema.md#apps)).

| App | What |
|---|---|
| [auth.md](apps/auth.md) | The auth app: users, roles, permissions and passwords |

## Performance

| Doc | What |
|---|---|
| [performance.md](performance.md) | The cost gate of build-time extensions, and measured engine opportunities |
| [../bench/README.md](../bench/README.md) | The benchmarks and how to run them |
| [../bench/RESULTS.md](../bench/RESULTS.md) | Recorded results |

## Example

[`examples/blog`](../examples/blog) has the blog schema, its generated models and migrations, and `demo.py` / `demo.ts`.
The demos run every core feature, and each extension when the build has it.
