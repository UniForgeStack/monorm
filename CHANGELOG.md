# Changelog

All notable changes are listed here, in the [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format.

## [Unreleased]

- Destructive-change gate. `makemigrations` writes a `-- monorm: destructive <kind> <table>[.<column>]` line above each step that drops a table, drops a column, or changes a column type so that values can be lost; `Step.destructive` says the same in Python and TypeScript plans. Breaking: `migrate` and `rollback` refuse such a migration when the marked table has rows, unless you pass `--allow-destructive` (Python `allow_destructive=True`, TypeScript `{ allowDestructive: true }`). A missing or empty table passes, and a migration made before this change has no markers.
- `monorm.toml` is the first settings file of every `monorm` command (`schema`, `migrations`, `query_sets`, `model_methods`). `[tool.monorm]` in `pyproject.toml` and the `"monorm"` key of `package.json` stay as the fallback, in that order; the first file that holds settings wins and the files do not merge. On Node, `[tool.monorm]` in `pyproject.toml` now wins over the `"monorm"` key of `package.json`; the Node host used to read `package.json` first. A settings file that does not parse is an error that names the file: `monorm` used to skip a broken `pyproject.toml` or `package.json`. A file after the winner is not read. A wrong key type, or the key spelling of another file format (`querySets` in a TOML file, `query_sets` in `package.json`), is an error that names the file and the key. The `query_sets` and `model_methods` tables come from the winning file for every language, so a project that keeps Python query sets in `pyproject.toml` and TypeScript query sets in `package.json` moves them to one `monorm.toml` per language directory. `monorm init` writes a `monorm.toml`.
- `ValidationError` in Python and TypeScript: messages per field path (`address.city`, `lines.2.qty`) and messages with no field (`non_field_messages`, `nonFieldMessages` in TypeScript). A dot in a field name is not escaped. It crosses the binding as a typed error, and Rust code reads it as `monorm_engine::ValidationError`.
- `monorm init [dir]` starts a Python or TypeScript project: it asks for the language, the tool, the database and the built-in extensions, and flags answer each question. `uvx monorm init`, `python -m monorm init`, `npx monorm init` and `npm create monorm` run the same code. The PyPI package now has a `monorm` console script, and `create-monorm` depends on `@ufstack/monorm`.
- Auth: `sync_permissions` / `syncPermissions` takes the permission names (`<label>.<codename>`) and creates the absent ones in one bulk insert. It deletes nothing. `model_permission_names` / `modelPermissionNames` gives the Django-style per-model names. Breaking: the call `sync_permissions([Post], "blog")` becomes `sync_permissions(model_permission_names([Post], "blog"))`.

## [0.1.2] - 2026-10-31

- `create-monorm` gives Bun commands under `bun create monorm`: the start script is `bun index.ts`.
- Linux arm64: a `manylinux_2_28_aarch64` wheel on PyPI and a `linux-arm64` addon in the npm package. The two Linux platforms need glibc 2.28 or newer. Outside extension packages do not ship arm64 yet.

## [0.1.1] - 2026-10-30

- The npm package is `@ufstack/monorm`, because npm rejected the name `monorm`. Install it under the alias `monorm`: `npm install monorm@npm:@ufstack/monorm`. Imports stay `monorm`.

## [0.1.0] - 2026-10-30

- Renamed from `orm` to `monorm`. The CLI is `monorm` and the Python module is `monorm`.
- Async Python and TypeScript APIs on one Rust core: queries, relation loading with `load()`, transactions, locks and protected writes.
- One Prisma-syntax schema (`schema.prisma`) for both languages, with generated typed models and migrations.
- PostgreSQL and SQLite.
- Query builders for `select()`, subqueries, windows and CTEs, plus cursor pagination and an N+1 finder.
- OpenTelemetry hooks.
- Build-time extensions: proxy models, query defaults, model composition, file storage, generic relations, timestamps, soft delete, optimistic locking and inherit.
- One package per registry: PyPI `monorm` (12 platform wheels), npm `monorm` (one tarball for 3 platforms) and the crate `monorm` (the contracts library). Each of the first two holds the engine, both databases, the CLI, the nine built-in extensions and the `monorm-auth` app. Outside extensions install next to it, for Python and npm.
- `MONORM_PROFILE`, the `monorm[...]` extras, the native-profile packages and the per-extension packages do not exist.
- A typed read client for Rust in `languages/rust/` (not published).
- Benchmarks against Django and Prisma in `bench/compare/`, recorded in `bench/RESULTS.md`.
