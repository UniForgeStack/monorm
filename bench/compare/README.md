# Benchmark: our ORM against the libraries a user would pick instead

This folder holds the shared schema and seed, the Python and TypeScript contenders, the runners, `run.sh` and `report.py`.
There is no Rust runner yet.

- `schema.prisma` is the model. `schema.sql` is the same schema as SQL. `seed.sql` loads 1,000 users, 20 categories, 10,000 posts, 50,000 comments, 50 tags and 30,000 post-tag links.
- `python/run.py` runs the contenders `orm`, `django-sync`, `django-async`, `sqlalchemy-sync` (psycopg) and `sqlalchemy-async` (asyncpg) (and `orm-all`, see below).
- `js/run.mjs` runs the contenders `orm`, `prisma`, `drizzle` and `pg` on Node, and `orm`, `prisma` and `drizzle` on Bun.

## Full run

```bash
bench/compare/run.sh                     # builds release artifacts, 20 rounds x 20 timed iterations, writes results/<date>-<sha>/summary.md
```

`run.sh` builds first, then waits up to 5 minutes for a 1-minute load of at most 2; each runner waits again before every round.
It marks `run.json` invalid (and `summary.md` shows an INVALID banner) when the load at the end is above 2.
`--ignore-load` and `--fast-build` are for a pipeline smoke: the directory gets a `-smoke` suffix, and the numbers are not valid.
The database is `bench` (`BENCH_DB_NAME` changes it); the run drops and recreates it, and drops it at the end.
`report.py` gives each cell as the median of the per-round medians with a 95% cluster bootstrap interval (rounds resampled), and each speed-up as the median of the paired per-round values (contender time / `monorm` time) with its interval.
Read `2.3×` as "`monorm` is 2.3 times faster"; below 1 reads `0.8× (slower)`.
The `monorm / pg` column is overhead over raw `pg` (1.25× means `monorm` takes 1.25 times the time of `pg`), and `monorm-all / monorm` is the extension cost.
The Summary block at the top gives the geometric mean and the range over the cases, end to end (each time includes the database round trip), and the approximate CPU split separately.
`python report.py <results dir> [output.md]` writes `summary.md` in the results directory unless you name another file.

## Run one runner by hand

```bash
createdb -h localhost -p 55432 -U postgres bench          # a database of your own
export BENCH_DATABASE_URL=postgres://postgres:postgres@localhost:55432/bench
uv pip install django==6.1.2 'psycopg[binary]==3.3.6' psycopg-pool==3.3.3 asgiref==3.12.1 'sqlalchemy[asyncio]==2.1.6' asyncpg==0.32.0   # into a venv of its own that has the `monorm` wheel
cd bench/compare/python
python run.py check                                       # equality of results and database state, per case
python run.py run --rounds 2 --iters 5                    # check first, then timing; writes results/smoke/ (defaults: 20 x 20)
```

The runner resets the tables (`schema.sql`, `seed.sql`) before every process, so `BENCH_DATABASE_URL` must name a database that holds only benchmark data.
Each contender runs in its own process, once per round. The contender order rotates every round, and each case runs 50 untimed warm-up calls first, then the timed calls.
The equality check compares the result and the database state at iterations 0, 1 and the last one.
The output has one JSON file per contender (median, samples, batch medians, CPU split) and `environment.json` (versions, Postgres, load).

`--orm-artifact PATH` adds a row `orm-all`: it starts the `orm` runner with the Python of `PATH/venv` and, on Node, `MONORM_NATIVE=PATH/monorm.node`. `run.sh` builds `PATH` (`build/all`) as the one `monorm` wheel with the built-in extensions.

## Profile one case

```bash
cd bench/compare/js
node run.mjs profile --cases projection-1000 --contenders orm,pg   # options: --iters 200, --top 25, --out results/profile
```

The `profile` command runs one case per contender in its own process, under the V8 CPU profiler (Node only, a 100 µs sampling interval).
The profiler records the timed calls and their setup, after the warm-up.
For each contender, it writes `<contender>-<case>.cpuprofile` and `<contender>-<case>.top.txt` to the output directory, and prints the text.
The text gives the top functions by self time, then the self time by script.
`(program)` is native code with no JS frame (for `monorm`, the addon converts the rows), and `(idle)` is the wait for the database.
The profiler slows the calls, so do not compare its medians with the medians of `run`.
Open the `.cpuprofile` in Chrome DevTools (Performance) for the full call tree.

## Read the numbers with care

- The `monorm` wheel must be a release build with the postgres profile. A debug tooling build (the dev venv) runs, but its timings are not valid.
- Django has no async `atomic()`. The `tx-5` case on `django-async` runs a sync function through `sync_to_async`. Django async calls also serialize on one thread.
- The sync ORMs run without an event loop: Django refuses to run in one. The `django-sync` and `sqlalchemy-sync` processes run without one; their `concurrent` case uses 50 threads.
- SQLAlchemy opens one session per operation, and the session begins a transaction on first use: a read runs BEGIN, SELECT, ROLLBACK.
- The CPU split is the time to build the query and to render its SQL (`.sql()` of `monorm` inlines values, `str(queryset.query)` of Django does too; SQLAlchemy `compile()` and Drizzle `toSQL()` keep placeholders; the SQLAlchemy split compiles anew each time and skips the compiled cache that real calls use). The decode time is not reachable in either library and is not measured. Treat the split as approximate.
- `filter-ne-null` is an extra case: `exclude(category_id=3)` in Django keeps the NULL rows, and `(Post.category_id != 3).or_null()` is the same in `monorm`. SQLAlchemy uses `is_distinct_from(3)`; Prisma and Drizzle add an explicit `IS NULL` branch.
- Prepared statements: `monorm` prepares once per connection and caches. psycopg prepares after 5 runs of a statement. `adapter-pg` of Prisma and Drizzle send unnamed statements. The asyncpg dialect of SQLAlchemy caches prepared statements per connection.
- `monorm` returns full rows from `bulk-insert-*` (`returning()` has no key-only form); SQLAlchemy returns full instances too; Prisma and Drizzle select ids only. `summary.md` prints this and the other per-contender notes.
