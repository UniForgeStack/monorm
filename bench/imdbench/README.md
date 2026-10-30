# IMDBench run

This folder runs [IMDBench](https://github.com/geldata/imdbench) with MonOrm as two new contenders, and keeps the results.
`monorm.patch` holds 4 commits for the IMDBench repo: the MonOrm contenders (Python asyncio and Node.js) in `_monorm/`, small changes to `_shared.py`, `jsbench.js` and the `Makefile`, and a result check.
The contenders use the released packages: `monorm` 0.1.1 from PyPI and `@ufstack/monorm` 0.1.1 from npm.
Nothing in this repo is built for the run.

## Procedure

Run on a VM that does no other work at the same time.
Install any tool you need at its latest stable version: Python, Node.js, PostgreSQL client, Docker, Go, Rust, build tools.
Do not open a PR and post nothing to a third party.

### Source

```sh
git clone https://github.com/geldata/imdbench ~/imdbench && cd ~/imdbench
git switch -c monorm 88e5eb6
git -c user.name=bench -c user.email=bench@localhost am <this repo>/bench/imdbench/monorm.patch
```

Read `_monorm/README.md` first.
Install: `pip install monorm==0.1.1` in the IMDBench venv, and `npm install` in `_monorm/`.

### Correctness first

1. Load a small dataset and run `_monorm/tools/check_results.py` (see `_monorm/README.md`).
   It compares all 7 queries of MonOrm Python, MonOrm JS, SQLAlchemy and Prisma, and must print `RESULT: PASS`.
   If it fails, find the cause.
   Fix the contender for a contender bug.
   For a MonOrm bug, stop and write a report; do not change MonOrm.
2. Do not use `_monorm/tools/` for the timed run.

### Contenders

Run MonOrm and these references on the same VM, with the same dataset and settings:

- Python: `django sqlalchemy monorm postgres_asyncpg`
- JS: `typeorm prisma monorm_js postgres_pg`

Do not run Sequelize, EdgeDB, MongoDB, Drizzle, Hasura, PostGraphile or the Go and Dart contenders.
For EdgeDB and Sequelize, keep the upstream numbers in the merged files and mark them "upstream, 2023, other machine".

1. Use the upstream dataset: `make new-dataset` (upstream `synth`, default size), then `make load-postgres load-django load-sqlalchemy load-typeorm load-prisma load-monorm`.
   `load-postgres` serves Prisma, MonOrm and the raw drivers.
   Django, SQLAlchemy and TypeORM load their own databases.
   If one reference does not install after a reasonable try, skip it and record why.
2. Use the settings of the upstream published run (`docs/py.json`): `--concurrency 1 --duration 10 --net-latency 1`, and the queries `insert_movie`, `get_movie`, `get_user`.
   - Python: `python bench.py --query insert_movie --query get_movie --query get_user --concurrency 1 --duration 10 --net-latency 1 --json <out>/py-<n>.json --html <out>/py-<n>.html django sqlalchemy monorm postgres_asyncpg`
   - JS: the same with the JS list, output `js-<n>`.
3. Run each group 3 times.
   Use the run with the median requests/s per query and contender.
4. Before each run, check that the load average is under 1.

### Known differences

Write these in the report; do not fix them.

- MonOrm has no `avg()` in `load()`, so `get_user`, `get_movie` and `get_person` do one extra `select()` for the average rating.
- The MonOrm contender samples ids in Python or JS, not with SQL `random()`.
  This cost is in the setup, not in the timed queries.

### Output

Put everything in `bench/imdbench/results/<date>/` of this repo:

- `monorm-results.md`: one table per language, every contender of this VM, with requests/s, p50 and p99 latency per query (the median run).
  Mark every number as measured.
  Add the known differences above and any anomaly (errors, more than 10% variance between runs).
- `machine.md`: CPU, cores, RAM, disk, OS, kernel, and the versions of Python, Node, PostgreSQL and MonOrm.
  Also copy the upstream `platform` block of `docs/py.json` (AMD Ryzen Threadripper 3970X, 32 cores, 2023).
- `py.json`, `js.json`, `py.html`, `js.html`: copies of the upstream `docs/` files with MonOrm added.
  Merge in the median-run entries of this VM: MonOrm is a new entry; for a reference that ran here, add a new entry "<name> (this VM)" next to the upstream one.
  Keep every upstream entry unchanged, and make the HTML with the upstream `TEMPLATE.html` (see how `bench.py` and `merge.py` write it).
  Under each chart, write: "(this VM)" rows and MonOrm are measured on this VM (see machine.md); the other rows are upstream, 2023, on another machine.
  Compare MonOrm only with the "(this VM)" rows.
- `raw/`: the `--json` and `--html` files of each run.
- `contender.patch`: `git format-patch --stdout 88e5eb6..monorm` of the IMDBench clone, if you changed a contender.

Do not change the files in the IMDBench `docs/` folder.
Commit the results folder as one commit on `main` and push to `origin main`.
