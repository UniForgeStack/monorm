# Language bindings

This page is for a contributor who wants to add a language to MonOrm.
Python and TypeScript (Node and Bun) are shipped.
Rust is in the repo as the `monorm-client` crate (`languages/rust`) and is not published: it has `publish = false`.
Go, PHP, Java and other languages are open for contributors.
No date is promised for any of them.

## What the engine owns

The Rust crates `monorm-core` and `monorm-engine` do the work.
A binding calls them and does not copy their logic.
Both shipped bindings (`languages/python/native/src/lib.rs`, `languages/js/native/src/lib.rs`) use the same engine calls:

- Schema: `monorm_core::dsl` (compile and check a project), `monorm_core::behavior::prepare`, `monorm_core::ir`.
- Queries: `monorm_engine::parse_op`, `plan::Planner`, `exec` (plan and run a read, a write or an upsert), `params::Scoped`.
- Database: `monorm_engine::db` (the PostgreSQL and SQLite drivers, executors and transactions).
- Migrations and drift: `monorm_engine::migrate`, `introspect::pull`, `introspect::drift`.
- Other services: `advisory` locks, `trace` (query hooks), `protect` (protected writes), `multi_table`, `inspect`, `result_check`.
- Code generation: `monorm_core::codegen`, one module for each language.

## What a binding owns

- **Argument conversion.** The binding converts a value of the language to an engine value, and the reverse for a result row.
  The two shipped bindings have a `convert.rs` for this.
- **Error mapping.** The engine returns `Error` and `DbError` values.
  The binding maps each kind to an error type of the language.
  The kinds in both bindings are `QueryError`, `SchemaError`, `MigrationError`, `WriteProtected`, `DatabaseError`, `IntegrityError`, `LockNotAvailable` and `TransactionAborted`.
  Look at `errors.rs` (Python) and `engine_err` (Node).
- **Async bridge.** The engine is async and runs on a Tokio runtime.
  Python uses `pyo3_async_runtimes::tokio::future_into_py`.
  Node uses the `tokio_rt` feature of `napi`.
  A new language must connect its own async model (a thread, a callback, a future or a promise) to that runtime.
- **Packaging.** The binding builds a native library for each platform and ships it in a package of the language registry.
  See `languages/python/packaging` and `languages/js/packaging`, and [optional-packaging.md](optional-packaging.md).

## The API contract

The public API of the two shipped languages is the reference: [python-api.md](../api/python-api.md) and [typescript-api.md](../api/typescript-api.md).
A new language must keep:

- the same names, in the case style of the language;
- the same query semantics;
- the same errors;
- the same results.

The schema (`schema.prisma`), the migrations and the generated SQL do not change with the language.

## The steps

1. Open an issue and agree on the design first (the rule in [CONTRIBUTING.md](../../CONTRIBUTING.md)).
2. Add a `generate-<lang>` Cargo feature and a module in `crates/core/src/codegen`, next to `python.rs`, `typescript.rs` and `rust.rs`.
   The features today are `generate-python`, `generate-typescript` and `generate-rust`.
3. Add a native crate in `languages/<lang>/native` that calls the engine and exposes the API of the language.
4. Add tests that run on PostgreSQL and on SQLite.
5. Add packaging for the registry of the language.
6. Add the new commands to the check lists in [CLAUDE.md](../../CLAUDE.md) and [CONTRIBUTING.md](../../CONTRIBUTING.md).
   Keep the two lists in step.
   After a change to code generation, regenerate the example models.

## The bridge for each language

The table shows how [OpenDAL](https://opendal.apache.org/) binds each language.
Read its `bindings/` directory in the `apache/opendal` repository as an example.
It is an example of a pattern, not a MonOrm requirement.

| Language | Bridge in OpenDAL |
|---|---|
| PHP | A PHP extension (`cdylib`) built with the `ext-php-rs` crate |
| Java | The `jni` crate |
| Go | A C ABI library, loaded with `purego` and `jupiterrider/ffi` (no cgo) |

MonOrm has no C ABI crate for the engine today.
The C ABI in `crates/contracts/src/abi.rs` is for extension libraries, not for a language binding.
A Go binding needs a new C ABI crate first.
