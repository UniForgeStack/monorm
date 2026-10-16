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

# Ticket 02 validation

Validated on stable Rust 1.97.0 with SQLite and an isolated PostgreSQL 18 database.

- The workspace run passed 206 tests. The native extension/binding integration test passed separately on SQLite and PostgreSQL with the macOS configuration described above (207 total).
- Public loading and read tests passed on both databases, including defaults and bypass ordering, additive/reload transitions, NULL and empty caches, per-parent filtered limits, recursive relations, named views, runtime conversion, hidden keys, default ordering, and PostgreSQL arrays.
- All 48 downstream compiler cases passed. They cover direct associated-type requirements on functions and structs, nested target requirements, supersets, fixed runtime row types, cancellation ownership, immutable fields/views, and rejected unloaded or incompatible shapes.
- Offline generation checks, client Clippy with warnings denied, SQLite-only and PostgreSQL-only client builds, formatting of changed Rust modules, and whitespace checks passed.

## Standards review

Review against ticket 01's completed commit found an OR-combination panic when relation-specific filters entered root Boolean filters. Root and nested filters now stay separate, and both filtered and unfiltered prefetch combinations have public regression coverage. A shared upgrade-query initializer also removes duplicated setup. The follow-up review found no blockers. Repeated relation cardinality rendering in the generator remains an optional refactoring opportunity.

## Specification review

Review found that reloading a narrowed nested target could discard previously loaded fields while advertising their union in the resulting type. Generated selection normalization now fetches the advertised target shape, with public regression coverage. Follow-up review also checked bare versus narrowed targets through default bypass, owned array storage, foreign-key/helper precedence, and default ordering; it found no blockers.

Ticket 02 measurements are recorded in [bench/selection-results.json](bench/selection-results.json). They include finite nested selections over a cyclic schema. These are local observations with warm dependencies, not acceptance thresholds or isolated monomorphization timings. Rows retain owned loaded values and private helper keys to support checked conversion and transitions; this increases storage relative to the ticket 01 decode-only representation.

## Contract follow-up integration

Ticket 02 supersedes the earlier contract implementation described above. The follow-up default-registration isolation/lifecycle tests and compiler cases are retained, adapted to queryable unloaded relations. Custom Query extension traits start at `User::objects.query()` with the ticket 02 root.

Main integration also adapts the Rust client to complete composite keys introduced on main. Identity, relation queries, prefetch grouping, parameter budgeting, and default ordering use every key component; composite primary keys expose a tuple lookup token rather than individually unique component tokens. All client tests passed on SQLite and PostgreSQL, including two new composite loading cases and 53 downstream compiler cases. Client all-target Clippy passed with warnings denied.

## Ticket 02 root contract audit (2026-10-15)

The earlier integration replaced ticket 01's `Query` constant with a forwarding root wrapper. The associated root is now a `Query` constant again, and `Query::new()` is const and allocation-free. Custom query extensions apply directly before or after nested selection. `.query()` remains an identity convenience. Deferred model/default setup uses the same initializer for execution, shape transitions, runtime rows, and nested target plans; engine semantics are unchanged.

Added downstream compiler checks for the exact constant type and constructor, direct root extensions, and nested associated-type bounds on structs accepting partial, full, and checked runtime rows. Database checks exercise extension order, normal bare prefetch targets with default references, and one nested-shape consumer accepting partial/full/checked rows while conversion rejects unloaded caches and cached targets missing required columns. Both supported databases use the same public cases.

Validation passed: 12 client tests with 56 downstream compiler cases, PostgreSQL/SQLite public reads and loading, both backend-only builds, and client all-target Clippy with warnings denied. Reverting the production fix reproduced the constant/root compiler failures and both database-test compilation failures; restoring it made the client suite green. The borrowed-title example now states the target lifetime explicitly.

Repository checks passed: 222 workspace Rust tests including both native binding backends, repository Clippy, Python (564 passed, 37 skipped), Node (387 passed, 30 skipped), Bun (383 passed, 31 skipped), strict mypy, pyright, TypeScript checking, and unchanged Python/TypeScript example regeneration. The fresh virtualenv initially lacked pytest-asyncio; installing the declared dev dependencies resolved that environment failure. All 17 entries in the all-features matrix passed, including Python (598 passed, 3 skipped). Timings in retained logs were measured on a loaded machine.
