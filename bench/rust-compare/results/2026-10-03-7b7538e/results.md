# Rust engine versus SeaORM, Diesel and raw SQLx — 2026-10-03

Rust only: no Python or TypeScript overhead, and no speed claim about those APIs.
Every number is **measured**: macOS arm64, Rust 1.97.0, PostgreSQL 18.6 on localhost, one pooled connection per contender (see [README.md](../../README.md#environment)).
The run did not record its commit; `7b7538e` is inferred: the last `main` commit before `bc18f78e` added these results on 2026-10-04. The CPU model and machine load were not recorded either.

## Verdict

- With matched pool checkout settings, our engine is faster than SeaORM in all five cases (about 1.5× on a primary-key get).
- Diesel async is faster for 1,000-row reads (263 vs 303 µs); small reads and counts are close; our update is faster than Diesel.
- The largest measured opportunity: build the final results from the incoming row stream (1,000 rows: 296.5 → 247.8 µs, 16.4% lower). See [`docs/performance.md`](../../../../docs/performance.md#optimization-opportunities).

## Five cases, matched checkout settings

SeaORM runs with its per-checkout health ping off (`ORM_BENCH_SEA_FAST=1`), as our engine and Diesel do.
Median of three run medians, µs per operation; 90,240 timed operations.

| Case | SeaORM | Our direct IR + planner | Our JSON parse + planner | Diesel async |
|---|---:|---:|---:|---:|
| get by pk | 56.53 | 37.04 | 37.74 | 35.52 |
| read 50 | 78.11 | 55.21 | 55.18 | 55.09 |
| read 1000 | 362.57 | 303.32 | 303.12 | 262.96 |
| filtered count | 90.76 | 62.23 | 62.29 | 61.59 |
| update 100 | 171.26 | 151.65 | 151.75 | 188.52 |

Raw results: [run 1](results-matched-1.json), [run 2](results-matched-2.json), [run 3](results-matched-3.json).

## Read-path diagnosis

[`src/bin/diagnose.rs`](../../src/bin/diagnose.rs): nine read paths into the same owned Rust model, full row-equality checks, 18 rotated batches per run.
Median of three run medians, µs per operation; 160,380 database operations.

| Read path | 1 row | 50 rows | 1,000 rows |
|---|---:|---:|---:|
| Our direct IR + planner | 40.53 | 61.48 | 305.36 |
| Our driver, prebuilt SQL | 38.72 | 59.43 | 298.88 |
| tokio-postgres, typed buffered rows | 38.32 | 59.12 | 296.52 |
| tokio-postgres, typed streamed rows (prototype) | 38.25 | 58.78 | 247.84 |
| Diesel async | 39.42 | 61.03 | 253.14 |
| Raw SQLx `query_as`, default health checks | 74.86 | 99.69 | 335.67 |
| Raw SQLx `query_as`, fast checkout | 57.65 | 83.17 | 314.18 |
| SeaORM, default health checks | 77.11 | 101.58 | 385.02 |
| SeaORM, fast checkout | 60.06 | 83.92 | 369.80 |

- Streaming versus buffering the same SQL, pool and decoder: 296.52 → 247.84 µs at 1,000 rows, close to Diesel (253.14). Diesel async also maps rows from `query_raw` straight into models.
- Skipping our planner saves only 6.48 µs (305.36 → 298.88), so precompiled queries are a low priority.
- A CPU-only decode of 1,000 fetched rows: runtime `RowSet::cell` 56.11 µs, typed `Row::try_get` 48.18 µs.
- Default health checks add about 16–21 µs per read.

These cover simple sequential local reads, not joins, transactions, inserts, concurrency or remote latency.
Raw results: [run 1](results-diagnose-1.json), [run 2](results-diagnose-2.json), [run 3](results-diagnose-3.json).
