# MonOrm

An async ORM for Python and TypeScript with one Rust core.
You write the schema once in Prisma syntax.
The Rust core plans the SQL and runs it on PostgreSQL or SQLite.
Both languages get the same queries, the same migrations and generated, typed models.

> **Under active development.**
> It works today, and the author runs it in production.
> It is version 0.1.0 and not published yet.
> Other people should not use it in production yet, or do so at their own risk.
> Evaluate it against your own app before you rely on it.

**About 2× faster end to end than Django (async), up to 4.2×.**
Source: the geometric mean of 20 cases in one run on one machine, with the database round trip included ([results](bench/RESULTS.md#speed-up-summary)).
Method, limits and the cases where it is not faster: [Benchmarks](#benchmarks).

The name is MonOrm.
The packages, the import name and the commands are still `orm` until the rename.

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
- [Django](https://www.djangoproject.com): the model API (`Model.objects`, querysets) and migrations from the schema (`orm makemigrations`, `orm migrate`, see [schema.md](docs/schema/schema.md)).

## Quickstart

There is no registry install line yet. Build from source:

```bash
uv venv .venv && . .venv/bin/activate
uv pip install -e '.[dev]' maturin
(cd languages/python/packaging/tooling && maturin develop)   # the native library
python examples/quickstart/quickstart.py                      # Python 3.11 or later

cd languages/js && npm install && npm run build:native && npm run build
cd ../../examples/quickstart && npm install && node quickstart.ts   # Node 23.6 or later
```

You need `uv`, Rust (`cargo`), and Node 20.19+ or 22.12+ (or Bun) for the TypeScript package.

## Features

| Feature | Read |
|---|---|
| Schema language, SQLite and PostgreSQL types, client defaults, code generation | [schema.md](docs/schema/schema.md) |
| Python API: queries, pagination, subqueries, windows, CTEs, writes, transactions, locks, hooks | [python-api.md](docs/api/python-api.md) |
| TypeScript API with the same sections | [typescript-api.md](docs/api/typescript-api.md) |
| Migrations from the schema | [schema.md](docs/schema/schema.md) |
| Protected writes, cursor pagination, the N+1 finder (`orm.debug`) | [python-api.md](docs/api/python-api.md) |
| Native profiles (`postgres`, `sqlite`, `combined`, `tooling`, `full`) | [optional-packaging.md](docs/contracts/optional-packaging.md) |
| Auth app (`orm-auth`) | [auth.md](docs/apps/auth.md) |

Build-time extensions are Rust crates that you select when you build the native library:
proxy models, query defaults, model composition, file storage, generic relations, timestamps, soft delete, locking and inheritance.
See the [extension index](docs/README.md#feature-extensions).

## Databases and platforms

| | |
|---|---|
| Databases | PostgreSQL and SQLite |
| Python | 3.11 to 3.14 |
| TypeScript | Node 20.19+ or 22.12+, and Bun |
| Rust | **Coming soon.** A typed read client is in [`languages/rust/`](languages/rust/README.md) and is not published. |
| CI | Linux, macOS and Windows (package profiles); Linux with PostgreSQL 18 (feature check) |

## Benchmarks

Measured in one run (`c9bbf7c`, 2026-10-21) on a shared cloud VM: Intel Xeon 2.80GHz, 4 cores, 16 GB, PostgreSQL 16.15.
Contenders: Django 6.1.2 (sync and async), Prisma 7.10.1 and `pg` 8.23.1.
"N× faster" is the contender median time divided by the `orm` median time.
Every number is end to end: it includes the database round trip.

| Case | Python vs Django async | Node vs Prisma |
|---|---|---|
| `read-pk` | 1.6× faster | 2.0× faster |
| `read-1000` | 3.0× faster | 1.6× faster |
| `prefetch-to-many` | 3.3× faster | 1.7× faster |
| `m2m` | 4.2× faster | 2.2× faster |
| `bulk-insert-1000` | 1.9× faster | 4.3× faster |
| `concurrent` | 2.7× faster | 1.5× faster |
| `tx-5` | 1.1× faster (level) | 1.4× faster |
| Geometric mean of 20 cases | 1.95× faster | 1.77× faster |

- The Python work alone (build the query and make the SQL, no database) is about 4.9× faster than Django async. This number is approximate.
- `orm` is not always faster.
  On Node `projection-1000`, Prisma is 1.4× faster than `orm`.
  Node `delete-100` (0.91×) and Python `tx-5` (1.1×) are level.
  Against Django sync, Python is level on `tx-5` (0.95×) and `insert-1` (1.01×).
- Against Django sync, the Python geometric mean is 1.57× faster.
- It is one run on a shared VM with a load near the limit. Treat a difference whose interval overlaps 1.00 as no difference.
- SQLAlchemy and Drizzle are not in this run yet. Django async has no async `atomic()`, which affects `tx-5`.

Full tables, the method and the raw data: [`bench/RESULTS.md`](bench/RESULTS.md#speed-up-summary) and [`bench/compare/README.md`](bench/compare/README.md).
Run the comparison yourself before you decide.

## Status and limits

Both API docs list what is not done yet: [Python](docs/api/python-api.md#not-done-yet) and [schema](docs/schema/schema.md#not-done-yet).
The API is async only. There is no MySQL driver.
The packages are not published, and the name `orm` changes to `monorm` in the rename.

## Docs

- [docs/README.md](docs/README.md): the documentation index (core, extensions, performance).
- [examples/blog](examples/blog): a schema, its generated models and migrations, and runnable demos.
- [CHANGELOG.md](CHANGELOG.md): what changed.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the setup, the checks and the pull request rules.
Report a security problem as described in [SECURITY.md](SECURITY.md).

## License

MIT. See [LICENSE](LICENSE).
