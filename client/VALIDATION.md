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

## Ticket 01 completion (2026-10-12)

The initial commit omitted three criteria from the current ticket: associated query-root access, direct shape traits, and configured-default execution. The completion branch `fix/rust-basic-read-contract` adds all three, preserving the selection/loading changes in the original worktree.

`User::objects` is a normal `Query` constant, so custom query extension traits apply without a wrapper. Scalar tokens also expose snake_case associated constants. Every generic model shape implements its generated shape trait with ordinary value types and immutable accessors; relation associated types remain `Unloaded` until ticket 02. Consumer functions and structs accept minimal and fuller matching shapes and reject unloaded, incompatible, or falsely loaded-cache types.

Default database registration is isolated by generated schema identity. Both PostgreSQL and SQLite tests cover optional and required direct terminals, missing configuration, default removal, explicit binding overrides, repeated execution, and runtime get cardinality. The downstream compiler suite now contains 31 cases. Both backend-only builds and example regeneration passed. Updated measurements are in `bench/results.json`.

The completion standards review reported no hard violation and one optional observation about repeated forwarding in default-bound terminals. The completion specification review reported no blockers.

The workspace regression suite was rerun excluding `application_rust_runs_through_both_bindings`; its successful SQLite/PostgreSQL result from the initial implementation remains recorded above. The completion changes affect the Rust client and Rust-only generation, leaving that test's binding configuration unchanged. Broader all-target Clippy completes with existing repository warnings; the Rust client itself passes with warnings denied.
