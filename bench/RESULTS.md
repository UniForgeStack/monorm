# Benchmark results

## What to cite

All entries are measured results on the recorded machines, not new measurements.
“Current” identifies the retained engine or implementation, not a fresh run of HEAD.
The [Python and TypeScript comparison](#python-and-typescript-versus-django-and-prisma--2026-10-21)
compares the Python API with Django and the TypeScript API with Prisma and raw `pg`.
No recorded run includes SQLAlchemy or Drizzle yet; `bench/compare/` has those contenders.

| Section | Status | Engine / implementation measured | Comparison it supports |
|---|---|---|---|
| [Python and TypeScript versus Django and Prisma](#python-and-typescript-versus-django-and-prisma--2026-10-21) | Current comparison to cite | Public Python and TypeScript APIs, release builds at `c9bbf7c` | Python versus Django sync / async; Node versus Prisma and the `pg` floor; Bun versus Prisma (three cases) |
| [Build-time extension gate](#build-time-extension-gate--2026-10-08) | Current | Build-time extensions on the Rust engine | Extension cost versus handwritten controls; gate has not passed |
| [Advisory locks](#advisory-locks-in-rust--2026-10-04) | Current | Rust lock helper through Python / TypeScript | Paired change in lock-key hashing and locking |
| [Query construction](#query-construction-in-rust--2026-10-04) | Historical, rejected | Removed direct operation decoder | Experiment only; no current API claim |
| [Phase 0](#phase-0-performance-feasibility-2026-10-02) | Historical | Removed SeaORM `ormcore` prototype | No current-engine comparison; do not cite its cross-language ratios |
| [Summary](#summary) | Historical | Removed SeaORM prototype | Prototype feasibility decisions only |
| [Cross-language](#cross-language-median-ms-per-call-unix-socket) | Historical | Removed SeaORM prototype | No current Python / TypeScript comparison |
| [Language notes](#what-each-language-teaches) | Historical | Removed SeaORM prototype | Interpretation of prototype results only |
| [Transport](#transport-unix-socket-vs-tcp) | Historical | Removed SeaORM prototype and vendored SQLx patch | Old transport experiment only |
| [Async versus sync](#async-vs-sync) | Historical | Removed SeaORM prototype | No current sync API claim |
| [Notes and caveats](#notes-and-caveats) | Historical | Removed SeaORM prototype | Limits of the prototype measurements |
| [Full results](#full-results) | Historical | Removed SeaORM prototype | Archived prototype data only |
| [Engine swap](#engine-seaorm-vs-tokio-postgres) | Historical | SeaORM prototype versus initial tokio-postgres engine | Engine-swap experiment only |
| [Instances built in Rust](#instances-built-in-rust) | Historical | Early tokio-postgres engine commits | Object-construction change at the named commits |
| [Query profile](#where-a-querys-time-goes), including hand-off, plan cache and schema JSON | Historical | Early tokio-postgres engine | Profiling and experiments at the named commits |
| [Prepared queries](#prepared-queries) | Historical | Early tokio-postgres engine | Prepared-query experiment only |
| [Rust versus SeaORM](#current-rust-engine-versus-seaorm--2026-10-03), including Completed verification | Superseded | Current Rust engine versus SeaORM with an extra checkout ping | Replaced by matched settings below |
| [Diesel added](#diesel-added--2026-10-03) | Superseded | Current Rust engine, Diesel async and unmatched SeaORM | Replaced by matched settings below |
| [Pool-setting correction](#diesel-investigation-and-pool-setting-correction--2026-10-03) and [matched ORM table](#full-orm-comparison-with-matched-health-check-settings) | Current comparison to cite | Rust engine, SeaORM / SQLx and Diesel async, matched checkout settings | Five equal Rust-only workloads; no Python / TypeScript overhead |
| [Read-path diagnosis](#read-path-diagnosis-including-raw-sqlx) and [Diesel lessons](#what-we-can-learn-from-diesel) | Current diagnosis to cite | Same engine plus raw SQLx and tokio-postgres variants | Driver, checkout and decoding costs on equal Rust read results |

All numbers here are measured. Each section names its date, commit (where recorded) and machine.
How to run each benchmark: [`README.md`](README.md).

| Date | Benchmark | Commit | Machine | Result |
|---|---|---|---|---|
| 2026-10-21 | [Python and TypeScript versus Django and Prisma](#python-and-typescript-versus-django-and-prisma--2026-10-21) | `c9bbf7c` | Cloud VM, Intel Xeon 2.80GHz, 4 cores, 16 GB, Ubuntu 24.04.5, PostgreSQL 16.15; load 1.06 to 1.98 | Valid run; see the ratio summary in the section |
| 2026-10-08 | [Build-time extension gate](#build-time-extension-gate--2026-10-08) | `10663b5` | Apple M5, macOS, Docker PostgreSQL; load 1.9 to 4.3 | Not passed: 0 regressions, 20/44 accepted, 24 inconclusive |
| 2026-10-08 | Build-time extension gate, jury-4 (`build-time-extensions/results/jury-4/gate/`) | `b398168` | same, load 11 to 23 | Not passed: 0 regressions, 4/44 accepted, 40 inconclusive |
| 2026-10-04 | [Advisory locks in Rust](#advisory-locks-in-rust--2026-10-04) | `909680a` (results commit) | macOS arm64, isolated PostgreSQL (port 55439) | Passed: a gain in both languages, no regression |
| 2026-10-04 | [Query construction in Rust](#query-construction-in-rust--2026-10-04) | `d9f70c0` (results commit) | macOS arm64, PostgreSQL 18.6, CPython 3.14.7, Node 26.9 | Rejected: an error compatibility change |
| 2026-10-03 | [Current Rust engine versus SeaORM and Diesel](#current-rust-engine-versus-seaorm--2026-10-03) | not recorded | macOS arm64, PostgreSQL 18.6 | See the section; the correction subsection replaces earlier SeaORM ratios |
| 2026-10-02/03 | Phase 0, engine change, instances in Rust, query profile, prepared queries (below) | not recorded | 4-vCPU Xeon Linux VM, PostgreSQL 16.14 | Historical: earlier engines |

## Python and TypeScript versus Django and Prisma — 2026-10-21

`bench/compare`: the public Python and TypeScript APIs of `orm` against Django 6.1.2 (sync and async), Prisma 7.10.1 and raw `pg` 8.23.1 (the floor), on one shared schema and seed. The run (`bench/compare/run.sh`, no flags) used release builds at `c9bbf7c`: 20 rounds × 20 timed iterations, 50 untimed warm-up calls per case, one fresh process per contender and round, rotated order.
Results directory: [`compare/results/2026-10-21-c9bbf7c/`](compare/results/2026-10-21-c9bbf7c/summary.md) (`summary.md`, raw JSON per contender, `environment.json`, logs).

Machine (from `environment.json`, `lscpu` and `/proc/meminfo`): cloud VM `vm`, Intel Xeon Processor @ 2.80GHz, 4 cores (1 thread per core), 16 GB RAM, Ubuntu 24.04.5 LTS (Linux 6.18.44, x86_64), PostgreSQL 16.15 over TCP on localhost. CPython 3.14.6, Node v22.22.0, Bun 1.4.2, rustc 1.97.0.

All numbers below are **measured** and copied from `summary.md` unchanged. Each cell is `median [95% cluster bootstrap interval]` of the per-round medians; each ratio is the median of the paired per-round ratios with its interval. `run.json` marks the run valid: the 1-minute load was 1.13 at the start and 1.25 at the end, and 1.06 to 1.98 before each round (limit 2). The machine is a shared cloud VM, so the load sat close to the limit for much of the run; treat small differences (intervals that overlap 1.00) as no difference.

Summary of the ratios to each side's reference, the `pg` floor on Node (measured; the lowest and highest case of the 20, see the tables for each case):

- Python, reference `django-async` (no `pg`-like floor on Python): `orm / django-async` is 0.23 (`m2m`) to 0.92 (`tx-5`); `django-sync / django-async` is 0.67 to 1.04.
- TypeScript on Node, floor `pg`: `orm / pg` is 1.00 (`read-1000`) to 2.24 (`projection-1000`); `prisma / pg` is 1.20 (`bulk-update-100`) to 22.01 (`upsert-100`).
- Bun, reference `prisma`: `orm / prisma` is 0.27 to 0.59 on the three cases.
- `orm-all / orm` (every extension built in) is 0.98 to 1.15 on Python and 0.91 to 1.07 on Node.

### Read the numbers with care

From [`compare/README.md`](compare/README.md#read-the-numbers-with-care):

- The `orm` wheel must be a release build with the postgres profile. A debug tooling build (the dev venv) runs, but its timings are not valid.
- Django has no async `atomic()`. The `tx-5` case on `django-async` runs a sync function through `sync_to_async`. Django async calls also serialize on one thread.
- The sync ORM cannot run in an event loop. The `django-sync` process runs without one; its `concurrent` case uses 50 threads.
- The CPU split is the time to build the query and to render its SQL (`.sql()` of `orm` inlines values, `str(queryset.query)` of Django does too). The decode time is not reachable in either library and is not measured. Treat the split as approximate.
- `filter-ne-null` is an extra case: `exclude(category_id=3)` in Django keeps the NULL rows, and `(Post.category_id != 3).or_null()` is the same in `orm`.
- Prepared statements: `orm` prepares once per connection and caches. psycopg prepares after 5 runs of a statement. `adapter-pg` of Prisma sends unnamed statements.
- `orm` returns full rows from `bulk-insert-*` (`returning()` has no key-only form); Prisma selects ids only. `summary.md` prints this and the other per-contender notes.

### Python

orm 0.1.0, django 6.1.2, psycopg 3.3.6, psycopg-binary 3.3.6, psycopg-pool 3.3.3, asgiref 3.12.1. orm: release build, revision c9bbf7c; orm-all: release build, revision c9bbf7c.

Reference: `django-async`. There is no `pg`-like floor contender on Python. `orm-all` is the `orm` build with every extension; the `orm-all / orm` column compares the two.

#### Time per operation

| case | orm (ms) | orm-all (ms) | django-sync (ms) | django-async (ms) | orm / django-async | orm-all / django-async | django-sync / django-async | orm-all / orm |
|---|---|---|---|---|---|---|---|---|
| read-pk | 0.542 [0.532, 0.57] | 0.599 [0.575, 0.634] | 0.603 [0.565, 0.667] | 0.856 [0.804, 0.908] | 0.64 [0.57, 0.69] | 0.71 [0.64, 0.78] | 0.73 [0.60, 0.80] | 1.10 [1.04, 1.24] |
| read-50 | 0.741 [0.697, 0.775] | 0.801 [0.755, 0.914] | 1 [0.907, 1.05] | 1.35 [1.3, 1.41] | 0.53 [0.49, 0.59] | 0.60 [0.57, 0.67] | 0.73 [0.68, 0.81] | 1.15 [1.03, 1.20] |
| read-1000 | 3.18 [3.03, 3.25] | 3.24 [3, 3.45] | 8.5 [8.11, 8.76] | 9.64 [9.5, 10.1] | 0.33 [0.30, 0.34] | 0.32 [0.31, 0.36] | 0.86 [0.80, 0.91] | 0.98 [0.93, 1.08] |
| projection-1000 | 2.73 [2.66, 2.86] | 2.92 [2.67, 3.1] | 5.92 [5.73, 6.19] | 7.08 [6.87, 7.44] | 0.39 [0.35, 0.40] | 0.40 [0.36, 0.43] | 0.84 [0.79, 0.88] | 1.06 [0.94, 1.15] |
| filter | 0.781 [0.741, 0.843] | 0.793 [0.763, 0.862] | 1.08 [1.06, 1.13] | 1.52 [1.38, 1.67] | 0.51 [0.49, 0.57] | 0.51 [0.49, 0.57] | 0.73 [0.64, 0.78] | 1.01 [0.99, 1.07] |
| filter-ne-null | 0.767 [0.744, 0.788] | 0.825 [0.794, 0.868] | 1.02 [1.01, 1.06] | 1.41 [1.35, 1.5] | 0.53 [0.50, 0.59] | 0.61 [0.55, 0.62] | 0.72 [0.68, 0.77] | 1.09 [0.98, 1.18] |
| count | 1.07 [1.01, 1.12] | 1.13 [1.05, 1.25] | 1.27 [1.22, 1.3] | 1.67 [1.61, 1.77] | 0.64 [0.59, 0.68] | 0.68 [0.60, 0.78] | 0.74 [0.66, 0.81] | 1.03 [0.95, 1.18] |
| aggregate | 3.48 [3.35, 3.73] | 3.67 [3.43, 3.91] | 4.31 [4.1, 4.45] | 5.3 [5.07, 5.56] | 0.69 [0.64, 0.72] | 0.70 [0.62, 0.78] | 0.85 [0.74, 0.90] | 1.05 [0.93, 1.18] |
| join-to-one | 1.47 [1.39, 1.51] | 1.51 [1.42, 1.58] | 2.04 [1.94, 2.2] | 2.58 [2.42, 2.86] | 0.54 [0.52, 0.60] | 0.57 [0.48, 0.62] | 0.80 [0.70, 0.84] | 1.03 [0.99, 1.14] |
| prefetch-to-many | 2.96 [2.58, 3.12] | 2.91 [2.72, 3.14] | 8.64 [8.1, 9.35] | 9.78 [9.1, 10.1] | 0.29 [0.28, 0.32] | 0.30 [0.28, 0.32] | 0.87 [0.85, 0.92] | 1.05 [0.95, 1.12] |
| m2m | 1.41 [1.3, 1.47] | 1.45 [1.39, 1.65] | 5.39 [4.82, 5.92] | 5.89 [5.59, 6.33] | 0.23 [0.21, 0.26] | 0.26 [0.24, 0.27] | 0.87 [0.77, 0.96] | 1.06 [0.97, 1.18] |
| insert-1 | 0.937 [0.876, 1.01] | 0.981 [0.901, 1.01] | 0.942 [0.884, 0.997] | 1.29 [1.21, 1.41] | 0.71 [0.65, 0.77] | 0.77 [0.70, 0.82] | 0.67 [0.65, 0.82] | 1.02 [0.96, 1.07] |
| bulk-insert-100 | 3.9 [3.66, 4.02] | 3.72 [3.5, 4.54] | 6.32 [6.05, 6.63] | 7.12 [6.48, 7.83] | 0.54 [0.49, 0.60] | 0.56 [0.49, 0.61] | 0.89 [0.78, 0.98] | 1.05 [0.89, 1.17] |
| bulk-insert-1000 | 25.1 [24.2, 25.7] | 25.8 [25.2, 26.7] | 44.5 [44, 45.6] | 47.4 [46.3, 48.1] | 0.54 [0.52, 0.55] | 0.54 [0.53, 0.56] | 0.95 [0.93, 0.97] | 1.05 [0.99, 1.08] |
| update-1 | 0.927 [0.866, 1.03] | 1.06 [0.996, 1.11] | 1.03 [0.966, 1.12] | 1.52 [1.4, 1.74] | 0.59 [0.54, 0.68] | 0.64 [0.59, 0.77] | 0.68 [0.58, 0.82] | 1.12 [1.00, 1.24] |
| bulk-update-100 | 2.32 [2.17, 2.54] | 2.51 [2.23, 2.82] | 2.57 [2.27, 2.72] | 3.48 [3.02, 4.01] | 0.69 [0.58, 0.80] | 0.68 [0.60, 0.96] | 0.71 [0.60, 0.87] | 1.09 [1.04, 1.19] |
| upsert-100 | 2.39 [2.34, 2.75] | 2.44 [2.35, 2.74] | 4.19 [4.03, 4.59] | 5.47 [5.01, 5.68] | 0.45 [0.40, 0.53] | 0.47 [0.43, 0.53] | 0.77 [0.74, 0.87] | 1.02 [0.91, 1.08] |
| delete-100 | 1.67 [1.51, 1.93] | 1.74 [1.49, 1.89] | 2.35 [2.08, 2.38] | 3.04 [2.77, 3.26] | 0.55 [0.50, 0.64] | 0.59 [0.52, 0.64] | 0.75 [0.68, 0.87] | 0.99 [0.81, 1.23] |
| tx-5 | 5.09 [4.69, 5.55] | 5.44 [4.98, 5.99] | 4.83 [4.49, 5.17] | 5.61 [5.02, 6.17] | 0.92 [0.80, 1.02] | 0.99 [0.88, 1.11] | 0.83 [0.77, 0.99] | 1.03 [0.98, 1.19] |
| concurrent | 90.7 [82.8, 93.1] | 94.3 [90.8, 97.8] | 246 [242, 255] | 241 [235, 249] | 0.36 [0.35, 0.39] | 0.39 [0.37, 0.41] | 1.04 [1.01, 1.06] | 1.04 [1.00, 1.14] |

#### concurrent (50 tasks, 4 operations each, pool of 10)

| contender | ops/s | p99 latency (ms) |
|---|---|---|
| orm | 2206 | 45.1 |
| orm-all | 2120 | 45.4 |
| django-sync | 814 | 206 |
| django-async | 831 | 91 |

#### CPU split (approximate)

Measured with no database, mean of 20 repetitions, median over rounds. Approximate: `.sql()` and Django's `str(query)` inline the values, so the split is not the work of a real call. Decode time is not measured.

**orm**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.00509 | 0.0244 |
| read-50 | 0.00352 | 0.0235 |
| read-1000 | 0.00414 | 0.0292 |
| projection-1000 | 0.0125 | 0.0404 |
| filter | 0.0113 | 0.0574 |
| filter-ne-null | 0.0166 | 0.0608 |
| aggregate | 0.0107 | 0.0443 |
| join-to-one | 0.011 | 0.046 |
| prefetch-to-many | 0.0136 | 0.0552 |
| m2m | 0.0124 | 0.0578 |

**orm-all**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.00612 | 0.0278 |
| read-50 | 0.00359 | 0.0253 |
| read-1000 | 0.00422 | 0.0263 |
| projection-1000 | 0.0127 | 0.0424 |
| filter | 0.0109 | 0.0574 |
| filter-ne-null | 0.016 | 0.0659 |
| aggregate | 0.0128 | 0.0495 |
| join-to-one | 0.0124 | 0.0495 |
| prefetch-to-many | 0.0144 | 0.0617 |
| m2m | 0.0131 | 0.0575 |

**django-sync**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.0377 | 0.123 |
| read-50 | 0.0117 | 0.153 |
| read-1000 | 0.0148 | 0.153 |
| projection-1000 | 0.0203 | 0.119 |
| filter | 0.0796 | 0.365 |
| filter-ne-null | 0.0838 | 0.312 |
| aggregate | 0.103 | 0.267 |
| join-to-one | 0.0237 | 0.285 |
| prefetch-to-many | 0.0186 | 0.118 |
| m2m | 0.0182 | 0.193 |

**django-async**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.0394 | 0.117 |
| read-50 | 0.0145 | 0.161 |
| read-1000 | 0.0148 | 0.196 |
| projection-1000 | 0.0212 | 0.138 |
| filter | 0.0799 | 0.357 |
| filter-ne-null | 0.0854 | 0.324 |
| aggregate | 0.104 | 0.269 |
| join-to-one | 0.0248 | 0.363 |
| prefetch-to-many | 0.0208 | 0.139 |
| m2m | 0.0201 | 0.232 |

### TypeScript on Node

pg 8.23.1, prisma 7.10.1, @prisma/client 7.10.1, @prisma/adapter-pg 7.10.1. orm: release build, revision c9bbf7c; orm-all: release build, revision c9bbf7c.

Reference: `pg` (prepared statements, no ORM work), the floor. The `orm-all / orm` column compares the two builds.

#### Time per operation

| case | orm (ms) | orm-all (ms) | prisma (ms) | pg (ms) | orm / pg | orm-all / pg | prisma / pg | orm-all / orm |
|---|---|---|---|---|---|---|---|---|
| read-pk | 0.342 [0.316, 0.373] | 0.328 [0.325, 0.354] | 0.667 [0.648, 0.74] | 0.234 [0.223, 0.26] | 1.44 [1.34, 1.55] | 1.44 [1.33, 1.55] | 2.82 [2.59, 3.15] | 1.00 [0.92, 1.06] |
| read-50 | 0.671 [0.644, 0.698] | 0.671 [0.654, 0.738] | 1.11 [1, 1.15] | 0.611 [0.583, 0.659] | 1.08 [0.97, 1.20] | 1.14 [0.98, 1.23] | 1.81 [1.55, 2.02] | 1.07 [0.98, 1.12] |
| read-1000 | 4.29 [4.02, 4.33] | 4.19 [4.06, 4.47] | 6.98 [6.69, 7.08] | 4.24 [4.1, 4.34] | 1.00 [0.96, 1.03] | 0.98 [0.95, 1.07] | 1.68 [1.57, 1.75] | 0.99 [0.94, 1.07] |
| projection-1000 | 3.75 [3.49, 3.89] | 3.65 [3.51, 3.94] | 2.68 [2.44, 3.09] | 1.63 [1.54, 1.73] | 2.24 [2.17, 2.41] | 2.35 [2.16, 2.50] | 1.71 [1.44, 1.99] | 1.02 [0.97, 1.09] |
| filter | 0.542 [0.501, 0.593] | 0.555 [0.495, 0.604] | 0.784 [0.743, 0.837] | 0.334 [0.315, 0.397] | 1.54 [1.38, 1.71] | 1.54 [1.47, 1.67] | 2.22 [2.10, 2.58] | 1.01 [0.95, 1.10] |
| filter-ne-null | 0.537 [0.508, 0.593] | 0.572 [0.543, 0.587] | 0.762 [0.722, 0.829] | 0.353 [0.302, 0.434] | 1.56 [1.38, 1.76] | 1.57 [1.33, 1.75] | 2.09 [1.93, 2.52] | 1.01 [0.96, 1.12] |
| count | 0.833 [0.817, 0.88] | 0.874 [0.838, 0.946] | 1.09 [1.03, 1.12] | 0.734 [0.715, 0.782] | 1.10 [1.06, 1.19] | 1.15 [1.10, 1.27] | 1.43 [1.41, 1.51] | 0.99 [0.95, 1.12] |
| aggregate | 3.6 [3.26, 3.93] | 3.72 [3.51, 4.13] | 5.08 [4.76, 5.29] | 3.09 [2.94, 3.15] | 1.15 [1.09, 1.25] | 1.23 [1.13, 1.31] | 1.62 [1.53, 1.76] | 1.07 [0.91, 1.21] |
| join-to-one | 1.27 [1.19, 1.37] | 1.31 [1.24, 1.35] | 1.94 [1.89, 2.05] | 1.13 [1.08, 1.17] | 1.12 [1.05, 1.21] | 1.19 [1.11, 1.24] | 1.77 [1.63, 1.96] | 1.06 [0.91, 1.10] |
| prefetch-to-many | 3 [2.76, 3.12] | 2.96 [2.66, 3.12] | 5 [4.58, 5.34] | 2.81 [2.63, 2.91] | 1.05 [0.98, 1.14] | 1.05 [0.96, 1.10] | 1.77 [1.68, 1.89] | 1.02 [0.95, 1.06] |
| m2m | 1.43 [1.35, 1.53] | 1.34 [1.29, 1.44] | 3.08 [2.9, 3.43] | 1.14 [1.03, 1.2] | 1.29 [1.15, 1.45] | 1.24 [1.10, 1.29] | 2.76 [2.61, 3.07] | 0.92 [0.86, 1.00] |
| insert-1 | 0.737 [0.662, 0.793] | 0.742 [0.675, 0.822] | 1.08 [0.998, 1.16] | 0.652 [0.621, 0.691] | 1.13 [1.04, 1.35] | 1.21 [1.04, 1.34] | 1.63 [1.44, 1.96] | 1.01 [0.91, 1.16] |
| bulk-insert-100 | 3.82 [3.47, 3.94] | 3.63 [3.32, 3.98] | 13.2 [12.6, 14.4] | 3 [2.87, 3.18] | 1.21 [1.17, 1.27] | 1.20 [1.08, 1.37] | 4.58 [4.07, 4.94] | 1.01 [0.89, 1.08] |
| bulk-insert-1000 | 26.5 [25.7, 27.3] | 26 [24.4, 27.8] | 114 [110, 116] | 21.2 [20.7, 22.1] | 1.22 [1.16, 1.32] | 1.24 [1.15, 1.30] | 5.33 [4.95, 5.56] | 0.97 [0.92, 1.04] |
| update-1 | 0.735 [0.677, 0.819] | 0.709 [0.651, 0.769] | 0.975 [0.89, 1.04] | 0.556 [0.514, 0.604] | 1.42 [1.23, 1.54] | 1.29 [1.14, 1.40] | 1.82 [1.60, 2.09] | 0.99 [0.76, 1.14] |
| bulk-update-100 | 2.13 [1.96, 2.38] | 2.1 [2.01, 2.28] | 2.48 [2.32, 2.77] | 1.93 [1.77, 2.12] | 1.12 [0.91, 1.22] | 1.14 [0.96, 1.23] | 1.20 [1.13, 1.34] | 0.96 [0.94, 1.01] |
| upsert-100 | 2.21 [2.08, 2.58] | 2.2 [2.1, 2.31] | 41.2 [37.3, 42.3] | 1.83 [1.74, 1.94] | 1.18 [1.14, 1.27] | 1.20 [1.07, 1.28] | 22.01 [19.92, 23.34] | 0.95 [0.91, 1.03] |
| delete-100 | 1.45 [1.26, 1.68] | 1.37 [1.15, 1.59] | 1.32 [1.14, 1.57] | 1.11 [1.02, 1.17] | 1.38 [1.09, 1.66] | 1.24 [1.11, 1.38] | 1.27 [1.08, 1.63] | 0.91 [0.75, 1.14] |
| tx-5 | 3.38 [3.13, 3.8] | 3.29 [3.11, 3.93] | 4.83 [4.38, 5.04] | 1.86 [1.73, 2.05] | 1.87 [1.55, 2.08] | 1.87 [1.55, 2.23] | 2.39 [2.17, 2.96] | 0.97 [0.90, 1.20] |
| concurrent | 42.2 [40.2, 43.7] | 42.4 [39.8, 43.6] | 63.8 [61.6, 66] | 31.5 [30.4, 32.9] | 1.31 [1.21, 1.37] | 1.31 [1.24, 1.41] | 2.06 [1.96, 2.14] | 1.02 [0.98, 1.08] |

#### concurrent (50 tasks, 4 operations each, pool of 10)

| contender | ops/s | p99 latency (ms) |
|---|---|---|
| orm | 4745 | 19.3 |
| orm-all | 4721 | 20.4 |
| prisma | 3133 | 31.1 |
| pg | 6352 | 14.5 |

#### CPU split (approximate)

Measured with no database, mean of 20 repetitions, median over rounds. Approximate: `.sql()` and Django's `str(query)` inline the values, so the split is not the work of a real call. Decode time is not measured.

**orm**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.00558 | 0.0302 |
| read-50 | 0.00418 | 0.0331 |
| read-1000 | 0.00449 | 0.025 |
| projection-1000 | 0.0109 | 0.0377 |
| filter | 0.00984 | 0.041 |
| filter-ne-null | 0.0116 | 0.043 |
| aggregate | 0.00918 | 0.0507 |
| join-to-one | 0.0114 | 0.0417 |
| prefetch-to-many | 0.0161 | 0.0517 |
| m2m | 0.0172 | 0.0575 |

**orm-all**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.00575 | 0.0306 |
| read-50 | 0.00431 | 0.0349 |
| read-1000 | 0.00468 | 0.0225 |
| projection-1000 | 0.011 | 0.0378 |
| filter | 0.00999 | 0.0424 |
| filter-ne-null | 0.0115 | 0.0447 |
| aggregate | 0.00993 | 0.0495 |
| join-to-one | 0.0126 | 0.0447 |
| prefetch-to-many | 0.0163 | 0.0562 |
| m2m | 0.0155 | 0.0544 |

### TypeScript on Bun

Three cases only. Reference: `prisma` (there is no `pg` row on Bun).

#### Time per operation

| case | orm (ms) | prisma (ms) | orm / prisma |
|---|---|---|---|
| read-pk | 0.385 [0.36, 0.421] | 0.709 [0.698, 0.733] | 0.59 [0.51, 0.63] |
| read-1000 | 3.28 [3.06, 3.63] | 5.53 [5.18, 6.08] | 0.57 [0.54, 0.63] |
| bulk-insert-1000 | 26.1 [24.1, 26.6] | 93 [91.2, 96.1] | 0.27 [0.26, 0.29] |

### Notes (from `summary.md`)

- Prisma `upsert-100` runs 100 upserts in one `$transaction`: Prisma has no bulk upsert. orm and Django send one statement.
- Django async `concurrent` runs each `aget` through thread-sensitive `sync_to_async`: the calls run one after another on one thread.
- psycopg prepares a statement after 5 executions of it. Django inlines LIMIT/OFFSET, so the read-N statements differ per page and are not prepared.
- Prisma batches `findUnique` calls of one tick into one query (recalled from the Prisma docs, not checked here), so its first reads in `concurrent` can become about one round trip.
- django-sync `concurrent`: an untimed setup closes the main-thread connection first, so all 10 pooled connections serve the 50 threads (each task returns its connection with `close()`).
- Prisma `update-1` uses `updateMany` (count only), not `update` (returns the row): the same work as the other contenders.
- `pg` `join-to-one` uses an inner JOIN. `posts.author_id` is NOT NULL, so the rows equal the ORMs' joins.
- The JS `camelRow`/`data()` row conversion runs inside the timed operation: a user does the same work.
- Django `delete-100` (`QuerySet.delete()`) runs BEGIN, DELETE, COMMIT: 3 statements.
- orm returns full rows from `bulk-insert-*`: `insertMany(...).returning()` has no key-only form. Prisma and Django return ids only or full instances as noted per contender below.
- Warm-up: every case runs 50 untimed calls before the timed calls, the same count for every contender. Timed calls use iterations 1..N, the warm-up uses distinct iteration numbers.
- Contenders run one per process and round, in a rotating order, on a database reset before every process.

Per-contender notes (prepared statements, pools, insert and upsert shapes): [`summary.md`](compare/results/2026-10-21-c9bbf7c/summary.md#notes).

## Build-time extension gate — 2026-10-08

`bench/build-time-extensions`, `--native-profile`: handwritten built-in controls against the generated extension build, 3 processes × 24 batches per runtime and backend, at `10663b5`.
Gate rule: a warm case passes when its paired 95% upper bound is below 1%.
Result (measured, loaded machine): **not passed**. 0 warm regressions, 20 of 44 cases accepted, 24 inconclusive.
Most inconclusive cases have a change near 0 (−2.1% to +1.4%) but intervals 1.4 to 4.7 points wide.
Full table and load record: [`build-time-extensions/results/gate-2026-10-08/summary.md`](build-time-extensions/results/gate-2026-10-08/summary.md).
The jury-4 run before it (load 11 to 23) had 0, 4 and 40.
Artifact size at `360685a` (measured): the disabled Python library grew 5.5% and Node 5.4%, from contract and IR decoders ([`jury-4/size.md`](build-time-extensions/results/jury-4/size.md)).

## Advisory locks in Rust — 2026-10-04

`bench/shared-behavior`: `Database.lock` with key hashing and lock SQL in Rust, against the earlier Python and TypeScript code, through the public API, paired AB/BA batches, 3 processes.
Result (measured): Python is 0.5% to 4.1% faster in every case. Node integer keys are 0.8% to 1.6% faster, string keys 32% (short) to 90% (4 KiB) faster, complete transactions 0.3% to 15% faster. No case has a detected regression.
Table: [`shared-behavior/results/summary.md`](shared-behavior/results/summary.md).

## Query construction in Rust — 2026-10-04

The removed `query-construction/` experiment decoded operations directly in Rust instead of through JSON IR.
Timings changed by less than about 2% either way (measured), and the decoder changed an existing error, so it failed compatibility and was not kept.
`git show 889f1c4:bench/query-construction/results/summary.md` has the table.

## Phase 0: performance feasibility (2026-10-02)

**Historical: removed SeaORM prototype, not the current engine. Do not cite.**

**Question:** Is one Rust ORM core (SeaORM on Postgres), called natively from Python,
Node, Bun and Go, fast enough compared with each language's own ORM?

**Answer: yes for Python and JS, about even for Go.**

| language | binding | vs its native ORM | verdict |
|---|---|---|---|
| **Python** | PyO3 | **2–4.5× faster** than Django / SQLAlchemy async on reads and bulk writes | Big win: Python object building is slow and Rust replaces most of it. |
| **Node** | napi-rs | **up to 2× faster** on small queries, **3.3× faster** on bulk writes, **even** on 1000-row reads | Win, except large reads (napi builds JS objects slowly). |
| **Bun** | same napi addon | **up to 4.8× faster** on bulk writes; 1000-row reads **0.8–1.0×** | Same picture as Node. Bun's own `pg` path is fast at reads. |
| **Go** | cgo (C ABI) | **0.4–1.2×** vs GORM; raw pgx is faster than both | No speed win. Go is already compiled; Rust only adds a copy. |

The engine is worth it for speed in Python and JS. In Go it gives consistency (the same
models, queries and behaviour as the other languages), not speed.

## Summary

| question | answer |
|---|---|
| Sync or async: which is faster? | **Sync, by ~0.1–0.2 ms per call** (no event-loop hand-off). **Async handles more traffic** under concurrent load. Ship both, with async as the primary API. |
| Unix socket or TCP? | **About the same for everyone** (TCP ≤ ~0.3 ms slower per call), once sqlx's missing `TCP_NODELAY` is patched. Before the patch, large SeaORM writes over TCP were ~4× slower. |
| How much does the language → Rust call cost? | Go cgo call: **~70 ns**. Node/Bun async (Promise ↔ Tokio): **~40 µs**. Python async (asyncio ↔ Tokio): **~115 µs**. Sync calls in every language: **~0**. |

## Cross-language: median ms per call, Unix socket

Read 1 post:

| | native ORM | Rust core, async | Rust core, sync | pure Rust floor |
|---|---:|---:|---:|---:|
| Python | 0.79 (Django async) · 0.43 (Django sync) | 0.32 | 0.20 | 0.14 |
| Node | 0.36 (Drizzle) | 0.20 | 0.21 | 0.14 |
| Bun | 0.24 (Drizzle) | 0.23 | 0.21 | 0.14 |
| Go | 0.13 (GORM) · 0.11 (pgx) | — | 0.22 (cgo) | 0.14 |

Read 1000 posts:

| | native ORM | Rust core, async | Rust core, sync | pure Rust floor |
|---|---:|---:|---:|---:|
| Python | 10.25 (Django async) · 9.72 (Django sync) | 2.50 | 2.30 | 1.43 |
| Node | 3.32 (Drizzle) | 3.37 | 3.48 | 1.43 |
| Bun | 2.76 (Drizzle) | 3.63 | 3.40 | 1.43 |
| Go | 2.52 (GORM) · 0.90 (pgx) | — | 2.59 (cgo) | 1.43 |

Bulk insert 1000 posts:

| | native ORM | Rust core, async | Rust core, sync | pure Rust floor |
|---|---:|---:|---:|---:|
| Python | 41.94 (Django async) · 43.14 (Django sync) | 12.77 | 13.55 | 12.22 |
| Node | 50.01 (Drizzle) | 15.35 | 15.27 | 12.22 |
| Bun | 68.54 (Drizzle) | 15.84 | 14.38 | 12.22 |
| Go | 16.68 (GORM) · 12.48 (pgx) | — | 14.75 (cgo) | 12.22 |

"Pure Rust floor" is SeaORM timed inside Rust with no foreign objects (measured from
the Python harness, `rust-only`).

## What each language teaches

**Python.** Building objects is where Django and SQLAlchemy spend their time: ~8–9 µs
per row over the Rust floor, against ~0.6–0.8 µs for the PyO3 binding. That gap is the
whole case for the engine.

**Node / Bun.** Writes are the big win: the Rust side builds and sends a 1000-row INSERT
3–5× faster than Drizzle on `pg`. Large reads are even, because napi creates each JS
object property by property (~2 µs per row), which costs about what V8 saves by
parsing rows itself. Possible fixes: return rows as arrays or a columnar buffer
instead of objects, or build objects lazily.

**Go.** pgx is a very fast native driver: it reads 1000 rows in 0.9 ms, faster than
SeaORM itself (1.43 ms). The cgo call is cheap (~70 ns), but copying each row out of
Rust memory into Go strings and times costs about as much as GORM's reflection, so we
land level with GORM. Each cgo call also ties up an OS thread while it waits on Postgres,
which matters under heavy concurrency. Use the Rust core from Go for consistency across
languages, not for speed.

**Single-row inserts** are dominated everywhere by the per-commit WAL fsync
(0.3–1.5 ms per row). The ORM matters least there.

## Transport: Unix socket vs TCP

Every contender in every language was measured over both the Unix socket and localhost
TCP. All drivers in a run use the same transport. Results were consistent between the
two: TCP is up to ~0.1–0.3 ms slower per call, and the rankings didn't change.

### sqlx doesn't set `TCP_NODELAY` (fixed by a vendored patch)

Before the patch, SeaORM's 1000-row bulk insert over TCP took **~55 ms**. Postgres's
own logs showed it spending only ~8 ms on the statement, and the same insert over the
Unix socket took ~12 ms. The cause: sqlx 0.9 never calls `set_nodelay(true)`, so large
multi-packet requests stall on Nagle's algorithm combined with delayed ACKs, adding
about 40 ms. libpq, asyncpg, `pg` and pgx all set `TCP_NODELAY`.

**Historical fix (path no longer exists):** `bench/ormcore/patches/sqlx-core` held sqlx-core 0.9.0 plus a one-line
`stream.set_nodelay(true)` change, wired in through `[patch.crates-io]`. It applies to all
four bindings. We should send this fix upstream and drop the patch once sqlx ships it.

| Rust core bulk insert, N=1000, TCP | before patch | after patch |
|---|---:|---:|
| Python `ormcore-obj` | 54.5 ms | 13.9 ms |
| Python `rust-only` | 55.9 ms | 12.6 ms |

## Async vs sync

An async call into Rust pays to hand the result from Tokio back to the language's event
loop. A sync call blocks the caller on the runtime and pays nothing extra:

| language → Rust | median per call |
|---|---:|
| Python `future_into_py` (asyncio ↔ Tokio) | ~113 µs |
| Python: Django `sync_to_async`, for comparison | ~132 µs |
| Node napi async fn (Promise ↔ Tokio) | ~41 µs |
| Bun napi async fn | ~37 µs |
| Go cgo call (always sync) | ~70 ns |
| Any sync call (`block_on`), excluding I/O | ~0.2 µs |

In practice:
- **Python:** sync is ~0.1–0.2 ms faster on small queries (read 1 post: 0.20 vs 0.32 ms).
- **Node/Bun:** the gap is smaller (~40 µs), and sync and async are often within noise.
- **Large results:** async and sync are about the same, because materialization and the
  database dominate.

This is single-query latency. Async exists so a server can keep other requests moving
while one waits on Postgres, and this sequential benchmark doesn't measure that.
Recommendation: **async as the primary API, plus a sync API** (scripts, sync Django views,
Celery, CLIs). Go uses its normal blocking style.

## Notes and caveats

- Hardware: shared cloud VM, 4 vCPU Xeon @ 2.1 GHz, Postgres 16.14 on the same host,
  default durability settings. Absolute numbers are noisy (see p95 in the JSON); the
  rankings were stable across runs and transports.
- Versions: Python 3.11, Django 5.2.17 (psycopg 3.3), SQLAlchemy 2.1.2 (asyncpg, psycopg);
  Node 22.22, Bun 1.3.14, Drizzle 0.45 (`pg` 8.23); Go 1.25, GORM 1.31, pgx 5.11;
  SeaORM 2.0.4 (sqlx 0.9 + `TCP_NODELAY` patch), PyO3 0.29, napi-rs 3.
- Defaults were kept for each ORM, so every contender runs as idiomatic code:
  - SQLAlchemy opens a transaction per session.
  - GORM wraps each `Create` in BEGIN/COMMIT.
  - Drizzle on `pg` doesn't prepare statements.
- The Rust core uses compile-time SeaORM entities that mirror the Django tables. This
  matches the decision that schema → IR → engine translation happens at compile time.
- Not measured yet: concurrent load (many in-flight queries), free-threaded Python,
  uvloop, alternative result encodings for JS and Go.

## Full results

The per-language tables, the raw `results-<lang>-<transport>.json` files and the harness
(`run_bench.py`, `report.py`, `run_all.sh`, `ormcore/`, `django_bench/`, `js/`, `go/`) were
removed after `889f1c4`, because they measure the SeaORM prototype, not the current engine.
`git show 889f1c4:bench/RESULTS.md` has the tables.

## Engine: SeaORM vs tokio-postgres

Phase 1 replaced SeaORM (sqlx underneath) with our own driver layer on tokio-postgres.
`bench/engine_bench.py` times the public Python API on the blog models (1000 posts, 10
users), release builds, localhost TCP, `sslmode=disable`, best of two interleaved runs,
median per run. Microseconds per operation.

| case | SeaORM / sqlx | tokio-postgres | change |
|---|---:|---:|---:|
| get by pk | 593 | 399 | −33% |
| read 50 | 805 | 645 | −20% |
| read 1000 | 2671 | 2545 | −5% |
| read 1000 + select_related | 5825 | 4813 | −17% |
| 10 users + prefetch 1000 posts | 4321 | 3718 | −14% |
| count with EXISTS filter | 744 | 551 | −26% |
| insert 1 | 915 | 832 | −9% |
| insert_many 50 | 1694 | 1724 | +2% |
| update 100 rows | 1632 | 1248 | −24% |
| transaction, 2 updates | 2348 | 2026 | −14% |
| 10 concurrent gets | 2594 | 2243 | −14% |

The machine (4-core VM) has a high floor: a bare `SELECT 1` through the whole stack
takes ~210 µs. With the default `sslmode=prefer` both engines use TLS, which adds ~80 µs
per query here and hides the difference: in a single TLS run the cases ranged from 9%
slower to 8% faster, within this machine's noise.

## Instances built in Rust

`afe76ab` moved building result objects (instances, `select_related` objects,
prefetched lists, `Row`s) from Python into Rust and added subqueries, window functions,
CTEs and nested / filtered prefetch; `e5f12ce` added shared windows and CTE joins.
`bench/engine_bench.py` on each commit (release builds side by side, localhost TCP,
`sslmode=disable`), three interleaved rounds, best of the three medians. Microseconds per
operation.

| case | `4ca5514` (before) | `afe76ab` | `e5f12ce` | change |
|---|---:|---:|---:|---:|
| get by pk | 433 | 450 | 378 | noise |
| read 50 | 624 | 559 | 560 | −10% |
| read 1000 | 2273 | 1882 | 1949 | −14% to −17% |
| read 1000 + select_related | 4266 | 3138 | 3358 | −21% to −26% |
| 10 users + prefetch 1000 posts | 3545 | 3160 | 3218 | −9% to −11% |
| count with EXISTS filter | 657 | 628 | 687 | noise |
| insert 1 | 796 | 758 | 798 | noise |
| insert_many 50 | 1448 | 1439 | 1524 | noise |
| update 100 rows | 1377 | 1394 | 1363 | noise |
| transaction, 2 updates | 2428 | 2330 | 2304 | noise |
| 10 concurrent gets | 2290 | 2212 | 2231 | noise |

The gain is in reads that build many objects; operations that return a count or one row
don't change. Between `afe76ab` and `e5f12ce` the table suggests reads got a few percent
slower, but a focused A/B of those two builds (four alternating rounds) put them level:
read 1000 at 1806–1894 vs 1794–1909 µs, `select_related` at a median ~3020 vs ~3080 µs,
inside the spread of either build; building the IR and planning costs ~15 µs in both.
`count with EXISTS filter` swings between ~450 and ~750 µs on this machine within one
build.


## Where a query's time goes

Historical (2026-10-03): the probe patch no longer applies and was removed with
`profile_query.py` after `889f1c4`. `bench/profile_query.py` (with the probes in
`bench/profile_probes.patch`, which were not part of the shipped module) times each stage of one query on the blog models: release
build, localhost TCP, `sslmode=disable`, 4-vCPU VM, Postgres on the same host. Medians,
µs. Runs vary by ±10–15% on this machine; the shares are stable.

### `get by pk` today, ~300–340 µs (asyncio)

| stage | measured alone | inside the real loop |
|---|---:|---:|
| Python: query set, IR dicts, `json.dumps` | 12 | 32–38 |
| Rust: parse IR, plan, render SQL (+ creating the future) | 4 | 25 |
| asyncio ↔ Tokio hand-off (`future_into_py`) | 120 | ~110 |
| Postgres round trip (prepared statement, cached per connection) | 82–94 | 82–94 |
| decode row, build the instance, coroutine layers | — | ~40 |

Stages cost 3–5× more inside the loop than alone: between two queries the CPU runs the
Tokio worker and the Postgres backend, so the Python and planner code runs on cold
caches. Estimates below use the in-loop numbers.

### The hand-off is the largest cost we control

| | asyncio | uvloop |
|---|---:|---:|
| no-op async call, `future_into_py` (today) | 120 | 64 |
| no-op async call, eventfd completion (prototype) | 50 | 44 |
| `SELECT 1`, Rust loop with no Python | 82 | 87 |
| `SELECT 1`, blocking call from Python | 79 | 81 |
| `SELECT 1`, async, `future_into_py` (today) | 181 | 122 |
| `SELECT 1`, async, eventfd completion (prototype) | 103 | 101 |

`future_into_py` finishes the asyncio future from the Tokio thread. That thread has to
take the GIL from the event loop thread and then wake it through the loop's self-pipe,
which costs several thread wake-ups. In the prototype the Tokio task writes to an
eventfd that the loop watches (`loop.add_reader`), and the loop picks up the result on
its own thread. With that change an async call costs the same as a blocking one.
uvloop by itself also removes about half of the cost.

### Plan cache: measured A/B

`_probe_run_cached` stores the SQL, arguments and output shape under the IR JSON key,
which is the upper bound of a Rust-side plan cache. A "prepared query" builds the IR
once and reuses it. Interleaved runs:

| | get by pk, asyncio | read 50 + 2 filters, asyncio | get by pk, uvloop | read 50 + 2 filters, uvloop |
|---|---:|---:|---:|---:|
| today, end to end | 343 | 518 | 201 | 400 |
| prepared query (IR built once) | 229 | 383 | 154 | 274 |
| prepared query + Rust plan cache | 206 | 324 | 137 | 227 |

The Rust plan cache alone saves 17–58 µs per query (7–15%): parse, plan and render
take 4 µs in a microbenchmark, but more on cold caches. Skipping the Python IR build
saves more than that (65–135 µs). Postgres already caches its own plans, because
statements are prepared once per connection.

### Schema JSON at runtime: a startup cost only

| schema | `json.loads` | `define()` (Python classes) | `json.dumps` | Rust parse + index |
|---|---:|---:|---:|---:|
| blog, 6 models, 9 KB | 0.06–0.18 ms | 0.65–0.85 ms | 0.12–0.17 ms | 0.12–0.14 ms |
| 204 models, 277 KB | 1.2–1.5 ms | 9.9–10.5 ms | 2.4 ms | 1.2–1.5 ms |

This cost is paid once per process. Queries never touch the JSON: `Schema::from_ir`
indexes models, fields and relations into hash maps, and the planner reads those.
Creating the Python classes costs more than parsing the JSON. Compiling the schema into
Rust would save about 5 ms of startup on a 200-model schema and nothing per query.

### Prepared queries

`qs.prepare()` (built once, values bound per call) against the same query built per
call. The two variants alternate (21 rounds of 200 calls each), release build, same
machine. Median, µs.

| case | asyncio | prepared | uvloop | prepared |
|---|---:|---:|---:|---:|
| get by pk | 305 | 239 (−22%) | 214 | 166 (−22%) |
| read 50 + 2 filters | 498 | 344 (−31%) | 306 | 266 (−13%) |

`bench/engine_bench.py` now has prepared cases. Sequential runs of it on this machine
swing by ±20% (an unchanged "read 50" went from 313 to 509 µs between runs), so the
table above uses alternating runs.


## Current Rust engine versus SeaORM — 2026-10-03

Direct Rust comparison of the current `orm-engine` against unmodified SeaORM
2.0.4 / SQLx 0.9.0. This measures the current planner and database driver,
separately from the older SeaORM-based `ormcore` prototype above.

Environment: macOS ARM64, Rust 1.97.0, release build with LTO, isolated PostgreSQL
18.6 (Homebrew), localhost TCP with TLS disabled. Each ORM has one pooled
connection and runs on the same current-thread Tokio runtime. PostgreSQL uses
default durability settings.

Median batch-average latency in **µs per operation**; lower is better.
“Speed ratio” is SeaORM latency divided by our direct-IR latency.

| Case | SeaORM | Our direct IR + planner | Our JSON parse + planner | Speed ratio |
|---|---:|---:|---:|---:|
| get by pk | 70.87 | 37.26 | 37.89 | 1.90× |
| read 50 | 95.46 | 55.63 | 56.39 | 1.72× |
| read 1000 | 383.33 | 306.26 | 306.49 | 1.25× |
| filtered count | 105.76 | 62.26 | 63.98 | 1.70× |
| update 100 | 138.94 | 106.32 | 107.40 | 1.31× |

Our engine had lower median latency in all five measured cases: **1.25–1.90×**
the operation rate implied by SeaORM's latency in this sequential workload.
Skipping JSON parsing reduced median latency by only **0.23–1.73 µs** in these
cases. These results do not demonstrate a benefit from generated Rust entities
or precompiled SQL; neither is implemented in this comparison.

Method: 1,000 seeded rows with two integer fields, a title, and a 200-byte body.
Both implementations bind query values and decode every selected field into
exactly the same owned Rust model. The harness asserts complete result equality
and expected counts before timing. Each contender receives 30 warm-up calls,
followed by 15 batches of 20–150 operations; contender order rotates each round.
Connections, schema compilation, and seeding are outside timing. Query building,
planning, execution, result allocation, and result destruction are inside timing.
The direct-IR path clones a typed operation template for each call; the JSON path
parses a prepared JSON string per call, without timing JSON serialization.

The update case sets `views = 0` on 100 rows and returns the affected-row count.
Its batch averages varied considerably: SeaORM **135.49–191.44 µs**, direct IR
**101.14–157.68 µs**, and JSON IR **103.96–159.57 µs**. The primary-key case also
had a SeaORM outlier of **145.24 µs** versus its **70.87 µs** median. Reported
values are medians of batch averages, not per-request tail latencies.

Scope: warm-cache sequential operations against a local database. The comparison
includes different drivers (`tokio-postgres` versus SQLx), so it cannot attribute
the difference to the planner alone. It does not cover joins, prefetching,
inserts, concurrent load, remote databases, or production workloads.

Harness and methodology: [rust-compare/README.md](rust-compare/README.md).
Raw batch measurements: [rust-compare/results.json](rust-compare/results.json).


### Completed verification — three independent runs

Completed two additional release runs against the isolated PostgreSQL instance,
with a fresh table and seed data for each run. The initial measurements above
and their raw JSON remain unchanged. All three runs completed all five cases
and all correctness assertions: **63,450 timed operations** in total, plus
untimed warm-up and validation calls.

The verification runs additionally assert the exact seeded ID, title, body,
and view count of every returned row. For each of the three update paths, the
harness resets `views = id`, runs the update, and independently checks that
exactly IDs 1–100 have `views = 0` while IDs 101–1000 retain their original views.
These checks and resets are outside timing.

The following latency values are the **median of the three run medians**, in
µs per operation. The final column gives the range of SeaORM/direct-IR latency
ratios across those runs, rather than a confidence interval.

| Case | SeaORM | Our direct IR + planner | Our JSON parse + planner | Speed ratio across runs |
|---|---:|---:|---:|---:|
| get by pk | 71.45 | 37.26 | 37.89 | 1.90–1.92× |
| read 50 | 95.36 | 55.63 | 56.13 | 1.70–1.72× |
| read 1000 | 387.77 | 305.26 | 305.16 | 1.25–1.30× |
| filtered count | 102.63 | 62.64 | 63.29 | 1.63–1.70× |
| update 100 | 140.14 | 107.13 | 107.40 | 1.22–1.31× |

Our direct Rust path had lower median latency in **every case in every run**.
The observed speed ratios span **1.22–1.92×**. The 100-row update remained the
most variable case: run medians were 138.94, 179.91, and 140.14 µs for SeaORM,
and 106.32, 147.15, and 107.13 µs for direct IR. Both implementations slowed
in the same repeat run; the measurements do not isolate the cause. Read timing
was steadier. The small direct-IR versus JSON differences include measurement
noise; JSON was marginally faster in some cases in the last run.

Validation completed: release compilation, Rust formatting check, Clippy on
the benchmark crate with warnings treated as errors, report whitespace check,
and all runtime result/state assertions in both verification runs. There were
no benchmark failures. The same workload and driver limitations stated above
still apply; these checks do not establish a universal performance ranking.

Additional raw measurements:
[repeat 1](rust-compare/results-repeat-1.json),
[repeat 2](rust-compare/results-repeat-2.json).
The harness accepts `ORM_BENCH_OUT` to preserve each run in a separate file.


### Diesel added — 2026-10-03

Expanded the Rust comparison with **Diesel 2.3.13 / diesel-async 0.9.2**, using
its typed query DSL and `AsyncPgConnection`. Diesel uses a deadpool pool limited
to one connection, with fast recycling. Both Diesel's async adapter and our
engine use `tokio-postgres`; SeaORM uses SQLx. All contenders run in the same
process, on the same current-thread Tokio runtime, against the same isolated
PostgreSQL 18.6 server and seeded data. This tests Diesel's async adapter,
not its synchronous libpq connection implementation.

Ran the complete expanded comparison **three times**, recreating and seeding
the table for each run. Four contenders now rotate through **16 batches** per
case, giving each contender each execution-order position four times.
The three runs contain **90,240 timed operations**, plus warm-up and validation.
All contenders decode reads into the identical owned Rust model; Diesel derives
`Queryable` on the same struct used by SeaORM and our engine. All read-result,
expected-field, affected-row-count, and independent update-state checks passed.

Latencies below are the **median of the three run medians**, in µs per operation.
These are fresh measurements for all contenders, not Diesel measurements merged
with the earlier three-contender timings. Lower is better.

| Case | SeaORM | Our direct IR + planner | Our JSON parse + planner | Diesel async |
|---|---:|---:|---:|---:|
| get by pk | 71.58 | 37.35 | 38.08 | 36.47 |
| read 50 | 95.77 | 55.74 | 56.01 | 55.60 |
| read 1000 | 388.76 | 302.57 | 302.05 | 262.08 |
| filtered count | 102.48 | 62.43 | 62.75 | 61.48 |
| update 100 | 185.15 | 152.52 | 152.69 | 191.00 |

**Measured outcome:** Diesel and our engine are close for primary-key lookup,
50-row reads, and the filtered count. Across runs, Diesel's latency was about
2–4% lower for lookup, within 0.4% of our engine for 50-row reads, and about
1–4% lower for count. Those small differences should not be treated as a broad
performance advantage.

For 1,000-row reads, Diesel was consistently faster: its latency was **13–15%
lower**, equivalent to about **1.15–1.18×** our operation rate in this sequential
workload. For the 100-row update, our direct-IR path was consistently faster:
its latency was **20–26% lower**, equivalent to about **1.25–1.36×** Diesel's
operation rate. Our engine continued to have lower median latency than SeaORM
in every case in every run.

Write timings remain variable. Diesel's update run medians were **191.00,
193.51, and 144.45 µs**; our direct-IR medians were **152.52, 155.28, and
106.21 µs**. These results therefore support a workload-specific comparison,
not a universal winner. They do not isolate SQL generation, model decoding,
connection-pool behavior, or driver overhead, and still exclude joins, inserts,
concurrent load, remote databases, and production workloads.

Validation: release compilation, formatting, benchmark Clippy with warnings
treated as errors, report whitespace checks, and all runtime assertions passed.
Earlier report sections and raw measurement files were preserved.

Raw expanded-comparison results:
[run 1](rust-compare/results-diesel-1.json),
[run 2](rust-compare/results-diesel-2.json),
[run 3](rust-compare/results-diesel-3.json).
Implementation: [rust-compare/src/main.rs](rust-compare/src/main.rs).
Adapter reference: [diesel-async documentation](https://docs.rs/diesel-async/latest/diesel_async/).


### Diesel investigation and pool-setting correction — 2026-10-03

**Correction to the preceding comparisons:** SeaORM was using its default
`test_before_acquire = true`, which performs a connection-health ping for each
pool checkout. Our engine and Diesel used fast recycling. The reported timings
remain measurements of those configurations, but their speed ratios also
include this health-check difference and overstate the advantage attributable
to ORM/driver implementation alone. The historical “SeaORM / sqlx” driver-swap
measurements were full-stack measurements through SeaORM and the Python API;
they did not establish a ranking against raw SQLx. Earlier sections and raw
files have been preserved; use the controlled results below for this distinction.

Both libraries document the default ping behavior:
[SeaORM ConnectOptions](https://docs.rs/sea-orm/latest/sea_orm/struct.ConnectOptions.html),
[SQLx PoolOptions](https://docs.rs/sqlx/latest/sqlx/pool/struct.PoolOptions.html).

#### Full ORM comparison with matched health-check settings

Repeated all five cases three times with SeaORM's per-checkout ping disabled,
matching the fast-recycling policy of our engine and Diesel. Pool size, database,
query results, runtime, correctness checks, and rotating batch order remain as
in the expanded comparison. This is another **90,240 timed operations**.
Values are medians of three run medians, in µs per operation.

| Case | SeaORM, fast checkout | Our direct IR + planner | Our JSON parse + planner | Diesel async |
|---|---:|---:|---:|---:|
| get by pk | 56.53 | 37.04 | 37.74 | 35.52 |
| read 50 | 78.11 | 55.21 | 55.18 | 55.09 |
| read 1000 | 362.57 | 303.32 | 303.12 | 262.96 |
| filtered count | 90.76 | 62.23 | 62.29 | 61.59 |
| update 100 | 171.26 | 151.65 | 151.75 | 188.52 |

Our engine still had lower median latency than SeaORM in all five cases in each
matched run. The ratios are smaller than with default health checks: primary-key
lookup is about 1.5×, rather than the earlier 1.9×. Diesel remains faster for
1,000-row reads, small reads/counts remain close, and our update remains faster
than Diesel. This correction does not reverse those workload-specific outcomes.

Matched-setting raw results:
[run 1](rust-compare/results-matched-1.json),
[run 2](rust-compare/results-matched-2.json),
[run 3](rust-compare/results-matched-3.json).
Set `ORM_BENCH_SEA_FAST=1` in the original harness to select this configuration;
the output now records `seaorm_test_before_acquire` explicitly.

#### Read-path diagnosis, including raw SQLx

Added [src/bin/diagnose.rs](rust-compare/src/bin/diagnose.rs) to separate planner
cost, runtime cell decoding, raw-row buffering, and checkout pings. Nine variants
read identical seeded rows into the same owned Rust model, with one pooled
connection per variant. Raw SQLx uses `query_as` with a typed `FromRow` that reads
columns by index; it does not use the compile-time `query!` macro. The prebuilt
SQL variants bind the same limit and use cached prepared statements.

Each variant passes full expected-row equality checks. Eighteen batches rotate
execution order, so each variant occupies each position twice. Three clean runs
measure **160,380 database operations**, plus warm-up and separate CPU tests.
The first diagnostic pass overlapped compilation and is retained only as
`results-diagnose-calibration.json`; it is excluded from these results. Clean
runs started after compilation and Clippy finished, and ran sequentially.

Median of three run medians, in µs per operation:

| Read path | 1 row | 50 rows | 1,000 rows |
|---|---:|---:|---:|
| Our direct IR + planner | 40.53 | 61.48 | 305.36 |
| Our driver, prebuilt SQL | 38.72 | 59.43 | 298.88 |
| tokio-postgres, typed buffered rows | 38.32 | 59.12 | 296.52 |
| tokio-postgres, typed streamed rows (prototype) | 38.25 | 58.78 | 247.84 |
| Diesel async | 39.42 | 61.03 | 253.14 |
| Raw SQLx, default health checks | 74.86 | 99.69 | 335.67 |
| Raw SQLx, fast checkout | 57.65 | 83.17 | 314.18 |
| SeaORM, default health checks | 77.11 | 101.58 | 385.02 |
| SeaORM, fast checkout | 60.06 | 83.92 | 369.80 |

For these ordered 1,000-row reads, Diesel has lower latency than both our
current engine and the raw SQLx `query_as` path, even after matching health-check
settings. Raw SQLx is substantially closer to our engine than SeaORM is. This is
not a general ranking across SQLx APIs or production workloads.

The default health checks add about 16–21 µs in this read-only diagnostic when
comparing run-level aggregate medians. This accounts for part of the earlier
gap; other driver and ORM costs remain. Ordered one-row reads here are a
separate query from the primary-key lookup in the full comparison, so their
absolute timings should not be merged.

#### What we can learn from Diesel

Source inspection of diesel-async 0.9.2 shows that its PostgreSQL
`load_prepared` calls `query_raw`, wraps each arriving row, and maps the stream
through `U::build_from_row` before collecting the final models. Our PostgreSQL
`query` first calls `Client::query` to collect a `Vec<Row>` and returns a boxed
`RowSet`; the caller then builds a second vector of models through runtime
`Cell` conversion. Diesel's `Queryable` supplies the result shape and field
conversions through Rust types.

Relevant source:
[Diesel async row-to-model mapping](https://docs.rs/diesel-async/latest/src/diesel_async/run_query_dsl/mod.rs.html),
[Diesel async PostgreSQL execution](https://docs.rs/diesel-async/latest/src/diesel_async/pg/mod.rs.html),
[our PostgreSQL driver](../crates/engine/src/db/postgres.rs).

The controlled buffered-versus-streamed tokio-postgres pair uses the same pool,
SQL, prepared-statement cache, row decoder, capacity for the final model vector,
and owned result type. Only collection of the intermediate raw rows changes.
At 1,000 rows, its median drops from **296.52 to 247.84 µs**: **48.68 µs / 16.4%**
lower latency. The streamed prototype is close to Diesel's **253.14 µs**. This
supports streaming directly into final results as the strongest measured
opportunity. The experiment does not separate reduced buffering/allocation from
changes in when decoding work happens while receiving rows.

The separate CPU-only test repeatedly decodes already fetched 1,000-row sets,
including model allocation and destruction, without database I/O. Runtime
`RowSet::cell` decoding takes **56.11 µs** versus **48.18 µs** for typed
`tokio_postgres::Row::try_get`: a smaller **7.93 µs** difference. This identifies
an opportunity for a reusable decoder chosen from the output schema, while
retaining support for dynamic models and extension types.

Skipping our planner entirely changes the diagnostic median from **305.36 to
298.88 µs**, only **6.48 µs**. Precompiling every query is therefore a lower
priority for this workload. Diesel also has a typed query identity for cached
prepared statements; our engine already caches prepared statements by SQL and
parameter types. Inspection of this diesel-async version shows that it still
renders SQL before the cache lookup, so its result should not be explained as
all SQL having been precompiled at build time.

**Recommended implementation order:** prototype an internal query/row-consumer
API that materializes final results from the incoming stream; keep the buffered
path available where prefetching or consumers require a row set. Then measure
a reusable schema-driven decoder. A generated typed Rust decoder can be an
additional native-Rust optimization. These prototypes validate a direction;
the current production engine has not been replaced by the diagnostic path.

Release builds, formatting, Clippy with warnings treated as errors, result/state
assertions, and report whitespace checks passed. These diagnostics cover simple
sequential local reads, not joins, transactions, inserts, concurrency, remote
latency, or every SQLx execution API.

Diagnostic raw results:
[run 1](rust-compare/results-diagnose-1.json),
[run 2](rust-compare/results-diagnose-2.json),
[run 3](rust-compare/results-diagnose-3.json).
