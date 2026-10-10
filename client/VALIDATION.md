# Ticket 01 validation

Validated locally on stable Rust 1.97.0, with SQLite and an isolated PostgreSQL 18 database.

- Workspace Rust tests: 202 passed, plus the native extension/binding integration test passed separately on SQLite and PostgreSQL (203 total; see below).
- Rust client: public SQLite/PostgreSQL reads, offline generation checks, and 26 downstream compiler cases passed.
- Python: 407 passed, 38 skipped; strict mypy and pyright passed.
- Node: 288 passed, 31 skipped; Bun: 284 passed, 32 skipped; TypeScript checking passed.
- Clippy passed for the client, core, engine, both bindings, contracts, and extension builder. The client also passed with each backend enabled separately.
- Python and TypeScript example regeneration produced no changes. Staged whitespace checks passed.

The full workspace formatting check reports existing differences across the repository; it was read-only and no formatting sweep was applied.

The native extension integration test's default build stalled during import with the uv standalone Python interpreter: its artifact also linked `libpython`. It was retried separately with `PYO3_BUILD_EXTENSION_MODULE=1` and `RUSTFLAGS=-C link-arg=-undefined -C link-arg=dynamic_lookup`, using macOS extension symbol lookup without changing the repository's binding configuration.

Standards and specification reviews found two blockers: composite unique keys were limited to four fields, and enum names could shadow generated generic parameters. Both were fixed and verified by downstream compiler cases. The follow-up specification review confirmed those blockers were resolved. Two optional refactoring observations remain: separate validation from rendering in the generator, and reduce terminal methods' navigation through the database handle.

Initial generation, checking, incremental checking, release build, and binary size measurements are recorded in [bench/results.json](bench/results.json). [README.md](README.md) describes their scope and reproduction.
