# Performance

## Python and TypeScript versus Django, SQLAlchemy, Prisma and Drizzle

[`bench/compare`](../bench/compare/README.md): the public Python and TypeScript APIs of `monorm` against Django 6.1.2 and SQLAlchemy 2.1.6 (sync and async), Prisma 7.10.1, Drizzle 0.45.4 and raw `pg` 8.23.1 (the floor), on one shared schema and seed. The run (`bench/compare/run.sh`, no flags) used release builds at `d428e01`: 20 rounds × 20 timed iterations, 50 untimed warm-up calls per case, one fresh process per contender and round, rotated order.
Results directory: [`bench/compare/results/2026-10-28-d428e01/`](../bench/compare/results/2026-10-28-d428e01/summary.md) (`summary.md`, raw JSON per contender, `run.json`, `environment.json`, logs).

Machine (from `environment.json`): cloud VM `vm`, Linux 6.18.44 x86_64, PostgreSQL 16.15 over TCP on localhost. CPython 3.14.6, Node v22.22.0, Bun 1.4.4. The CPU model is not in this run's `environment.json`. The VM comes from the same cloud service as the 2026-10-21 run, so it probably has the same Intel Xeon 2.80GHz, 4 cores and 16 GB (owner's statement, not recorded).

All numbers below are **measured** on 2026-10-28 on that VM and copied from `summary.md` unchanged. `run.json` marks the run valid: the 1-minute load was 1.00 at the start and 1.13 at the end, and 1.14 to 1.97 before each round (limit 2). The machine is a shared cloud VM, so treat differences whose interval overlaps 1.00 as no difference.

Speed-up is contender time / `monorm` time: `2.3×` means `monorm` is 2.3 times faster. End to end: each time includes the database round trip.

- Python vs `django-sync`: geometric mean 1.59×, range 1.03× (tx-5) to 3.36× (concurrent) (20 cases)
- Python vs `django-async`: geometric mean 1.97×, range 1.10× (tx-5) to 3.55× (m2m) (20 cases)
- Python vs `sqlalchemy-sync`: geometric mean 2.25×, range 0.92× (slower) (tx-5) to 11.57× (bulk-insert-1000) (20 cases)
- Python vs `sqlalchemy-async`: geometric mean 2.56×, range 1.19× (tx-5) to 13.23× (bulk-insert-1000) (20 cases)
- Node vs `prisma`: geometric mean 1.90×, range 1.03× (delete-100) to 11.68× (upsert-100) (20 cases)
- Node vs `drizzle`: geometric mean 1.50×, range 0.97× (slower) (tx-5) to 3.95× (m2m) (20 cases)
- Bun vs `prisma`: geometric mean 2.48×, range 1.61× (read-1000) to 3.61× (bulk-insert-1000) (3 cases)
- Bun vs `drizzle`: geometric mean 1.46×, range 1.24× (read-1000) to 1.63× (bulk-insert-1000) (3 cases)

CPU split (build query + make SQL, contender time / `monorm` time): approximate, no database, decode not measured. The main numbers above are end to end.

- Python vs `django-sync`: geometric mean 4.11×, range 1.92× (prefetch-to-many) to 6.65× (read-1000) (10 cases)
- Python vs `django-async`: geometric mean 4.39×, range 2.05× (prefetch-to-many) to 7.12× (read-1000) (10 cases)
- Python vs `sqlalchemy-sync`: geometric mean 10.21×, range 5.96× (aggregate) to 24.99× (join-to-one) (10 cases)
- Python vs `sqlalchemy-async`: geometric mean 10.20×, range 6.28× (aggregate) to 24.09× (join-to-one) (10 cases)
- Node vs `drizzle`: geometric mean 2.42×, range 1.09× (aggregate) to 5.82× (join-to-one) (10 cases)
- Bun vs `drizzle`: geometric mean 1.16×, range 1.08× (read-pk) to 1.24× (read-1000) (2 cases)

All numbers are **measured** on this machine: 20 rounds x 20 iterations per case (the warm-up calls are not timed), the contender order rotated every round, one fresh process per contender and round. Each cell is `median [95 percent cluster bootstrap interval]` of the per-round medians; each speed-up is the median of the paired per-round values (contender time / monorm time) with its interval.

Per-case tables (median and 95% cluster bootstrap interval), the `concurrent` throughput, the CPU split per contender and the per-contender notes: [`bench/RESULTS.md`](../bench/RESULTS.md#python-and-typescript-versus-django-sqlalchemy-prisma-and-drizzle--2026-10-28).
The 2026-10-21 run (`c9bbf7c`, no SQLAlchemy or Drizzle) is superseded and stays there as a record.

### Read the numbers with care

From [`bench/compare/README.md`](../bench/compare/README.md#read-the-numbers-with-care):

- The `monorm` wheel must be a release build with the postgres profile. A debug tooling build (the dev venv) runs, but its timings are not valid.
- Django has no async `atomic()`. The `tx-5` case on `django-async` runs a sync function through `sync_to_async`. Django async calls also serialize on one thread.
- The sync ORM cannot run in an event loop. The `django-sync` process runs without one; its `concurrent` case uses 50 threads.
- The CPU split is the time to build the query and to render its SQL (`.sql()` of `monorm` inlines values, `str(queryset.query)` of Django does too). The decode time is not reachable in either library and is not measured. Treat the split as approximate.
- `filter-ne-null` is an extra case: `exclude(category_id=3)` in Django keeps the NULL rows, and `(Post.category_id != 3).or_null()` is the same in `monorm`.
- Prepared statements: `monorm` prepares once per connection and caches. psycopg prepares after 5 runs of a statement. `adapter-pg` of Prisma sends unnamed statements.
- `monorm` returns full rows from `bulk-insert-*` (`returning()` has no key-only form); Prisma selects ids only. `summary.md` prints this and the other per-contender notes.

## Build-time extension cost gate

A build with extensions must cost less than 1% on warm operations, compared with the same behavior written by hand into the core.
`bench/build-time-extensions` measures it: paired batches of handwritten built-in controls against the generated extension build, for Python and Node, on PostgreSQL and SQLite.
A case passes when its paired 95% upper bound is below 1%.

The latest run (2026-10-08, `10663b5`, Apple M5, PostgreSQL in Docker) has **not passed** (measured, load 1.9 to 4.3):
0 warm regressions, 20 of 44 cases accepted, 24 inconclusive.
Most inconclusive cases change by −2.1% to +1.4%, but their intervals are too wide on a loaded machine to clear 1%.
Definition (setup) cost and artifact size are reported separately: the disabled native library is 5.5% (Python) and 5.4% (Node) larger, from contract and IR decoders (measured at `360685a`).
Details: [`gate-2026-10-08/summary.md`](../bench/build-time-extensions/results/gate-2026-10-08/summary.md), [`bench/RESULTS.md`](../bench/RESULTS.md).

## Optimization opportunities

Status: measured opportunities, not scheduled or implemented in the production
engine. Evidence recorded on 2026-10-03. Start with result materialization for
large reads; the measurements below do not establish priorities for every
workload or language binding.

### Evidence and scope

The [benchmark report](../bench/RESULTS.md#diesel-investigation-and-pool-setting-correction--2026-10-03)
contains the source investigation, corrected ORM comparisons, and raw results.
The [diagnostic harness](../bench/rust-compare/src/bin/diagnose.rs) compares
identical owned Rust models on PostgreSQL 18.6, with one pooled connection per
variant, release builds, and three independent runs. It checks every returned
field and rotates contender order. Timings are medians of run medians.

| Opportunity | Measured evidence | Priority |
|---|---|---|
| Materialize results from the incoming row stream | Typed buffered versus typed streamed reads of 1,000 rows: **296.52 → 247.84 µs**, 16.4% lower latency | First |
| Choose a reusable decoder from the query output shape | CPU-only decoding and allocation of 1,000 already fetched rows: runtime cells **56.11 µs**, typed decoding **48.18 µs** | Second |
| Skip query planning for fixed queries | Current planner path **305.36 µs**, prebuilt SQL through our driver **298.88 µs** | Defer until another workload justifies it |

These experiments isolate different costs; their savings must not be added
together as a forecast. The streaming prototype also skips our planner and
runtime cell interface. Its absolute latency is not a production-engine result.
The controlled buffered/streamed pair uses the same SQL, pool, prepared-statement
cache, typed decoder, and final result type. Small reads showed little benefit
from streaming.

### 1. Stream into final results

Today, [the PostgreSQL driver](../crates/engine/src/db/postgres.rs) calls
`Client::query`, collects a `Vec<Row>`, and returns a boxed `RowSet`. Consumers
then allocate and fill their final result collection. Diesel's async adapter
uses `query_raw`, converts arriving rows into typed models, and collects those
models directly. Our diagnostic prototype demonstrates the same approach with
tokio-postgres.

Prototype an internal query consumer or materialization API that can build final
results while consuming the incoming stream. Retain the buffered path for
existing consumers and operations that need a row set, including relation-key
collection and prefetching. Public query results can still be complete lists;
this opportunity does not require a public streaming API.

The implementation must preserve ordering, null handling, errors, transactions,
and cancellation behavior. Python and Node objects must be constructed in the
runtime context their bindings require; the Rust-struct prototype does not
measure those constraints. Measure binding materialization and thread hand-off
costs before claiming the same improvement for Python or JavaScript.

### 2. Reuse decoders for an output shape

`RowSet::cell` dispatches on `ValueType` for each field of each row. Investigate
choosing column decoders once from a validated output shape and reusing that
recipe across rows. Include selected expressions, joined models, nullable
relations, enum mappings, arrays, decimals, and extension types in the design.

For a future native Rust frontend, generated model types can enable specialized
row decoding. That is a distinct opportunity from compiling schema metadata
merely to reduce startup time. Shared Python/Node packages should continue to
support dynamic schemas without requiring a native build for each schema.

### 3. Keep planning and cache changes evidence-driven

Prepared database statements are already cached by our driver. Diesel also
caches prepared statements using typed query identities where available; the
inspected diesel-async version still renders SQL before its cache lookup.
Its read performance does not demonstrate that all SQL was compiled at build
time.

The current simple-read diagnostic attributes only about 6.5 µs to bypassing
planning. Keep the existing decision against a general IR-keyed plan cache
unless complex-query profiling establishes enough benefit to justify binding
recipes, cache memory, invalidation, and correctness work. Building typed Rust
operations directly can avoid JSON parsing independently of plan caching.

### Benchmark and acceptance requirements

- Match pool checkout policies, transport, TLS, connection count, database
  settings, result shape, and decoded fields. Earlier SeaORM comparisons used
  default per-checkout health pings while our engine and Diesel used fast
  recycling; use the corrected results when assessing implementation costs.
- Run release benchmarks sequentially after compilation finishes. Preserve raw
  measurements, rotate execution order, and report variability. The retained
  diagnostic calibration run overlapped compilation and is excluded.
- Assert complete result equality and write-state correctness. Include small
  reads as well as large reads to detect regressions.
- Before shipping an engine change, exercise joins, projections, nested
  prefetching, transactions, cancellation, conversion errors, and supported
  column types. Verify the existing buffered API and SQLite behavior.
- Measure the public Python and Node APIs and concurrent workloads. The current
  diagnostic covers sequential local PostgreSQL reads of 1, 50, and 1,000 rows;
  it does not establish production throughput or memory savings. Measure peak
  memory separately if the implementation claims a memory improvement.

Full comparison methodology and reproduction details live in
[bench/rust-compare/README.md](../bench/rust-compare/README.md).
