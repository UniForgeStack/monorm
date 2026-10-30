# MonOrm

[![PyPI](https://img.shields.io/pypi/v/monorm)](https://pypi.org/project/monorm/)
[![Python](https://img.shields.io/pypi/pyversions/monorm)](https://pypi.org/project/monorm/)
[![npm](https://img.shields.io/npm/v/@ufstack/monorm)](https://www.npmjs.com/package/@ufstack/monorm)
[![Checks](https://github.com/UniForgeStack/monorm/actions/workflows/checks.yml/badge.svg)](https://github.com/UniForgeStack/monorm/actions/workflows/checks.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

An async ORM for Python and TypeScript with one Rust core.
You write the schema once in Prisma syntax.
The Rust core plans the SQL and runs it on PostgreSQL or SQLite.
Both languages get the same queries, the same migrations and generated, typed models.

> **Under active development.**
> It works today, and the author runs it in production.
> It is version 0.1.1: install it with `pip install monorm` or `npm install monorm@npm:@ufstack/monorm`.
> The npm package is `@ufstack/monorm` for now; always install it under the alias `monorm`, because the code imports `monorm`.
> Other people should not use it in production yet, or do so at their own risk.
> Evaluate it against your own app before you rely on it.

**About 1.97× faster end to end than Django (async), up to 3.55×.**
Source: the geometric mean of 20 cases in one run on one machine, with the database round trip included ([results](bench/RESULTS.md#speed-up-summary)).
Method, limits and the cases where it is not faster: [Benchmarks](#benchmarks).

The name is MonOrm.

## Example

The schema (the same in both languages):

```prisma
model User {
  id    Int @id @default(autoincrement())
  name  String
  posts Post[]
}
model Post {
  id        Int @id @default(autoincrement())
  title     String
  published Boolean
  author_id Int
  author    User @relation(fields: [author_id], references: [id])
}
```

Python:

```python
users = await User.objects.filter(User.posts.published).load(User.posts)
```

TypeScript:

```ts
const users = await User.objects.filter(User.posts.published).load(User.posts);
```

Both examples run as they are: [`examples/quickstart`](examples/quickstart) (in-memory SQLite).

## Why

- **The schema language is Prisma's.** One `schema.prisma` file, with the editor tooling that already exists ([docs](docs/schema/prisma-syntax.md)).
- **The model API is Django-style.** `Model.objects`, `filter()`, `load()` and migrations that come from the schema.
- **Typed expressions** in the style of SQLAlchemy, and generated typed models.
- **One Rust engine** behind both languages: the schema compiler, the SQL planner and the database drivers are shared.

### Inspired by

Thank you to the projects below. We claim only what we took from each.

- [Robyn](https://github.com/sparckles/Robyn): a Python surface with the hot path in Rust through PyO3. We use the same split for the database layer.
- [Prisma](https://www.prisma.io): the schema-first idea and the `schema.prisma` language.
  MonOrm is independent of Prisma ([details](docs/schema/prisma-syntax.md)).
  It is not a drop-in replacement for Prisma Client, and its query API is its own.
- [Django](https://www.djangoproject.com): the model API (`Model.objects`, querysets) and migrations from the schema (`monorm makemigrations`, `monorm migrate`, see [schema.md](docs/schema/schema.md)).

## Quickstart

There is one package for each registry:

```bash
pip install monorm      # the engine, every built-in extension and the auth app; `python -m monorm generate`
npm install monorm@npm:@ufstack/monorm      # the same for Node and Bun (the alias is required); `npx monorm generate`
```

The `monorm` command line comes with the Python and npm packages.
Both packages hold PostgreSQL and SQLite. They build for 3 platforms: Linux x86-64, macOS arm64 and Windows x86-64.
An extension that is not built in is a separate package that you install next to `monorm` ([optional-packaging.md](docs/contracts/optional-packaging.md)).

To build from source:

```bash
uv venv .venv && . .venv/bin/activate
uv pip install maturin
python languages/python/packaging/stage.py   # the extension libraries of this platform
maturin develop                              # the one monorm package
python examples/quickstart/quickstart.py     # Python 3.11 or later

cd languages/js && npm install && npm run assemble
cd ../../examples/quickstart && npm install && node quickstart.ts   # Node 23.6 or later
```

### Upgrade from the split packages

Before `v0.1.0` the project had a thin `monorm` package, a native package for each profile and one package for each extension.
Remove them, then install `monorm`:

```bash
pip uninstall monorm-native-postgres monorm-native-sqlite monorm-native-combined monorm-native-tooling \
  monorm-timestamps monorm-soft-delete monorm-locking monorm-inherit monorm-query-defaults \
  monorm-generic monorm-proxy monorm-composition monorm-file-storage monorm-auth
pip install monorm
npm uninstall @ufstack/monorm @ufstack/monorm-native-postgres @ufstack/monorm-native-sqlite @ufstack/monorm-native-combined @ufstack/monorm-native-tooling @ufstack/monorm-auth \
  @ufstack/monorm-timestamps @ufstack/monorm-soft-delete @ufstack/monorm-locking @ufstack/monorm-inherit @ufstack/monorm-query-defaults \
  @ufstack/monorm-generic @ufstack/monorm-proxy @ufstack/monorm-composition @ufstack/monorm-file-storage
npm install monorm@npm:@ufstack/monorm
```

The `monorm[postgres]` and `monorm[sqlite]` extras are gone: the wheel has both drivers.
Two copies of one extension id stop the load with the error "installed twice", which names the package to uninstall.
In a schema, `import "app:monorm-auth"` is the Python name of the auth app and `import "app:auth"` is the Node name.
Change the imports in your code from `@ufstack/monorm` to `monorm` and from `@ufstack/monorm-<name>` to `monorm/extensions/<name>`.

You need `uv`, Rust (`cargo`), and Node 20.19+ or 22.12+ (or Bun) for the TypeScript package.

## Features

| Feature | Read |
|---|---|
| Schema language, SQLite and PostgreSQL types, client defaults, code generation | [schema.md](docs/schema/schema.md) |
| Python API: queries, pagination, subqueries, windows, CTEs, writes, transactions, locks, hooks | [python-api.md](docs/api/python-api.md) |
| TypeScript API with the same sections | [typescript-api.md](docs/api/typescript-api.md) |
| Migrations from the schema | [schema.md](docs/schema/schema.md) |
| Protected writes, cursor pagination, the N+1 finder (`monorm.debug`) | [python-api.md](docs/api/python-api.md) |
| Packaging: one package for each registry, outside extension packages | [optional-packaging.md](docs/contracts/optional-packaging.md) |
| Auth app (in the `monorm` package) | [auth.md](docs/apps/auth.md) |

Build-time extensions are Rust crates. The `monorm` package has these built in:
proxy models, query defaults, model composition, file storage, generic relations, timestamps, soft delete, locking and inheritance.
See the [extension index](docs/README.md#feature-extensions).

## Databases and platforms

| | |
|---|---|
| Databases | PostgreSQL and SQLite |
| Python | 3.11 to 3.14 |
| TypeScript | Node 20.19+ or 22.12+, and Bun |
| Rust | **Coming soon.** A typed read client is in [`languages/rust/`](languages/rust/README.md) and is not published. |
| CI | Linux, macOS and Windows (package builds); Linux with PostgreSQL 18 (feature check) |

## Benchmarks

Measured in one run (`d428e01`, 2026-10-28) on a shared cloud VM (`vm`, Linux 6.18.44 x86_64) with PostgreSQL 16.15.
Contenders: Django 6.1.2 (sync and async), SQLAlchemy 2.1.6 (sync and async, asyncpg), Prisma 7.10.1, Drizzle 0.45.4 and `pg` 8.23.1.
"N× faster" is the contender median time divided by the `monorm` median time.
Every number is measured and end to end: it includes the database round trip.

| Case | Python vs Django async | Python vs SQLAlchemy async | Node vs Prisma | Node vs Drizzle |
|---|---|---|---|---|
| `read-pk` | 2.31× faster | 2.72× faster | 2.30× faster | 1.56× faster |
| `read-1000` | 3.26× faster | 2.77× faster | 1.96× faster | 1.43× faster |
| `prefetch-to-many` | 3.20× faster | 2.64× faster | 1.87× faster | 3.63× faster |
| `m2m` | 3.55× faster | 2.62× faster | 2.40× faster | 3.95× faster |
| `bulk-insert-1000` | 1.87× faster | 13.23× faster | 4.47× faster | 2.11× faster |
| `concurrent` | 3.32× faster | 2.77× faster | 1.59× faster | 1.78× faster |
| `tx-5` | 1.10× faster (level) | 1.19× faster | 1.40× faster | 0.97× (slower, level) |
| Geometric mean of 20 cases | 1.97× faster | 2.56× faster | 1.90× faster | 1.50× faster |

- The Python work alone (build the query and make the SQL, no database) is 4.39× faster than Django async (geometric mean of 10 cases). This number is approximate.
- Level or slower cases: a case is level when the interval of its speed-up includes 1.00.
  - Python vs Django async: `tx-5` (1.10×, level).
  - Python vs Django sync: `insert-1` (1.07×), `bulk-update-100` (1.10×) and `tx-5` (1.03×) are level.
  - Python vs SQLAlchemy sync: `tx-5` is 0.92× (slower, level). Against SQLAlchemy async, no case is level.
  - Node vs Prisma: `bulk-update-100` (1.08×) and `delete-100` (1.03×) are level.
  - Node vs Drizzle: `projection-1000` (1.01×), `filter` (1.11×), `update-1` (1.08×), `bulk-update-100` (0.99×, slower), `delete-100` (1.05×) and `tx-5` (0.97×, slower) are level.
- Bun has only 3 cases (`read-pk`, `read-1000`, `bulk-insert-1000`): 2.48× faster than Prisma and 1.46× faster than Drizzle (geometric means), none level.
- Against Django sync the Python geometric mean is 1.59× faster, against SQLAlchemy sync 2.25×.
- It is one run on a shared VM. Treat a difference whose interval overlaps 1.00 as no difference. Django async has no async `atomic()`, which affects `tx-5`.

Full tables, the method and the raw data: [`bench/RESULTS.md`](bench/RESULTS.md#speed-up-summary) and [`bench/compare/README.md`](bench/compare/README.md).
Run the comparison yourself before you decide.

## Status and limits

Both API docs list what is not done yet: [Python](docs/api/python-api.md#not-done-yet) and [schema](docs/schema/schema.md#not-done-yet).
The API is async only. There is no MySQL driver.
The crate on crates.io comes later.

## Docs

- [docs/README.md](docs/README.md): the documentation index (core, extensions, performance).
- [examples/blog](examples/blog): a schema, its generated models and migrations, and runnable demos.
- [CHANGELOG.md](CHANGELOG.md): what changed.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the setup, the checks and the pull request rules.
Report a security problem as described in [SECURITY.md](SECURITY.md).

## License

MIT. See [LICENSE](LICENSE).
