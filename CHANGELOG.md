# Changelog

All notable changes are listed here, in the [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format.

## [Unreleased]

- `monorm init [dir]` starts a Python or TypeScript project: it asks for the language, the tool, the database and the built-in extensions, and flags answer each question. `uvx monorm init`, `python -m monorm init`, `npx monorm init` and `npm create monorm` run the same code. The PyPI package now has a `monorm` console script, and `create-monorm` depends on `@ufstack/monorm`.

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
