# Benchmark summary 2026-10-21-c9bbf7c

## Summary

Speed-up is contender time / `orm` time: `2.3×` means `orm` is 2.3 times faster. End to end: each time includes the database round trip.

- Python vs `django-sync`: geometric mean 1.57×, range 0.99× (slower) (tx-5) to 3.86× (m2m) (20 cases)
- Python vs `django-async`: geometric mean 1.97×, range 1.09× (tx-5) to 4.30× (m2m) (20 cases)
- Node vs `prisma`: geometric mean 1.79×, range 0.73× (slower) (projection-1000) to 18.67× (upsert-100) (20 cases)
- Bun vs `prisma`: geometric mean 2.21×, range 1.68× (read-pk) to 3.64× (bulk-insert-1000) (3 cases)

CPU split (build query + make SQL, contender time / `orm` time): approximate, no database, decode not measured. The main numbers above are end to end.

- Python vs `django-sync`: geometric mean 4.47×, range 1.98× (prefetch-to-many) to 6.72× (aggregate) (10 cases)
- Python vs `django-async`: geometric mean 4.93×, range 2.32× (prefetch-to-many) to 6.80× (join-to-one) (10 cases)

All numbers are **measured** on this machine: 20 rounds x 20 iterations per case (the warm-up calls are not timed), the contender order rotated every round, one fresh process per contender and round. Each cell is `median [95 percent cluster bootstrap interval]` of the per-round medians; each speed-up is the median of the paired per-round values (contender time / orm time) with its interval.

Load before: ` 12:38:15 up 39 min,  0 user,  load average: 1.13, 1.38, 1.29`  
Load after: ` 13:34:50 up  1:35,  0 user,  load average: 1.25, 1.42, 1.51`

1-minute load before each round (measured): min 1.06, max 1.98.

Build times (measured): build-python-wheel 172s, build-node-addon 185s, build-all-artifact 328s

## Python

orm 0.1.0, django 6.1.2, psycopg 3.3.6, psycopg-binary 3.3.6, psycopg-pool 3.3.3, asgiref 3.12.1. orm: release build, revision c9bbf7c; orm-all: release build, revision c9bbf7c.

There is no `pg`-like floor contender on Python. `orm-all` is the `orm` build with every extension; the `orm-all / orm` column is the extension cost.

### Time per operation

| case | orm (ms) | orm-all (ms) | django-sync (ms) | django-async (ms) | orm vs django-sync | orm vs django-async | orm-all / orm (extension cost) |
|---|---|---|---|---|---|---|---|
| read-pk | 0.542 [0.532, 0.57] | 0.599 [0.575, 0.634] | 0.603 [0.565, 0.667] | 0.856 [0.804, 0.908] | 1.07× [1.00, 1.20] | 1.56× [1.45, 1.77] | 1.10× [1.04, 1.24] |
| read-50 | 0.741 [0.697, 0.775] | 0.801 [0.755, 0.914] | 1 [0.907, 1.05] | 1.35 [1.3, 1.41] | 1.36× [1.30, 1.46] | 1.88× [1.71, 2.06] | 1.15× [1.03, 1.20] |
| read-1000 | 3.18 [3.03, 3.25] | 3.24 [3, 3.45] | 8.5 [8.11, 8.76] | 9.64 [9.5, 10.1] | 2.69× [2.58, 2.79] | 3.05× [2.94, 3.35] | 0.98× [0.93, 1.08] |
| projection-1000 | 2.73 [2.66, 2.86] | 2.92 [2.67, 3.1] | 5.92 [5.73, 6.19] | 7.08 [6.87, 7.44] | 2.14× [2.00, 2.21] | 2.58× [2.51, 2.85] | 1.06× [0.94, 1.15] |
| filter | 0.781 [0.741, 0.843] | 0.793 [0.763, 0.862] | 1.08 [1.06, 1.13] | 1.52 [1.38, 1.67] | 1.42× [1.29, 1.50] | 1.95× [1.75, 2.05] | 1.01× [0.99, 1.07] |
| filter-ne-null | 0.767 [0.744, 0.788] | 0.825 [0.794, 0.868] | 1.02 [1.01, 1.06] | 1.41 [1.35, 1.5] | 1.33× [1.31, 1.37] | 1.87× [1.70, 1.99] | 1.09× [0.98, 1.18] |
| count | 1.07 [1.01, 1.12] | 1.13 [1.05, 1.25] | 1.27 [1.22, 1.3] | 1.67 [1.61, 1.77] | 1.16× [1.05, 1.25] | 1.56× [1.48, 1.68] | 1.03× [0.95, 1.18] |
| aggregate | 3.48 [3.35, 3.73] | 3.67 [3.43, 3.91] | 4.31 [4.1, 4.45] | 5.3 [5.07, 5.56] | 1.23× [1.17, 1.32] | 1.45× [1.38, 1.57] | 1.05× [0.93, 1.18] |
| join-to-one | 1.47 [1.39, 1.51] | 1.51 [1.42, 1.58] | 2.04 [1.94, 2.2] | 2.58 [2.42, 2.86] | 1.45× [1.33, 1.53] | 1.86× [1.67, 1.92] | 1.03× [0.99, 1.14] |
| prefetch-to-many | 2.96 [2.58, 3.12] | 2.91 [2.72, 3.14] | 8.64 [8.1, 9.35] | 9.78 [9.1, 10.1] | 3.06× [2.82, 3.41] | 3.40× [3.14, 3.57] | 1.05× [0.95, 1.12] |
| m2m | 1.41 [1.3, 1.47] | 1.45 [1.39, 1.65] | 5.39 [4.82, 5.92] | 5.89 [5.59, 6.33] | 3.86× [3.52, 4.04] | 4.30× [3.91, 4.66] | 1.06× [0.97, 1.18] |
| insert-1 | 0.937 [0.876, 1.01] | 0.981 [0.901, 1.01] | 0.942 [0.884, 0.997] | 1.29 [1.21, 1.41] | 1.00× [0.92, 1.12] | 1.40× [1.30, 1.54] | 1.02× [0.96, 1.07] |
| bulk-insert-100 | 3.9 [3.66, 4.02] | 3.72 [3.5, 4.54] | 6.32 [6.05, 6.63] | 7.12 [6.48, 7.83] | 1.72× [1.51, 1.78] | 1.84× [1.67, 2.06] | 1.05× [0.89, 1.17] |
| bulk-insert-1000 | 25.1 [24.2, 25.7] | 25.8 [25.2, 26.7] | 44.5 [44, 45.6] | 47.4 [46.3, 48.1] | 1.77× [1.70, 1.89] | 1.86× [1.83, 1.92] | 1.05× [0.99, 1.08] |
| update-1 | 0.927 [0.866, 1.03] | 1.06 [0.996, 1.11] | 1.03 [0.966, 1.12] | 1.52 [1.4, 1.74] | 1.11× [0.97, 1.24] | 1.68× [1.47, 1.86] | 1.12× [1.00, 1.24] |
| bulk-update-100 | 2.32 [2.17, 2.54] | 2.51 [2.23, 2.82] | 2.57 [2.27, 2.72] | 3.48 [3.02, 4.01] | 1.05× [0.98, 1.16] | 1.44× [1.25, 1.74] | 1.09× [1.04, 1.19] |
| upsert-100 | 2.39 [2.34, 2.75] | 2.44 [2.35, 2.74] | 4.19 [4.03, 4.59] | 5.47 [5.01, 5.68] | 1.69× [1.48, 1.81] | 2.21× [1.90, 2.49] | 1.02× [0.91, 1.08] |
| delete-100 | 1.67 [1.51, 1.93] | 1.74 [1.49, 1.89] | 2.35 [2.08, 2.38] | 3.04 [2.77, 3.26] | 1.37× [1.16, 1.69] | 1.83× [1.57, 2.01] | 0.99× [0.81, 1.23] |
| tx-5 | 5.09 [4.69, 5.55] | 5.44 [4.98, 5.99] | 4.83 [4.49, 5.17] | 5.61 [5.02, 6.17] | 0.99× (slower) [0.88, 1.07] | 1.09× [0.98, 1.26] | 1.03× [0.98, 1.19] |
| concurrent | 90.7 [82.8, 93.1] | 94.3 [90.8, 97.8] | 246 [242, 255] | 241 [235, 249] | 2.77× [2.62, 2.99] | 2.77× [2.56, 2.84] | 1.04× [1.00, 1.14] |

### concurrent (50 tasks, 4 operations each, pool of 10)

| contender | ops/s | p99 latency (ms) |
|---|---|---|
| orm | 2206 | 45.1 |
| orm-all | 2120 | 45.4 |
| django-sync | 814 | 206 |
| django-async | 831 | 91 |

### CPU split (approximate)

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

## TypeScript on Node

pg 8.23.1, prisma 7.10.1, @prisma/client 7.10.1, @prisma/adapter-pg 7.10.1. orm: release build, revision c9bbf7c; orm-all: release build, revision c9bbf7c.

`pg` (prepared statements, no ORM work) is the floor: its column is the overhead orm / pg. The `orm-all / orm` column is the extension cost.

### Time per operation

| case | orm (ms) | orm-all (ms) | prisma (ms) | pg (ms) | orm vs prisma | orm / pg (overhead) | orm-all / orm (extension cost) |
|---|---|---|---|---|---|---|---|
| read-pk | 0.342 [0.316, 0.373] | 0.328 [0.325, 0.354] | 0.667 [0.648, 0.74] | 0.234 [0.223, 0.26] | 1.96× [1.81, 2.14] | 1.44× [1.34, 1.55] | 1.00× [0.92, 1.06] |
| read-50 | 0.671 [0.644, 0.698] | 0.671 [0.654, 0.738] | 1.11 [1, 1.15] | 0.611 [0.583, 0.659] | 1.60× [1.55, 1.72] | 1.08× [0.97, 1.20] | 1.07× [0.98, 1.12] |
| read-1000 | 4.29 [4.02, 4.33] | 4.19 [4.06, 4.47] | 6.98 [6.69, 7.08] | 4.24 [4.1, 4.34] | 1.67× [1.55, 1.73] | 1.00× [0.96, 1.03] | 0.99× [0.94, 1.07] |
| projection-1000 | 3.75 [3.49, 3.89] | 3.65 [3.51, 3.94] | 2.68 [2.44, 3.09] | 1.63 [1.54, 1.73] | 0.73× (slower) [0.67, 0.79] | 2.24× [2.17, 2.41] | 1.02× [0.97, 1.09] |
| filter | 0.542 [0.501, 0.593] | 0.555 [0.495, 0.604] | 0.784 [0.743, 0.837] | 0.334 [0.315, 0.397] | 1.48× [1.33, 1.56] | 1.54× [1.38, 1.71] | 1.01× [0.95, 1.10] |
| filter-ne-null | 0.537 [0.508, 0.593] | 0.572 [0.543, 0.587] | 0.762 [0.722, 0.829] | 0.353 [0.302, 0.434] | 1.37× [1.29, 1.45] | 1.56× [1.38, 1.76] | 1.01× [0.96, 1.12] |
| count | 0.833 [0.817, 0.88] | 0.874 [0.838, 0.946] | 1.09 [1.03, 1.12] | 0.734 [0.715, 0.782] | 1.26× [1.23, 1.32] | 1.10× [1.06, 1.19] | 0.99× [0.95, 1.12] |
| aggregate | 3.6 [3.26, 3.93] | 3.72 [3.51, 4.13] | 5.08 [4.76, 5.29] | 3.09 [2.94, 3.15] | 1.40× [1.31, 1.50] | 1.15× [1.09, 1.25] | 1.07× [0.91, 1.21] |
| join-to-one | 1.27 [1.19, 1.37] | 1.31 [1.24, 1.35] | 1.94 [1.89, 2.05] | 1.13 [1.08, 1.17] | 1.55× [1.47, 1.62] | 1.12× [1.05, 1.21] | 1.06× [0.91, 1.10] |
| prefetch-to-many | 3 [2.76, 3.12] | 2.96 [2.66, 3.12] | 5 [4.58, 5.34] | 2.81 [2.63, 2.91] | 1.65× [1.53, 1.78] | 1.05× [0.98, 1.14] | 1.02× [0.95, 1.06] |
| m2m | 1.43 [1.35, 1.53] | 1.34 [1.29, 1.44] | 3.08 [2.9, 3.43] | 1.14 [1.03, 1.2] | 2.20× [2.11, 2.43] | 1.29× [1.15, 1.45] | 0.92× [0.86, 1.00] |
| insert-1 | 0.737 [0.662, 0.793] | 0.742 [0.675, 0.822] | 1.08 [0.998, 1.16] | 0.652 [0.621, 0.691] | 1.48× [1.38, 1.57] | 1.13× [1.04, 1.35] | 1.01× [0.91, 1.16] |
| bulk-insert-100 | 3.82 [3.47, 3.94] | 3.63 [3.32, 3.98] | 13.2 [12.6, 14.4] | 3 [2.87, 3.18] | 3.71× [3.38, 3.89] | 1.21× [1.17, 1.27] | 1.01× [0.89, 1.08] |
| bulk-insert-1000 | 26.5 [25.7, 27.3] | 26 [24.4, 27.8] | 114 [110, 116] | 21.2 [20.7, 22.1] | 4.21× [4.17, 4.43] | 1.22× [1.16, 1.32] | 0.97× [0.92, 1.04] |
| update-1 | 0.735 [0.677, 0.819] | 0.709 [0.651, 0.769] | 0.975 [0.89, 1.04] | 0.556 [0.514, 0.604] | 1.33× [1.12, 1.40] | 1.42× [1.23, 1.54] | 0.99× [0.76, 1.14] |
| bulk-update-100 | 2.13 [1.96, 2.38] | 2.1 [2.01, 2.28] | 2.48 [2.32, 2.77] | 1.93 [1.77, 2.12] | 1.25× [0.97, 1.37] | 1.12× [0.91, 1.22] | 0.96× [0.94, 1.01] |
| upsert-100 | 2.21 [2.08, 2.58] | 2.2 [2.1, 2.31] | 41.2 [37.3, 42.3] | 1.83 [1.74, 1.94] | 18.67× [16.26, 19.52] | 1.18× [1.14, 1.27] | 0.95× [0.91, 1.03] |
| delete-100 | 1.45 [1.26, 1.68] | 1.37 [1.15, 1.59] | 1.32 [1.14, 1.57] | 1.11 [1.02, 1.17] | 0.91× (slower) [0.79, 1.09] | 1.38× [1.09, 1.66] | 0.91× [0.75, 1.14] |
| tx-5 | 3.38 [3.13, 3.8] | 3.29 [3.11, 3.93] | 4.83 [4.38, 5.04] | 1.86 [1.73, 2.05] | 1.38× [1.26, 1.56] | 1.87× [1.55, 2.08] | 0.97× [0.90, 1.20] |
| concurrent | 42.2 [40.2, 43.7] | 42.4 [39.8, 43.6] | 63.8 [61.6, 66] | 31.5 [30.4, 32.9] | 1.57× [1.38, 1.66] | 1.31× [1.21, 1.37] | 1.02× [0.98, 1.08] |

### concurrent (50 tasks, 4 operations each, pool of 10)

| contender | ops/s | p99 latency (ms) |
|---|---|---|
| orm | 4745 | 19.3 |
| orm-all | 4721 | 20.4 |
| prisma | 3133 | 31.1 |
| pg | 6352 | 14.5 |

### CPU split (approximate)

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

## TypeScript on Bun

Three cases only. There is no `pg` row on Bun.

### Time per operation

| case | orm (ms) | prisma (ms) | orm vs prisma |
|---|---|---|---|
| read-pk | 0.385 [0.36, 0.421] | 0.709 [0.698, 0.733] | 1.68× [1.59, 1.96] |
| read-1000 | 3.28 [3.06, 3.63] | 5.53 [5.18, 6.08] | 1.76× [1.58, 1.86] |
| bulk-insert-1000 | 26.1 [24.1, 26.6] | 93 [91.2, 96.1] | 3.64× [3.41, 3.87] |

## Rust

Skipped: there is no Rust runner.

## Notes

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
- `orm` prepared statements: engine default: prepared once per connection and cached
- `orm` pool: 10
- `orm` insert note: orm returns full rows: insert_many(...).returning() has no key-only form
- `orm` upsert note: on_conflict returns a row count, no RETURNING
- `django-sync` prepared statements: psycopg default: server-side prepare after 5 executions of one statement; Django inlines LIMIT/OFFSET, so the read-N statements differ per page and are not prepared
- `django-sync` pool: psycopg_pool min=max=10, one pool per process; each concurrent task returns its connection with close()
- `django-sync` async note: Django async ORM runs each call through sync_to_async (thread-sensitive): calls serialize on one thread
- `django-sync` delete note: QuerySet.delete() runs BEGIN, DELETE, COMMIT: 3 statements
- `django-sync` upsert note: bulk_create(update_conflicts=True) adds RETURNING id
- `django-async` prepared statements: psycopg default: server-side prepare after 5 executions of one statement; Django inlines LIMIT/OFFSET, so the read-N statements differ per page and are not prepared
- `django-async` pool: psycopg_pool min=max=10, one pool per process; each concurrent task returns its connection with close()
- `django-async` async note: Django async ORM runs each call through sync_to_async (thread-sensitive): calls serialize on one thread
- `django-async` delete note: QuerySet.delete() runs BEGIN, DELETE, COMMIT: 3 statements
- `django-async` upsert note: bulk_create(update_conflicts=True) adds RETURNING id
- `orm-all` prepared statements: engine default: prepared once per connection and cached
- `orm-all` pool: 10
- `orm-all` insert note: orm returns full rows: insert_many(...).returning() has no key-only form
- `orm-all` upsert note: on_conflict returns a row count, no RETURNING
- `orm` insert note: orm returns full rows: insertMany(...).returning() has no key-only form
- `orm` upsert note: onConflict returns a row count, no RETURNING
- `prisma` query engine: TypeScript query compiler (WASM) with the driver adapter; no Rust query engine
- `prisma` prepared statements: unnamed statements: the pg driver re-parses each call (adapter-pg without statementNameGenerator)
- `prisma` pool: 10
- `prisma` insert note: bulk inserts use createManyAndReturn with select { id: true }: ids only, orm returns full rows
- `prisma` upsert note: upsert-100 is 100 upserts in one $transaction: Prisma has no bulk upsert, orm and Django send one statement
- `prisma` find unique note: findUnique calls in one tick are batched by Prisma into one query; this runner issues them one at a time
- `prisma` update note: update-1 uses updateMany (no row read back); update() would add a SELECT
- `pg` prepared statements: named prepared statements, one name per statement text
- `pg` pool: 10
- `pg` join note: join-to-one uses an inner JOIN: posts.author_id is NOT NULL, so the rows equal the ORMs' joins
- `orm-all` insert note: orm returns full rows: insertMany(...).returning() has no key-only form
- `orm-all` upsert note: onConflict returns a row count, no RETURNING
