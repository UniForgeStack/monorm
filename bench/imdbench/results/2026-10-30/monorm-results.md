# MonOrm in IMDBench, 2026-10-30

IMDBench (`geldata/imdbench` at `88e5eb6`) with `bench/imdbench/monorm.patch`, run on one VM (see `machine.md`).
MonOrm 0.1.1 from PyPI and npm.
Settings of the upstream published run: `--concurrency 1 --duration 10 --net-latency 1`, queries `insert_movie`, `get_movie`, `get_user`, upstream dataset of default size.
Each group ran 3 times, alternating Python and JS (`raw/runs.log`).
Each row is the run with the median requests/s for that query and contender; the latencies are from that same run.
Every number below is measured on this VM.
The geometric mean rows are computed with `bench.py`'s `mean_latency_stats` over the three median-run rows.

## Results

### Python

| Contender | Query | Requests/s | p50 (ms) | p99 (ms) | Median run | Requests/s of runs 1, 2, 3 | Spread |
|---|---|---:|---:|---:|---:|---|---:|
| Django ORM | `insert_movie` | 54.4 (measured) | 17.44 (measured) | 31.921 (measured) | 1 | 54.4, 55.4, 49.1 | 11.6% |
| Django ORM | `get_movie` | 138.6 (measured) | 6.727 (measured) | 13.773 (measured) | 1 | 138.6, 139.8, 129.8 | 7.2% |
| Django ORM | `get_user` | 129.0 (measured) | 7.195 (measured) | 17.052 (measured) | 3 | 128.3, 133.3, 129.0 | 3.9% |
| Django ORM | geometric mean | 99.08 (measured) | 9.451 (measured) | 19.572 (measured) | | | |
| SQLAlchemy | `insert_movie` | 126.7 (measured) | 7.12 (measured) | 16.627 (measured) | 1 | 126.7, 121.8, 139.3 | 13.8% |
| SQLAlchemy | `get_movie` | 145.5 (measured) | 6.316 (measured) | 15.539 (measured) | 2 | 164.8, 145.5, 140.3 | 16.8% |
| SQLAlchemy | `get_user` | 432.4 (measured) | 2.112 (measured) | 5.405 (measured) | 1 | 432.4, 406.6, 458.6 | 12.0% |
| SQLAlchemy | geometric mean | 199.76 (measured) | 4.563 (measured) | 11.177 (measured) | | | |
| MonOrm (Python, asyncio) | `insert_movie` | 229.7 (measured) | 3.966 (measured) | 10.161 (measured) | 3 | 259.0, 221.0, 229.7 | 16.5% |
| MonOrm (Python, asyncio) | `get_movie` | 232.5 (measured) | 3.982 (measured) | 9.945 (measured) | 1 | 232.5, 208.7, 256.2 | 20.4% |
| MonOrm (Python, asyncio) | `get_user` | 422.2 (measured) | 2.144 (measured) | 5.753 (measured) | 1 | 422.2, 388.1, 451.9 | 15.1% |
| MonOrm (Python, asyncio) | geometric mean | 282.51 (measured) | 3.235 (measured) | 8.346 (measured) | | | |
| PostgreSQL (Python, asyncpg) | `insert_movie` | 1048.6 (measured) | 0.866 (measured) | 2.876 (measured) | 3 | 1005.5, 1068.8, 1048.6 | 6.0% |
| PostgreSQL (Python, asyncpg) | `get_movie` | 896.1 (measured) | 0.999 (measured) | 3.33 (measured) | 2 | 888.9, 896.1, 950.3 | 6.9% |
| PostgreSQL (Python, asyncpg) | `get_user` | 1352.9 (measured) | 0.655 (measured) | 2.342 (measured) | 2 | 1453.6, 1352.9, 1213.1 | 17.8% |
| PostgreSQL (Python, asyncpg) | geometric mean | 1083.29 (measured) | 0.828 (measured) | 2.82 (measured) | | | |

### JavaScript (Node.js)

| Contender | Query | Requests/s | p50 (ms) | p99 (ms) | Median run | Requests/s of runs 1, 2, 3 | Spread |
|---|---|---:|---:|---:|---:|---|---:|
| TypeORM | `insert_movie` | 171.17 (measured) | 5.203 (measured) | 13.138 (measured) | 1 | 171.17, 154.16, 175.19 | 12.3% |
| TypeORM | `get_movie` | 42.6 (measured) | 22.04 (measured) | 57.675 (measured) | 3 | 45.99, 40.79, 42.6 | 12.2% |
| TypeORM | `get_user` | 742.06 (measured) | 1.17 (measured) | 4.356 (measured) | 3 | 848.43, 656.02, 742.06 | 25.9% |
| TypeORM | geometric mean | 175.56 (measured) | 5.119 (measured) | 14.889 (measured) | | | |
| Prisma | `insert_movie` | 200.32 (measured) | 4.753 (measured) | 8.959 (measured) | 3 | 204.09, 181.91, 200.32 | 11.1% |
| Prisma | `get_movie` | 157.92 (measured) | 6.019 (measured) | 12.234 (measured) | 2 | 158.59, 157.92, 156.54 | 1.3% |
| Prisma | `get_user` | 397.49 (measured) | 2.316 (measured) | 6.445 (measured) | 1 | 397.49, 463.67, 349.96 | 28.6% |
| Prisma | geometric mean | 232.54 (measured) | 4.046 (measured) | 8.906 (measured) | | | |
| MonOrm (Node.js) | `insert_movie` | 283.01 (measured) | 3.258 (measured) | 8.004 (measured) | 3 | 279.25, 303.72, 283.01 | 8.6% |
| MonOrm (Node.js) | `get_movie` | 344.49 (measured) | 2.731 (measured) | 6.605 (measured) | 3 | 346.14, 318.52, 344.49 | 8.0% |
| MonOrm (Node.js) | `get_user` | 576.21 (measured) | 1.629 (measured) | 4.847 (measured) | 3 | 550.11, 628.04, 576.21 | 13.5% |
| MonOrm (Node.js) | geometric mean | 382.99 (measured) | 2.438 (measured) | 6.352 (measured) | | | |
| PostgreSQL (Node.js, pg) | `insert_movie` | 291.75 (measured) | 3.065 (measured) | 8.746 (measured) | 3 | 328.72, 270.07, 291.75 | 20.1% |
| PostgreSQL (Node.js, pg) | `get_movie` | 284.75 (measured) | 3.253 (measured) | 8.587 (measured) | 3 | 295.13, 258.67, 284.75 | 12.8% |
| PostgreSQL (Node.js, pg) | `get_user` | 963.51 (measured) | 0.929 (measured) | 3.228 (measured) | 2 | 973.46, 963.51, 922.15 | 5.3% |
| PostgreSQL (Node.js, pg) | geometric mean | 430.97 (measured) | 2.1 (measured) | 6.235 (measured) | | | |
## Summary

Geometric mean of requests/s over the three queries (median runs, this VM only):

| | MonOrm | vs. reference |
|---|---:|---|
| Python | 282.5 | 1.41× SQLAlchemy (199.8), 2.85× Django (99.1), 0.26× raw asyncpg (1083.3) |
| JS | 383.0 | 1.65× Prisma (232.5), 2.18× TypeORM (175.6), 0.89× raw pg (431.0) |

For `get_user` in Python, MonOrm (422.2) and SQLAlchemy (432.4) are within the run-to-run spread.
MonOrm JS is above raw `pg` for `get_movie` (344.5 vs 284.8) and 3% below it for `insert_movie` (283.0 vs 291.8); the spread of `pg` on these queries (12.8% and 20.1%) is as large as or larger than those differences.

## Known differences

- MonOrm has no `avg()` in `load()`, so `get_user`, `get_movie` and `get_person` do one extra `select()` for the average rating.
- The MonOrm contender samples ids in Python or JS, not with SQL `random()`.
  This cost is in the setup, not in the timed queries.

## Anomalies and deviations

- **Variance over 10%.** 16 of the 24 query/contender pairs have a spread (max − min requests/s over the 3 runs, divided by the median) above 10%; see the Spread column.
  The largest: Prisma `get_user` 28.6%, TypeORM `get_user` 25.9%, MonOrm Python `get_movie` 20.4%, raw pg `insert_movie` 20.1%.
  The VM is a 4-vCPU KVM guest; the noise affects MonOrm and the references alike.
- **Errors.** None. Every run finished, and every median row has a non-zero query count (`raw/*.log`).
- **No network delay.** `--net-latency 1` was passed, but no 1 ms delay was applied (no `netem` in this kernel; see `machine.md`).
  The upstream rows may have run with a 1 ms delay. This is another reason to compare MonOrm only with the "(this VM)" rows.
- **Result check.** `_monorm/tools/check_results.py` first failed on `insert_movie` (MonOrm vs SQLAlchemy). The cause was in the check, not in MonOrm or a contender:
  SQLAlchemy loads the people with other ids, and each contender makes the person with the lowest id in its own table the director.
  So the same 4 people got different roles (Prisma agreed with MonOrm).
  The check now picks 4 people whose order is the same in both id spaces; then it printed `RESULT: PASS` for all 7 queries.
- **Prisma reference.** The upstream `_prisma` `setup` for `insert_movie` uses MySQL `DELETE D FROM … JOIN` syntax (since the upstream PlanetScale commit, after the 2023 run) and fails on PostgreSQL.
  Only this untimed cleanup step was changed to PostgreSQL syntax when `IMDBENCH_MYSQL` is not set; the timed queries are unchanged.
- **jsbench.js.** It loads all JS apps at start. `_edgedb_js` needs a query builder generated from a running EdgeDB, and the native `libpq` addon of `_sequelize` does not build on Node.js 22.
  Neither is run here, so both are now loaded on first use.
- **Python EdgeDB client.** `_shared.py` imports the `edgedb` package; the pinned `edgedb~=1.3.0` does not build on Python 3.13, so `edgedb` 2.2.0 was installed. EdgeDB was not run.
- All changes to the IMDBench clone are in `contender.patch` (`git format-patch --stdout 88e5eb6..monorm`, the 4 patch commits plus 3 new ones).

## Merged files

`py.json`, `js.json`, `py.html` and `js.html` are the upstream `docs/` files with the median-run entries of this VM added:
MonOrm as a new entry, "<name> (this VM)" next to each upstream reference that ran here, and the raw drivers (not in the upstream `py`/`js` files) as "(this VM)" entries at the end.
Every upstream entry is unchanged, including EdgeDB and Sequelize: upstream, 2023, other machine.
The JSON files have a `sources` map (each entry: "measured on this VM" or "upstream, 2023, other machine"), a `platform_this_vm` block and a `note`.
The HTML is rendered with the upstream `TEMPLATE.html` through `bench.format_report_html`; under each chart it says:
"(this VM)" rows and MonOrm are measured on this VM (see machine.md); the other rows are upstream, 2023, on another machine.
Compare MonOrm only with the "(this VM)" rows.

`raw/` holds the `--json`, `--html` and console output of each run, and `runs.log` with the load average at each start.
