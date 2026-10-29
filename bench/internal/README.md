# Internal benchmark

This benchmark shows where one monorm call spends its time.
It runs six cases through three call paths in Python and in TypeScript (Node), on PostgreSQL and on SQLite.
It does not compare other ORMs: that is [`../compare/`](../compare).

| Path | What it calls |
|---|---|
| Unprepared | The public query set API, built again on each call |
| Prepared | The public `prepare()` API, built once and called with values |
| Direct IR | A prebuilt IR JSON and its parameters, sent to the internal run entry. It still builds the model instances |

Two reference rows give the floors:

- **Engine only:** the Rust JSON IR method of [`../rust-compare/`](../rust-compare) (parse, plan, run, read every cell) on the same IR and values.
  It is one row for both languages.
  `rust/` reuses `rust-compare/src/environment.rs`.
- **Driver only:** the same SQL on the raw driver (`psycopg` or `sqlite3` in Python, `pg` or `node:sqlite` in Node).
  The row shows `skipped` with a reason when the driver is missing.

## Cases

`cases.json` holds the cases for all runners: primary-key get, filter and order with a limit of 50, read 1,000 rows, filtered count, one insert, and a related read with prefetch.
The data is 11 users (user 11 only receives the inserts) and 1,000 posts.
Each case binds its values, with no literals.
Before the timing, each case checks that all paths return the same result.
`schema.prisma` has a `BACKEND` placeholder that the runners replace with `postgresql` or `sqlite`, because `examples/blog` cannot run on SQLite.

## Run

The run drops and creates the tables `users` and `posts`.
Use a disposable database.

```bash
ORM_BENCH_URL=postgres://postgres:postgres@localhost:5432/orm_bench bench/internal/run.sh   # PostgreSQL
bench/internal/run.sh                                                                        # SQLite file in build/
```

Start on a quiet machine.
The script waits up to 5 minutes for a 1-minute load average of 2 or less, then stops.
`--ignore-load` runs anyway and marks the run as not valid.
Flags: `--rounds N` (default 15), `--skip-build`, `--out DIR`.
A full run builds release artifacts into `build/` (a wheel, the Node addon, the Rust binary).
It does not touch the default `.venv` or `languages/js/monorm.node`.

`--quick` is a smoke mode: it uses the dev build, calls each case once per path, skips the Rust row, and marks the run as not valid.
The test `scripts/tests/test_internal_bench.py` runs it on SQLite.

## Results

`results/<date>-<commit>-<backend>/` holds `python-<backend>.json`, `js-<backend>.json`, `rust-<backend>.json`, `run.json` and `summary.md`.
Each JSON file records the machine, the load average and the date.
Every number is measured: the median of the timed batches, in µs per operation, with the batches rotating the path order.

`summary.md` splits each unprepared call into layers:

| Layer | Computed as |
|---|---|
| Builder | unprepared − prepared. It includes the IR JSON encode, because `prepare()` caches the JSON |
| Value binding | prepared − direct IR. It is the binding of the values and the scope; the JSON encode is not in it |
| Hop + instances | direct IR − engine only |
| Engine | engine only − driver only |
| Driver | driver only |

A write has no IR to prepare, so the insert has no prepared path and its builder layer is unprepared − direct IR.
The prefetch case is one engine call that runs two statements.
A negative layer means that the two paths are closer than the run can tell apart.
The engine layer is the Rust driver minus the driver of each language, so it also holds the difference between the two drivers.
The load average is read before and after the run, not in each round.
