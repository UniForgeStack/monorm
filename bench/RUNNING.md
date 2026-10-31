# Running the benchmarks

The guide for an agent that runs or maintains a benchmark. The results are in [README.md](README.md).
Each directory or script below runs on the current API.

## Rules for results

- Each run writes to `results/<date>-<sha>/` of its benchmark: the UTC date of the run and the 7-character `main` commit it ran at. Add a suffix after the sha for a variant (`-r45-postgresql-1`).
- Keep only the latest run of each benchmark. Before you delete the previous folder, compare the two; if an old number was better, record it in the benchmark issue file of the plans folder, with the commit that recovers the data.
- Update [README.md](README.md): the verdict, the date, the sha and the link to the new results.
Run benchmarks on a quiet machine, after every build has finished, against a disposable PostgreSQL database.
Mark each number you report as measured (and say if the machine was loaded), estimated, or vendor-reported.

| Benchmark | Measures | Extension build |
|---|---|---|
| [`engine_bench.py`](engine_bench.py) | The public Python API on the blog models, end to end: reads, `select_related`, prefetch, inserts, updates, a transaction, concurrent gets | Runs on any build (core features only) |
| [`copy_insert.py`](copy_insert.py) | PostgreSQL COPY versus batched `insert_many()` through Python on the blog models | Runs on any PostgreSQL build |
| [`aggregates_pagination.py`](aggregates_pagination.py) | Scalar versus LATERAL relation aggregates with an unindexed parent sort, LIMIT 20 and OFFSET 10; 100,000 users / 1,000,000 posts by default | Runs on any build (PostgreSQL) |
| [`rust-compare/`](rust-compare) | `monorm-engine` (typed IR and JSON IR) against SeaORM 2.0.4 and Diesel-async, in Rust only; `diagnose` adds raw SQLx and a streaming read | Not applicable (engine only) |
| [`compare/`](compare) | Our ORM against Django and SQLAlchemy (sync and async), Prisma 7, Drizzle and `pg` on one schema: 20 cases, equality check, release builds, `run.sh` and `report.py` | `monorm-all` row uses the `all` extension artifact |
| [`internal/`](internal) | Where one monorm call spends its time: unprepared, `prepare()` and direct-IR calls in Python and TypeScript (Node) on six cases, with engine-only (Rust) and driver-only rows; PostgreSQL and SQLite; `run.sh` and `report.py` | Not applicable (core) |
| [`imdbench/`](imdbench) | The IMDBench scenarios: MonOrm (Python and Node) against Django, SQLAlchemy, TypeORM, Prisma and the raw drivers; a patch for the IMDBench repo, the run procedure and the results | Not applicable (released packages) |

## Disposable database

From the repository root, in Bash (Docker must be running):

```bash
docker run --rm -d -p 55432:5432 -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=orm_bench postgres:18
```

Wait until PostgreSQL accepts connections. This needs a free host port 55432;
choose another port if it is already occupied. The database is disposable and
removed when the container stops. See [the exact Rust run steps](rust-compare/README.md#reproduce).

## engine_bench.py

```bash
python languages/python/packaging/stage.py && maturin develop --release
python bench/engine_bench.py --url postgres://postgres:postgres@localhost/orm_bench [--rounds 15] [--out results.json]
```

It drops and creates the blog tables in that database.
The one wheel holds the built-in extensions, so the same command covers them.

## aggregates_pagination.py

The pagination benchmark reads `ORM_TEST_DATABASE_URL` (or `--url`) and drops and
creates the blog tables there. It checks equal results, alternates the two forms
after warm-up, and reports median milliseconds as measured on a loaded machine.
`--parents`, `--posts-per-parent`, `--rounds` and `--out` control the fixture and
JSON output; the output includes both SQL forms and `EXPLAIN (ANALYZE, BUFFERS)`.

## rust-compare

```bash
cd bench/rust-compare
ORM_BENCH_URL=postgres://... cargo run --release --bin monorm-rust-compare # writes results-diesel.json
ORM_BENCH_URL=postgres://... cargo run --release --bin diagnose
```

A separate Cargo workspace with path dependencies on `crates/core/` and `crates/engine/`.
The tables `monorm_rust_compare_post` and `monorm_rust_diagnose_post` must not exist before a run.
`ORM_BENCH_OUT` sets the output file; `ORM_BENCH_SEA_FAST=1` turns off SeaORM's per-checkout health ping (a matched-setting run).
See [`rust-compare/README.md`](rust-compare/README.md).

## Removed

Each harness or result below is gone from the tree; `git show <commit>:bench/<path>` recovers it from the last commit that has it.

- Last in `493e2472`: `shared-behavior/` (the advisory-lock hashing A/B, shipped), `build-time-extensions/` (the extension cost gate of a removed static build; the `monorm-all` column of `compare/` now measures the extension cost), the earlier result sets of `compare/`, `internal/` and `rust-compare/`, the merged IMDBench HTML and JSON files, and the long `RESULTS.md` with the Phase 0 and early-engine sections.
- Last in `889f1c4`: the Phase 0 harness (the SeaORM `ormcore` prototype) and `query-construction/`.
- `profile_query.py`: it could no longer run after `bdec377` deleted its `profile_probes.patch` (`git show bdec377^:bench/profile_probes.patch`).
