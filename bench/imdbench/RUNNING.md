# Running IMDBench

The procedure for an agent that runs the IMDBench benchmark. The results are in [README.md](README.md).

This folder runs the [IMDBench](https://github.com/geldata/imdbench) scenarios with MonOrm (Python asyncio and Node.js) and the reference ORMs of each language, on one machine.
`monorm.patch` holds 7 commits for the IMDBench repo, ready for an upstream PR:

1. The MonOrm contenders in `_monorm/`.
2. Their registration in `_shared.py`, `jsbench.js` and the `Makefile`, and their docs.
3. A result check and local run tools (`_monorm/tools/`).
4. The released packages: `monorm` 0.1.1 from PyPI and `@ufstack/monorm` 0.1.1 from npm.
5. A fix to the result check: it picks people whose order is the same in both id spaces.
6. `jsbench.js` loads the EdgeDB and Sequelize apps on first use (neither builds here).
7. The Prisma `insert_movie` setup uses PostgreSQL syntax unless `IMDBENCH_MYSQL` is set (upstream uses MySQL `DELETE … JOIN`).

Nothing in this repo is built for the run.

## Procedure

Run on a VM that does no other work at the same time.
Install any tool you need at its latest stable version: Python, Node.js, PostgreSQL client, Docker, build tools.
Do not open a PR and post nothing to a third party.

### Source

```sh
git clone https://github.com/geldata/imdbench ~/imdbench && cd ~/imdbench
git switch -c monorm 88e5eb6
git -c user.name=bench -c user.email=bench@localhost am <this repo>/bench/imdbench/monorm.patch
```

Read `_monorm/README.md` first.
Install: `pip install monorm==<version>` in the IMDBench venv, and `npm install` in `_monorm/`.

### Correctness first

1. Load a small dataset and run `_monorm/tools/check_results.py` (see `_monorm/README.md`).
   It compares all 7 queries of MonOrm Python, MonOrm JS, SQLAlchemy and Prisma, and must print `RESULT: PASS`.
   Fix the contender for a contender bug.
   For a MonOrm bug, stop and write a report; do not change MonOrm.
2. Do not use `_monorm/tools/` for the timed run.

### Contenders

- Python: `django sqlalchemy monorm postgres_asyncpg`
- JS: `typeorm prisma monorm_js postgres_pg`

Do not run Sequelize, EdgeDB, MongoDB, Drizzle, Hasura, PostGraphile or the Go and Dart contenders.

1. Use the upstream dataset: `make new-dataset` (default size), then `make load-postgres load-django load-sqlalchemy load-typeorm load-prisma load-monorm`.
   `load-postgres` serves Prisma, MonOrm and the raw drivers.
2. Use the settings of the upstream published run: `--concurrency 1 --duration 10 --net-latency 1`, and the queries `insert_movie`, `get_movie`, `get_user`.
   - Python: `python bench.py --query insert_movie --query get_movie --query get_user --concurrency 1 --duration 10 --net-latency 1 --json <out>/py-<n>.json django sqlalchemy monorm postgres_asyncpg`
   - JS: the same with the JS list, output `js-<n>.json`.
3. Run each group 3 times, Python and JS alternating.
   Before each run, check that the load average is under 1, and append the start time and load to `runs.log`.
4. Use the run with the median requests/s per query and contender.

### Output

Put everything in `bench/imdbench/results/<date>-<sha>/` of this repo: the UTC date of the run and the 7-character `main` commit of this repo at that time. Then point [README.md](README.md) at it and delete the previous folder:

- `results.md`, in the shape of the latest one: verdict, one chart per query plus the geometric mean (ORMs of both languages, raw drivers in the table only), one table of every contender, the caveats, the method and the machine.
  Only rows measured on this machine; no upstream numbers.
  Mark every number as measured.
- `raw/`: the `--json` file and the console output of each run, and `runs.log`.

If you change a contender, regenerate `monorm.patch` with `git format-patch --stdout 88e5eb6..monorm`.
If the MonOrm numbers drop against the previous run on comparable hardware, write it down in the benchmark issue file of the plans folder before you delete the old folder.
