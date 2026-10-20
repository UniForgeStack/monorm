# Benchmark: our ORM against the libraries a user would pick instead

This folder holds the shared schema, the seed and the Python half (job `bench-python` of `plans/rust-api/perf-compare.md`).
The TypeScript and Rust halves are separate jobs.

- `schema.prisma` is the model. `schema.sql` is the same schema as SQL. `seed.sql` loads 1,000 users, 20 categories, 10,000 posts, 50,000 comments, 50 tags and 30,000 post-tag links.
- `python/run.py` runs the contenders `orm`, `django-sync` and `django-async` (and `orm-all`, see below).

## Run

```bash
createdb -h localhost -p 55432 -U postgres bench          # a database of your own
export BENCH_DATABASE_URL=postgres://postgres:postgres@localhost:55432/bench
uv pip install django 'psycopg[binary,pool]'              # into the venv that has the `orm` wheel
cd bench/compare/python
python run.py check                                       # equality of results and database state, per case
python run.py run --rounds 2 --iters 5                    # check first, then timing; writes results/smoke/
```

The runner resets the tables (`schema.sql`, `seed.sql`) before every process, so `BENCH_DATABASE_URL` must name a database that holds only benchmark data.
Each contender runs in its own process, once per round. The contender order rotates every round, and each process warms up first.
The output has one JSON file per contender (median, samples, batch medians, CPU split) and `environment.json` (versions, Postgres, load).

`--orm-artifact PATH` adds a row `orm-all`: it starts the `orm` runner with `PYTHONPATH=PATH/py` and `ORM_PROFILE=custom`, the `all` extension artifact of `scripts/feature-check.sh`.

## Read the numbers with care

- The `orm` wheel must be a release build with the postgres profile. A debug tooling build (the dev venv) runs, but its timings are not valid.
- Django has no async `atomic()`. The `tx-5` case on `django-async` runs a sync function through `sync_to_async`. Django async calls also serialize on one thread.
- The sync ORM cannot run in an event loop. The `django-sync` process runs without one; its `concurrent` case uses 50 threads.
- The CPU split is the time to build the query and to render its SQL (`.sql()` of `orm` inlines values, `str(queryset.query)` of Django does too). The decode time is not reachable in either library and is `null`.
- `filter-ne-null` is an extra case: `exclude(category_id=3)` in Django keeps the NULL rows, and `(Post.category_id != 3).or_null()` is the same in `orm`.
- Prepared statements: `orm` prepares once per connection and caches. psycopg prepares after 5 runs of a statement.
