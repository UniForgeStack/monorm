# Changelog

All notable changes are listed here, in the [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format.
No release is published yet, so every entry is under Unreleased.

## [Unreleased]

- Async Python and TypeScript APIs on one Rust core: queries, relation loading with `load()`, transactions, locks and protected writes.
- One Prisma-syntax schema (`schema.prisma`) for both languages, with generated typed models and migrations.
- PostgreSQL and SQLite.
- Query builders for `select()`, subqueries, windows and CTEs, plus cursor pagination and an N+1 finder.
- OpenTelemetry hooks.
- Build-time extensions: proxy models, query defaults, model composition, file storage, generic relations, timestamps, soft delete, optimistic locking and inherit.
- Extension packages for Python and npm, and the `monorm-auth` app (users, roles, permissions, passwords).
- A typed read client for Rust in `languages/rust/` (not published).
- Benchmarks against Django and Prisma in `bench/compare/`, recorded in `bench/RESULTS.md`.
