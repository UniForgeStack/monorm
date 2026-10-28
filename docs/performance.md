# Performance

## Python and TypeScript versus Django and Prisma

[`bench/compare`](../bench/compare/README.md): the public Python and TypeScript APIs of `monorm` against Django 6.1.2 (sync and async), Prisma 7.10.1 and raw `pg` 8.23.1 (the floor), on one shared schema and seed. The run (`bench/compare/run.sh`, no flags) used release builds at `c9bbf7c`: 20 rounds × 20 timed iterations, 50 untimed warm-up calls per case, one fresh process per contender and round, rotated order.
Results directory: [`bench/compare/results/2026-10-21-c9bbf7c/`](../bench/compare/results/2026-10-21-c9bbf7c/summary.md) (`summary.md`, raw JSON per contender, `environment.json`, logs).

Machine (from `environment.json`, `lscpu` and `/proc/meminfo`): cloud VM `vm`, Intel Xeon Processor @ 2.80GHz, 4 cores (1 thread per core), 16 GB RAM, Ubuntu 24.04.5 LTS (Linux 6.18.44, x86_64), PostgreSQL 16.15 over TCP on localhost. CPython 3.14.6, Node v22.22.0, Bun 1.4.2, rustc 1.97.0.

All numbers below are **measured** and copied from `summary.md` unchanged. Each cell is `median [95% cluster bootstrap interval]` of the per-round medians; each ratio is the median of the paired per-round ratios with its interval. `run.json` marks the run valid: the 1-minute load was 1.13 at the start and 1.25 at the end, and 1.06 to 1.98 before each round (limit 2). The machine is a shared cloud VM, so the load sat close to the limit for much of the run; treat small differences (intervals that overlap 1.00) as no difference.

Summary of the ratios to each side's reference, the `pg` floor on Node (measured; the lowest and highest case of the 20, see the tables for each case):

- Python, reference `django-async` (no `pg`-like floor on Python): `monorm / django-async` is 0.23 (`m2m`) to 0.92 (`tx-5`); `django-sync / django-async` is 0.67 to 1.04.
- TypeScript on Node, floor `pg`: `monorm / pg` is 1.00 (`read-1000`) to 2.24 (`projection-1000`); `prisma / pg` is 1.20 (`bulk-update-100`) to 22.01 (`upsert-100`).
- Bun, reference `prisma`: `monorm / prisma` is 0.27 to 0.59 on the three cases.
- `monorm-all / monorm` (every extension built in) is 0.98 to 1.15 on Python and 0.91 to 1.07 on Node.

CPU split tables and per-contender notes: [`bench/RESULTS.md`](../bench/RESULTS.md#python-and-typescript-versus-django-and-prisma--2026-10-21).

### Read the numbers with care

From [`bench/compare/README.md`](../bench/compare/README.md#read-the-numbers-with-care):

- The `monorm` wheel must be a release build with the postgres profile. A debug tooling build (the dev venv) runs, but its timings are not valid.
- Django has no async `atomic()`. The `tx-5` case on `django-async` runs a sync function through `sync_to_async`. Django async calls also serialize on one thread.
- The sync ORM cannot run in an event loop. The `django-sync` process runs without one; its `concurrent` case uses 50 threads.
- The CPU split is the time to build the query and to render its SQL (`.sql()` of `monorm` inlines values, `str(queryset.query)` of Django does too). The decode time is not reachable in either library and is not measured. Treat the split as approximate.
- `filter-ne-null` is an extra case: `exclude(category_id=3)` in Django keeps the NULL rows, and `(Post.category_id != 3).or_null()` is the same in `monorm`.
- Prepared statements: `monorm` prepares once per connection and caches. psycopg prepares after 5 runs of a statement. `adapter-pg` of Prisma sends unnamed statements.
- `monorm` returns full rows from `bulk-insert-*` (`returning()` has no key-only form); Prisma selects ids only. `summary.md` prints this and the other per-contender notes.

### Python

monorm 0.1.0, django 6.1.2, psycopg 3.3.6, psycopg-binary 3.3.6, psycopg-pool 3.3.3, asgiref 3.12.1. monorm: release build, revision c9bbf7c; monorm-all: release build, revision c9bbf7c.

Reference: `django-async`. There is no `pg`-like floor contender on Python. `monorm-all` is the `monorm` build with every extension; the `monorm-all / monorm` column compares the two.

#### Time per operation

| case | monorm (ms) | monorm-all (ms) | django-sync (ms) | django-async (ms) | monorm / django-async | monorm-all / django-async | django-sync / django-async | monorm-all / monorm |
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
| monorm | 2206 | 45.1 |
| monorm-all | 2120 | 45.4 |
| django-sync | 814 | 206 |
| django-async | 831 | 91 |


### TypeScript on Node

pg 8.23.1, prisma 7.10.1, @prisma/client 7.10.1, @prisma/adapter-pg 7.10.1. monorm: release build, revision c9bbf7c; monorm-all: release build, revision c9bbf7c.

Reference: `pg` (prepared statements, no ORM work), the floor. The `monorm-all / monorm` column compares the two builds.

#### Time per operation

| case | monorm (ms) | monorm-all (ms) | prisma (ms) | pg (ms) | monorm / pg | monorm-all / pg | prisma / pg | monorm-all / monorm |
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
| monorm | 4745 | 19.3 |
| monorm-all | 4721 | 20.4 |
| prisma | 3133 | 31.1 |
| pg | 6352 | 14.5 |


### TypeScript on Bun

Three cases only. Reference: `prisma` (there is no `pg` row on Bun).

#### Time per operation

| case | monorm (ms) | prisma (ms) | monorm / prisma |
|---|---|---|---|
| read-pk | 0.385 [0.36, 0.421] | 0.709 [0.698, 0.733] | 0.59 [0.51, 0.63] |
| read-1000 | 3.28 [3.06, 3.63] | 5.53 [5.18, 6.08] | 0.57 [0.54, 0.63] |
| bulk-insert-1000 | 26.1 [24.1, 26.6] | 93 [91.2, 96.1] | 0.27 [0.26, 0.29] |

## Build-time extension cost gate

A build with extensions must cost less than 1% on warm operations, compared with the same behavior written by hand into the core.
`bench/build-time-extensions` measures it: paired batches of handwritten built-in controls against the generated extension build, for Python and Node, on PostgreSQL and SQLite.
A case passes when its paired 95% upper bound is below 1%.

The latest run (2026-10-08, `10663b5`, Apple M5, PostgreSQL in Docker) has **not passed** (measured, load 1.9 to 4.3):
0 warm regressions, 20 of 44 cases accepted, 24 inconclusive.
Most inconclusive cases change by −2.1% to +1.4%, but their intervals are too wide on a loaded machine to clear 1%.
Definition (setup) cost and artifact size are reported separately: the disabled native library is 5.5% (Python) and 5.4% (Node) larger, from contract and IR decoders (measured at `360685a`).
Details: [`gate-2026-10-08/summary.md`](../bench/build-time-extensions/results/gate-2026-10-08/summary.md), [`bench/RESULTS.md`](../bench/RESULTS.md).

## Optimization opportunities

Status: measured opportunities, not scheduled or implemented in the production
engine. Evidence recorded on 2026-10-03. Start with result materialization for
large reads; the measurements below do not establish priorities for every
workload or language binding.

### Evidence and scope

The [benchmark report](../bench/RESULTS.md#diesel-investigation-and-pool-setting-correction--2026-10-03)
contains the source investigation, corrected ORM comparisons, and raw results.
The [diagnostic harness](../bench/rust-compare/src/bin/diagnose.rs) compares
identical owned Rust models on PostgreSQL 18.6, with one pooled connection per
variant, release builds, and three independent runs. It checks every returned
field and rotates contender order. Timings are medians of run medians.

| Opportunity | Measured evidence | Priority |
|---|---|---|
| Materialize results from the incoming row stream | Typed buffered versus typed streamed reads of 1,000 rows: **296.52 → 247.84 µs**, 16.4% lower latency | First |
| Choose a reusable decoder from the query output shape | CPU-only decoding and allocation of 1,000 already fetched rows: runtime cells **56.11 µs**, typed decoding **48.18 µs** | Second |
| Skip query planning for fixed queries | Current planner path **305.36 µs**, prebuilt SQL through our driver **298.88 µs** | Defer until another workload justifies it |

These experiments isolate different costs; their savings must not be added
together as a forecast. The streaming prototype also skips our planner and
runtime cell interface. Its absolute latency is not a production-engine result.
The controlled buffered/streamed pair uses the same SQL, pool, prepared-statement
cache, typed decoder, and final result type. Small reads showed little benefit
from streaming.

### 1. Stream into final results

Today, [the PostgreSQL driver](../crates/engine/src/db/postgres.rs) calls
`Client::query`, collects a `Vec<Row>`, and returns a boxed `RowSet`. Consumers
then allocate and fill their final result collection. Diesel's async adapter
uses `query_raw`, converts arriving rows into typed models, and collects those
models directly. Our diagnostic prototype demonstrates the same approach with
tokio-postgres.

Prototype an internal query consumer or materialization API that can build final
results while consuming the incoming stream. Retain the buffered path for
existing consumers and operations that need a row set, including relation-key
collection and prefetching. Public query results can still be complete lists;
this opportunity does not require a public streaming API.

The implementation must preserve ordering, null handling, errors, transactions,
and cancellation behavior. Python and Node objects must be constructed in the
runtime context their bindings require; the Rust-struct prototype does not
measure those constraints. Measure binding materialization and thread hand-off
costs before claiming the same improvement for Python or JavaScript.

### 2. Reuse decoders for an output shape

`RowSet::cell` dispatches on `ValueType` for each field of each row. Investigate
choosing column decoders once from a validated output shape and reusing that
recipe across rows. Include selected expressions, joined models, nullable
relations, enum mappings, arrays, decimals, and extension types in the design.

For a future native Rust frontend, generated model types can enable specialized
row decoding. That is a distinct opportunity from compiling schema metadata
merely to reduce startup time. Shared Python/Node packages should continue to
support dynamic schemas without requiring a native build for each schema.

### 3. Keep planning and cache changes evidence-driven

Prepared database statements are already cached by our driver. Diesel also
caches prepared statements using typed query identities where available; the
inspected diesel-async version still renders SQL before its cache lookup.
Its read performance does not demonstrate that all SQL was compiled at build
time.

The current simple-read diagnostic attributes only about 6.5 µs to bypassing
planning. Keep the existing decision against a general IR-keyed plan cache
unless complex-query profiling establishes enough benefit to justify binding
recipes, cache memory, invalidation, and correctness work. Building typed Rust
operations directly can avoid JSON parsing independently of plan caching.

### Benchmark and acceptance requirements

- Match pool checkout policies, transport, TLS, connection count, database
  settings, result shape, and decoded fields. Earlier SeaORM comparisons used
  default per-checkout health pings while our engine and Diesel used fast
  recycling; use the corrected results when assessing implementation costs.
- Run release benchmarks sequentially after compilation finishes. Preserve raw
  measurements, rotate execution order, and report variability. The retained
  diagnostic calibration run overlapped compilation and is excluded.
- Assert complete result equality and write-state correctness. Include small
  reads as well as large reads to detect regressions.
- Before shipping an engine change, exercise joins, projections, nested
  prefetching, transactions, cancellation, conversion errors, and supported
  column types. Verify the existing buffered API and SQLite behavior.
- Measure the public Python and Node APIs and concurrent workloads. The current
  diagnostic covers sequential local PostgreSQL reads of 1, 50, and 1,000 rows;
  it does not establish production throughput or memory savings. Measure peak
  memory separately if the implementation claims a memory improvement.

Full comparison methodology and reproduction details live in
[bench/rust-compare/README.md](../bench/rust-compare/README.md).
