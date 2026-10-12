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
