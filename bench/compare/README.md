# Benchmark: our ORM against the libraries a user would pick instead

This folder holds the shared schema and seed, the Python and TypeScript contenders, the runners, `run.sh` and `report.py`.
There is no Rust runner yet.

- `schema.prisma` is the model. `schema.sql` is the same schema as SQL. `seed.sql` loads 1,000 users, 20 categories, 10,000 posts, 50,000 comments, 50 tags and 30,000 post-tag links.
- `python/run.py` runs the contenders `orm`, `django-sync` and `django-async` (and `orm-all`, see below).

## Full run

```bash
bench/compare/run.sh                     # builds release artifacts, 20 rounds x 20 timed iterations, writes results/<date>-<sha>/summary.md
```

`run.sh` builds first, then waits up to 5 minutes for a 1-minute load of at most 2; each runner waits again before every round.
It marks `run.json` invalid (and `summary.md` shows an INVALID banner) when the load at the end is above 2.
`--ignore-load` and `--fast-build` are for a pipeline smoke: the directory gets a `-smoke` suffix, and the numbers are not valid.
The database is `bench` (`BENCH_DB_NAME` changes it); the run drops and recreates it, and drops it at the end.
`report.py` gives each cell as the median of the per-round medians with a 95% cluster bootstrap interval (rounds resampled), and each ratio as the median of the paired per-round ratios.

## Run one runner by hand

```bash
createdb -h localhost -p 55432 -U postgres bench          # a database of your own
export BENCH_DATABASE_URL=postgres://postgres:postgres@localhost:55432/bench
uv pip install django==6.1.2 'psycopg[binary]==3.3.6' psycopg-pool==3.3.3 asgiref==3.12.1   # into a venv of its own that has the `orm` wheel
cd bench/compare/python
python run.py check                                       # equality of results and database state, per case
python run.py run --rounds 2 --iters 5                    # check first, then timing; writes results/smoke/ (defaults: 20 x 20)
```

The runner resets the tables (`schema.sql`, `seed.sql`) before every process, so `BENCH_DATABASE_URL` must name a database that holds only benchmark data.
Each contender runs in its own process, once per round. The contender order rotates every round, and each case runs 50 untimed warm-up calls first, then the timed calls.
The equality check compares the result and the database state at iterations 0, 1 and the last one.
The output has one JSON file per contender (median, samples, batch medians, CPU split) and `environment.json` (versions, Postgres, load).

`--orm-artifact PATH` adds a row `orm-all`: it starts the `orm` runner with `PYTHONPATH=PATH/py` and `ORM_PROFILE=custom`, the `all` extension artifact of `scripts/feature-check.sh`.

## Read the numbers with care

- The `orm` wheel must be a release build with the postgres profile. A debug tooling build (the dev venv) runs, but its timings are not valid.
- Django has no async `atomic()`. The `tx-5` case on `django-async` runs a sync function through `sync_to_async`. Django async calls also serialize on one thread.
- The sync ORM cannot run in an event loop. The `django-sync` process runs without one; its `concurrent` case uses 50 threads.
- The CPU split is the time to build the query and to render its SQL (`.sql()` of `orm` inlines values, `str(queryset.query)` of Django does too). The decode time is not reachable in either library and is not measured. Treat the split as approximate.
- `filter-ne-null` is an extra case: `exclude(category_id=3)` in Django keeps the NULL rows, and `(Post.category_id != 3).or_null()` is the same in `orm`.
- Prepared statements: `orm` prepares once per connection and caches. psycopg prepares after 5 runs of a statement. `adapter-pg` of Prisma sends unnamed statements.
- `orm` returns full rows from `bulk-insert-*` (`returning()` has no key-only form); Prisma selects ids only. `summary.md` prints this and the other per-contender notes.
