# Benchmark summary 2026-10-28-d428e01

## Summary

Speed-up is contender time / `monorm` time: `2.3×` means `monorm` is 2.3 times faster. End to end: each time includes the database round trip.

- Python vs `django-sync`: geometric mean 1.59×, range 1.03× (tx-5) to 3.36× (concurrent) (20 cases)
- Python vs `django-async`: geometric mean 1.97×, range 1.10× (tx-5) to 3.55× (m2m) (20 cases)
- Python vs `sqlalchemy-sync`: geometric mean 2.25×, range 0.92× (slower) (tx-5) to 11.57× (bulk-insert-1000) (20 cases)
- Python vs `sqlalchemy-async`: geometric mean 2.56×, range 1.19× (tx-5) to 13.23× (bulk-insert-1000) (20 cases)
- Node vs `prisma`: geometric mean 1.90×, range 1.03× (delete-100) to 11.68× (upsert-100) (20 cases)
- Node vs `drizzle`: geometric mean 1.50×, range 0.97× (slower) (tx-5) to 3.95× (m2m) (20 cases)
- Bun vs `prisma`: geometric mean 2.48×, range 1.61× (read-1000) to 3.61× (bulk-insert-1000) (3 cases)
- Bun vs `drizzle`: geometric mean 1.46×, range 1.24× (read-1000) to 1.63× (bulk-insert-1000) (3 cases)

CPU split (build query + make SQL, contender time / `monorm` time): approximate, no database, decode not measured. The main numbers above are end to end.

- Python vs `django-sync`: geometric mean 4.11×, range 1.92× (prefetch-to-many) to 6.65× (read-1000) (10 cases)
- Python vs `django-async`: geometric mean 4.39×, range 2.05× (prefetch-to-many) to 7.12× (read-1000) (10 cases)
- Python vs `sqlalchemy-sync`: geometric mean 10.21×, range 5.96× (aggregate) to 24.99× (join-to-one) (10 cases)
- Python vs `sqlalchemy-async`: geometric mean 10.20×, range 6.28× (aggregate) to 24.09× (join-to-one) (10 cases)
- Node vs `drizzle`: geometric mean 2.42×, range 1.09× (aggregate) to 5.82× (join-to-one) (10 cases)
- Bun vs `drizzle`: geometric mean 1.16×, range 1.08× (read-pk) to 1.24× (read-1000) (2 cases)

All numbers are **measured** on this machine: 20 rounds x 20 iterations per case (the warm-up calls are not timed), the contender order rotated every round, one fresh process per contender and round. Each cell is `median [95 percent cluster bootstrap interval]` of the per-round medians; each speed-up is the median of the paired per-round values (contender time / monorm time) with its interval.

Load before: ` 21:10:00 up 58 min,  0 user,  load average: 1.00, 1.00, 0.96`  
Load after: ` 22:30:37 up  2:18,  0 user,  load average: 1.13, 1.20, 1.33`

1-minute load before each round (measured): min 1.14, max 1.97.

Build times (measured): build-python-wheel 202s, build-node-addon 193s, build-all-artifact 433s

## Python

monorm 0.1.0, django 6.1.2, psycopg 3.3.6, psycopg-binary 3.3.6, psycopg-pool 3.3.3, asgiref 3.12.1, sqlalchemy 2.1.6, asyncpg 0.32.0, greenlet 3.5.6. monorm: release build, revision d428e01; monorm-all: release build, revision d428e01.

There is no `pg`-like floor contender on Python. `monorm-all` is the `monorm` build with every extension; the `monorm-all / monorm` column is the extension cost.

### Time per operation

| case | monorm (ms) | monorm-all (ms) | django-sync (ms) | django-async (ms) | sqlalchemy-sync (ms) | sqlalchemy-async (ms) | monorm vs django-sync | monorm vs django-async | monorm vs sqlalchemy-sync | monorm vs sqlalchemy-async | monorm-all / monorm (extension cost) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| read-pk | 0.411 [0.351, 0.458] | 0.504 [0.463, 0.579] | 0.588 [0.573, 0.614] | 0.993 [0.869, 1.01] | 0.776 [0.712, 0.865] | 1.1 [1.05, 1.19] | 1.54× [1.08, 1.70] | 2.31× [2.12, 2.70] | 1.99× [1.68, 2.39] | 2.72× [2.39, 3.52] | 1.15× [1.00, 1.49] |
| read-50 | 0.669 [0.64, 0.727] | 0.757 [0.694, 0.811] | 0.857 [0.822, 1.01] | 1.43 [1.36, 1.46] | 1.17 [1.07, 1.25] | 1.46 [1.39, 1.53] | 1.32× [1.24, 1.39] | 2.04× [1.90, 2.31] | 1.67× [1.61, 1.94] | 2.20× [2.06, 2.27] | 1.07× [0.91, 1.28] |
| read-1000 | 2.39 [2.32, 2.49] | 2.29 [2.25, 2.35] | 7 [6.88, 7.08] | 7.71 [7.52, 7.98] | 6.63 [6.28, 6.92] | 6.56 [6.37, 6.76] | 2.87× [2.79, 3.02] | 3.26× [3.09, 3.36] | 2.72× [2.59, 2.90] | 2.77× [2.67, 2.81] | 0.97× [0.91, 1.02] |
| projection-1000 | 2.11 [2.06, 2.23] | 2.15 [2.08, 2.21] | 4.85 [4.75, 4.89] | 5.57 [5.43, 5.8] | 6.33 [6.16, 6.44] | 6.68 [6.52, 6.83] | 2.25× [2.17, 2.37] | 2.61× [2.49, 2.68] | 2.94× [2.79, 3.08] | 3.15× [2.99, 3.35] | 1.02× [0.95, 1.04] |
| filter | 0.791 [0.73, 0.854] | 0.807 [0.734, 0.871] | 1.04 [1, 1.1] | 1.52 [1.46, 1.58] | 1.21 [1.14, 1.34] | 1.51 [1.47, 1.59] | 1.29× [1.21, 1.46] | 1.91× [1.80, 2.04] | 1.56× [1.45, 1.70] | 1.92× [1.80, 2.14] | 1.02× [0.89, 1.10] |
| filter-ne-null | 0.792 [0.732, 0.832] | 0.753 [0.69, 0.781] | 1.03 [0.916, 1.14] | 1.51 [1.48, 1.55] | 1.14 [1.03, 1.28] | 1.52 [1.39, 1.59] | 1.33× [1.11, 1.57] | 1.90× [1.83, 2.11] | 1.49× [1.31, 1.71] | 1.97× [1.77, 2.21] | 0.94× [0.85, 1.07] |
| count | 1.01 [0.935, 1.08] | 0.976 [0.921, 1.08] | 1.16 [1.14, 1.22] | 1.61 [1.57, 1.65] | 1.52 [1.48, 1.62] | 1.81 [1.74, 1.88] | 1.15× [1.08, 1.28] | 1.58× [1.48, 1.78] | 1.56× [1.43, 1.71] | 1.80× [1.66, 1.98] | 0.95× [0.88, 1.12] |
| aggregate | 2.87 [2.8, 2.96] | 2.86 [2.81, 2.91] | 3.16 [3.1, 3.26] | 3.74 [3.68, 3.92] | 3.26 [3.21, 3.43] | 3.83 [3.7, 3.95] | 1.13× [1.05, 1.16] | 1.32× [1.30, 1.34] | 1.13× [1.10, 1.19] | 1.34× [1.29, 1.38] | 1.00× [0.97, 1.03] |
| join-to-one | 1.33 [1.26, 1.39] | 1.31 [1.26, 1.38] | 1.92 [1.87, 1.97] | 2.4 [2.29, 2.43] | 2.28 [2.17, 2.39] | 2.58 [2.48, 2.63] | 1.45× [1.43, 1.48] | 1.81× [1.73, 1.97] | 1.72× [1.60, 1.91] | 1.99× [1.86, 2.07] | 1.01× [0.92, 1.08] |
| prefetch-to-many | 2.23 [2.18, 2.31] | 2.3 [2.26, 2.38] | 6.49 [6.37, 6.58] | 7.11 [6.86, 7.22] | 5.87 [5.55, 6.18] | 5.84 [5.67, 6.15] | 2.89× [2.79, 2.98] | 3.20× [3.00, 3.29] | 2.68× [2.39, 2.80] | 2.64× [2.52, 2.74] | 1.02× [1.00, 1.05] |
| m2m | 1.3 [1.23, 1.33] | 1.25 [1.22, 1.3] | 4.18 [4.09, 4.38] | 4.69 [4.41, 4.73] | 3.5 [3.28, 3.63] | 3.27 [3.2, 3.39] | 3.24× [3.08, 3.48] | 3.55× [3.38, 3.83] | 2.62× [2.47, 2.86] | 2.62× [2.38, 2.70] | 0.96× [0.93, 1.01] |
| insert-1 | 0.816 [0.74, 0.881] | 0.816 [0.734, 0.851] | 0.896 [0.86, 0.94] | 1.32 [1.26, 1.38] | 1.44 [1.34, 1.52] | 1.79 [1.73, 1.9] | 1.07× [1.00, 1.31] | 1.63× [1.46, 1.78] | 1.85× [1.69, 1.98] | 2.25× [2.13, 2.68] | 1.09× [0.87, 1.17] |
| bulk-insert-100 | 2.95 [2.92, 3.12] | 3.03 [2.95, 3.06] | 4.8 [4.64, 4.84] | 5.24 [5.2, 5.29] | 21.8 [20.3, 23.7] | 23.3 [22.3, 24.6] | 1.60× [1.54, 1.64] | 1.76× [1.66, 1.81] | 7.23× [6.76, 7.93] | 7.77× [7.34, 8.34] | 1.01× [1.00, 1.06] |
| bulk-insert-1000 | 19.5 [18.7, 20] | 19.5 [19, 20.7] | 35 [33.3, 35.4] | 36.3 [35.6, 37.1] | 226 [221, 239] | 258 [253, 268] | 1.77× [1.74, 1.79] | 1.87× [1.79, 1.94] | 11.57× [11.29, 12.20] | 13.23× [12.53, 14.00] | 1.03× [1.00, 1.06] |
| update-1 | 0.924 [0.861, 0.982] | 0.951 [0.878, 1] | 1.04 [0.988, 1.1] | 1.46 [1.41, 1.52] | 1.56 [1.3, 1.66] | 2.26 [2.08, 2.32] | 1.11× [1.04, 1.21] | 1.62× [1.55, 1.74] | 1.63× [1.44, 1.82] | 2.27× [2.07, 2.58] | 1.03× [0.95, 1.14] |
| bulk-update-100 | 2.14 [2.06, 2.17] | 2.12 [2.02, 2.21] | 2.28 [2.19, 2.36] | 2.58 [2.53, 2.67] | 3.09 [3.02, 3.16] | 3.77 [3.6, 3.85] | 1.10× [1.00, 1.14] | 1.24× [1.19, 1.28] | 1.46× [1.38, 1.59] | 1.78× [1.66, 1.88] | 0.99× [0.91, 1.04] |
| upsert-100 | 2.14 [2.02, 2.16] | 2.19 [2.05, 2.25] | 3.5 [3.41, 3.56] | 3.92 [3.8, 4.03] | 8.18 [8.02, 8.39] | 8.25 [8.05, 8.6] | 1.66× [1.56, 1.78] | 1.87× [1.79, 1.94] | 3.85× [3.72, 4.16] | 4.00× [3.71, 4.16] | 1.02× [0.98, 1.09] |
| delete-100 | 1.26 [1.21, 1.32] | 1.32 [1.2, 1.37] | 1.7 [1.57, 1.77] | 2.1 [1.94, 2.21] | 1.74 [1.62, 1.8] | 2.24 [2.07, 2.42] | 1.32× [1.19, 1.42] | 1.70× [1.48, 1.81] | 1.35× [1.22, 1.48] | 1.82× [1.66, 1.94] | 1.06× [0.91, 1.10] |
| tx-5 | 3.94 [3.53, 4.09] | 3.99 [3.57, 4.07] | 3.83 [3.71, 4.15] | 4.21 [3.98, 4.39] | 3.54 [3.38, 3.75] | 4.53 [4.2, 4.76] | 1.03× [0.94, 1.15] | 1.10× [0.99, 1.20] | 0.92× (slower) [0.88, 1.02] | 1.19× [1.05, 1.27] | 1.00× [0.92, 1.01] |
| concurrent | 65.8 [64.5, 66.4] | 67.2 [66.5, 69.3] | 224 [218, 226] | 218 [214, 221] | 321 [317, 325] | 183 [177, 185] | 3.36× [3.32, 3.47] | 3.32× [3.19, 3.43] | 4.92× [4.75, 5.00] | 2.77× [2.67, 2.89] | 1.03× [1.00, 1.05] |

### concurrent (50 tasks, 4 operations each, pool of 10)

| contender | ops/s | p99 latency (ms) |
|---|---|---|
| monorm | 3040 | 30.8 |
| monorm-all | 2978 | 29.7 |
| django-sync | 895 | 177 |
| django-async | 917 | 68.3 |
| sqlalchemy-sync | 623 | 284 |
| sqlalchemy-async | 1091 | 126 |

### CPU split (approximate)

Measured with no database, mean of 20 repetitions, median over rounds. Approximate: `.sql()` and Django's `str(query)` inline the values, so the split is not the work of a real call. Decode time is not measured.

**monorm**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.00986 | 0.0323 |
| read-50 | 0.00348 | 0.0225 |
| read-1000 | 0.00368 | 0.0233 |
| projection-1000 | 0.0119 | 0.0384 |
| filter | 0.0198 | 0.0647 |
| filter-ne-null | 0.0256 | 0.0706 |
| aggregate | 0.0106 | 0.0431 |
| join-to-one | 0.0116 | 0.0465 |
| prefetch-to-many | 0.0138 | 0.0531 |
| m2m | 0.0132 | 0.0572 |

**monorm-all**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.0103 | 0.0301 |
| read-50 | 0.00355 | 0.0226 |
| read-1000 | 0.00367 | 0.0242 |
| projection-1000 | 0.0122 | 0.0371 |
| filter | 0.021 | 0.0624 |
| filter-ne-null | 0.0276 | 0.0702 |
| aggregate | 0.0112 | 0.0446 |
| join-to-one | 0.0132 | 0.0461 |
| prefetch-to-many | 0.0142 | 0.0524 |
| m2m | 0.0131 | 0.0539 |

**django-sync**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.036 | 0.119 |
| read-50 | 0.0122 | 0.15 |
| read-1000 | 0.0134 | 0.166 |
| projection-1000 | 0.0184 | 0.113 |
| filter | 0.0799 | 0.321 |
| filter-ne-null | 0.0831 | 0.295 |
| aggregate | 0.0967 | 0.249 |
| join-to-one | 0.0233 | 0.3 |
| prefetch-to-many | 0.017 | 0.112 |
| m2m | 0.0177 | 0.173 |

**django-async**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.0409 | 0.12 |
| read-50 | 0.0126 | 0.161 |
| read-1000 | 0.0143 | 0.178 |
| projection-1000 | 0.0171 | 0.123 |
| filter | 0.0781 | 0.344 |
| filter-ne-null | 0.0847 | 0.363 |
| aggregate | 0.0966 | 0.262 |
| join-to-one | 0.0236 | 0.319 |
| prefetch-to-many | 0.0188 | 0.118 |
| m2m | 0.0198 | 0.175 |

**sqlalchemy-sync**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.0297 | 0.502 |
| read-50 | 0.0212 | 0.448 |
| read-1000 | 0.0233 | 0.428 |
| projection-1000 | 0.0631 | 0.411 |
| filter | 0.0503 | 0.568 |
| filter-ne-null | 0.0413 | 0.557 |
| aggregate | 0.0253 | 0.294 |
| join-to-one | 0.0495 | 1.4 |
| prefetch-to-many | 0.0513 | 0.373 |
| m2m | 0.0508 | 0.509 |

**sqlalchemy-async**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.0299 | 0.503 |
| read-50 | 0.0209 | 0.428 |
| read-1000 | 0.023 | 0.414 |
| projection-1000 | 0.066 | 0.406 |
| filter | 0.0502 | 0.565 |
| filter-ne-null | 0.0428 | 0.591 |
| aggregate | 0.0263 | 0.31 |
| join-to-one | 0.0477 | 1.35 |
| prefetch-to-many | 0.0524 | 0.406 |
| m2m | 0.0491 | 0.474 |

## TypeScript on Node

pg 8.23.1, prisma 7.10.1, @prisma/client 7.10.1, @prisma/adapter-pg 7.10.1, drizzle-orm 0.45.4. monorm: release build, revision d428e01; monorm-all: release build, revision d428e01.

`pg` (prepared statements, no ORM work) is the floor: its column is the overhead monorm / pg. The `monorm-all / monorm` column is the extension cost.

### Time per operation

| case | monorm (ms) | monorm-all (ms) | prisma (ms) | drizzle (ms) | pg (ms) | monorm vs prisma | monorm vs drizzle | monorm / pg (overhead) | monorm-all / monorm (extension cost) |
|---|---|---|---|---|---|---|---|---|---|
| read-pk | 0.278 [0.254, 0.336] | 0.258 [0.239, 0.301] | 0.649 [0.609, 0.686] | 0.447 [0.42, 0.46] | 0.158 [0.143, 0.165] | 2.30× [2.10, 2.76] | 1.56× [1.45, 1.73] | 1.71× [1.57, 2.20] | 0.98× [0.90, 1.06] |
| read-50 | 0.486 [0.439, 0.551] | 0.52 [0.473, 0.551] | 0.95 [0.898, 1.02] | 0.742 [0.68, 0.919] | 0.558 [0.47, 0.62] | 1.94× [1.81, 2.14] | 1.55× [1.42, 1.80] | 0.93× [0.80, 1.05] | 1.10× [0.86, 1.24] |
| read-1000 | 2.45 [2.41, 2.47] | 2.48 [2.39, 2.57] | 4.75 [4.5, 4.87] | 3.51 [3.42, 3.64] | 2.97 [2.84, 3.11] | 1.96× [1.87, 2.07] | 1.43× [1.39, 1.53] | 0.83× [0.78, 0.86] | 1.03× [0.99, 1.06] |
| projection-1000 | 1.67 [1.62, 1.75] | 1.61 [1.57, 1.69] | 2.07 [1.99, 2.13] | 1.69 [1.63, 1.76] | 1.25 [1.21, 1.34] | 1.22× [1.14, 1.29] | 1.01× [0.96, 1.06] | 1.38× [1.21, 1.42] | 0.96× [0.93, 1.03] |
| filter | 0.585 [0.472, 0.645] | 0.47 [0.441, 0.541] | 0.807 [0.717, 0.841] | 0.582 [0.528, 0.616] | 0.212 [0.202, 0.236] | 1.40× [1.26, 1.58] | 1.11× [0.94, 1.25] | 2.46× [2.01, 2.87] | 0.85× [0.71, 1.06] |
| filter-ne-null | 0.46 [0.418, 0.488] | 0.483 [0.424, 0.51] | 0.643 [0.628, 0.731] | 0.549 [0.492, 0.624] | 0.233 [0.205, 0.278] | 1.61× [1.41, 1.73] | 1.33× [1.02, 1.47] | 1.78× [1.68, 2.03] | 1.08× [0.91, 1.19] |
| count | 0.761 [0.722, 0.812] | 0.774 [0.734, 0.825] | 1.17 [1.11, 1.2] | 0.931 [0.89, 0.982] | 0.568 [0.531, 0.623] | 1.52× [1.44, 1.58] | 1.27× [1.19, 1.29] | 1.38× [1.19, 1.50] | 0.99× [0.95, 1.09] |
| aggregate | 2.57 [2.5, 2.64] | 2.55 [2.5, 2.62] | 3.63 [3.56, 3.7] | 3.23 [3.21, 3.26] | 2.55 [2.51, 2.58] | 1.42× [1.35, 1.46] | 1.27× [1.24, 1.30] | 1.03× [0.97, 1.05] | 1.00× [0.96, 1.02] |
| join-to-one | 1.13 [1.09, 1.17] | 1.14 [1.08, 1.21] | 1.83 [1.73, 1.93] | 1.84 [1.8, 1.88] | 1.03 [1.02, 1.16] | 1.62× [1.51, 1.68] | 1.60× [1.55, 1.69] | 1.05× [1.00, 1.13] | 1.02× [0.93, 1.09] |
| prefetch-to-many | 1.94 [1.9, 1.96] | 1.86 [1.82, 1.94] | 3.57 [3.5, 3.75] | 7.05 [6.73, 7.61] | 2.04 [1.95, 2.14] | 1.87× [1.79, 1.91] | 3.63× [3.48, 3.97] | 0.95× [0.89, 0.99] | 0.96× [0.94, 1.03] |
| m2m | 1.19 [1.13, 1.24] | 1.19 [1.13, 1.24] | 2.79 [2.69, 2.97] | 4.68 [4.55, 5.05] | 0.698 [0.661, 0.794] | 2.40× [2.25, 2.51] | 3.95× [3.69, 4.48] | 1.61× [1.44, 1.82] | 0.96× [0.93, 1.07] |
| insert-1 | 0.651 [0.518, 0.758] | 0.55 [0.517, 0.648] | 1.08 [1.01, 1.18] | 0.87 [0.747, 0.954] | 0.5 [0.414, 0.603] | 1.64× [1.53, 2.04] | 1.31× [1.19, 1.46] | 1.25× [1.14, 1.50] | 0.86× [0.76, 1.01] |
| bulk-insert-100 | 2.8 [2.69, 2.92] | 2.86 [2.74, 2.93] | 9.19 [8.96, 9.33] | 4.8 [4.68, 4.87] | 2.48 [2.44, 2.51] | 3.29× [3.10, 3.42] | 1.68× [1.63, 1.79] | 1.13× [1.09, 1.19] | 1.01× [0.92, 1.07] |
| bulk-insert-1000 | 18.8 [18.4, 19.1] | 19.4 [18.9, 20.2] | 83.6 [82.1, 85.6] | 39.5 [38.4, 41.1] | 16.1 [15.8, 16.5] | 4.47× [4.31, 4.64] | 2.11× [2.05, 2.18] | 1.16× [1.15, 1.19] | 1.02× [1.00, 1.05] |
| update-1 | 0.646 [0.565, 0.727] | 0.651 [0.542, 0.72] | 0.925 [0.862, 1] | 0.683 [0.635, 0.771] | 0.399 [0.367, 0.415] | 1.37× [1.23, 1.68] | 1.08× [0.94, 1.27] | 1.64× [1.44, 1.95] | 1.05× [0.76, 1.28] |
| bulk-update-100 | 1.87 [1.8, 1.99] | 1.94 [1.82, 2] | 2.05 [1.94, 2.13] | 1.92 [1.8, 2.01] | 1.6 [1.54, 1.64] | 1.08× [0.99, 1.18] | 0.99× (slower) [0.94, 1.05] | 1.20× [1.13, 1.25] | 1.04× [0.94, 1.10] |
| upsert-100 | 1.95 [1.83, 2] | 1.85 [1.81, 2] | 22.6 [21.6, 23.8] | 3.83 [3.77, 3.88] | 1.63 [1.54, 1.71] | 11.68× [10.99, 12.66] | 1.99× [1.93, 2.06] | 1.15× [1.12, 1.29] | 0.99× [0.93, 1.05] |
| delete-100 | 1.1 [1.01, 1.21] | 1.04 [0.983, 1.13] | 1.09 [1.04, 1.15] | 1.14 [1.1, 1.18] | 0.884 [0.75, 0.955] | 1.03× [0.91, 1.07] | 1.05× [0.88, 1.12] | 1.26× [1.07, 1.56] | 0.91× [0.83, 1.01] |
| tx-5 | 2.48 [2.35, 2.63] | 2.4 [2.25, 2.56] | 3.53 [3.32, 3.6] | 2.56 [2.06, 3.03] | 1.19 [0.994, 1.5] | 1.40× [1.33, 1.53] | 0.97× (slower) [0.89, 1.22] | 2.13× [1.70, 2.55] | 0.99× [0.91, 1.05] |
| concurrent | 27.3 [25.5, 29.9] | 27.8 [25.5, 31.2] | 42.7 [41.2, 47.7] | 47.8 [44.6, 49.9] | 18.3 [17.9, 18.7] | 1.59× [1.40, 1.78] | 1.78× [1.58, 1.85] | 1.43× [1.34, 1.70] | 1.03× [0.93, 1.15] |

### concurrent (50 tasks, 4 operations each, pool of 10)

| contender | ops/s | p99 latency (ms) |
|---|---|---|
| monorm | 7339 | 14 |
| monorm-all | 7191 | 13.6 |
| prisma | 4687 | 28 |
| drizzle | 4186 | 20.3 |
| pg | 10941 | 9.81 |

### CPU split (approximate)

Measured with no database, mean of 20 repetitions, median over rounds. Approximate: `.sql()` and Django's `str(query)` inline the values, so the split is not the work of a real call. Decode time is not measured.

**monorm**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.00826 | 0.0329 |
| read-50 | 0.00308 | 0.0253 |
| read-1000 | 0.00386 | 0.0191 |
| projection-1000 | 0.00822 | 0.0291 |
| filter | 0.0142 | 0.0395 |
| filter-ne-null | 0.0157 | 0.0387 |
| aggregate | 0.0073 | 0.0381 |
| join-to-one | 0.0109 | 0.0342 |
| prefetch-to-many | 0.0126 | 0.0383 |
| m2m | 0.0149 | 0.0464 |

**monorm-all**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.00823 | 0.0323 |
| read-50 | 0.00317 | 0.0252 |
| read-1000 | 0.00398 | 0.0197 |
| projection-1000 | 0.00819 | 0.0279 |
| filter | 0.014 | 0.0392 |
| filter-ne-null | 0.0158 | 0.0413 |
| aggregate | 0.00749 | 0.0381 |
| join-to-one | 0.0109 | 0.0354 |
| prefetch-to-many | 0.0128 | 0.0416 |
| m2m | 0.0142 | 0.0452 |

**drizzle**

| case | build query (ms) | make SQL (ms) |
|---|---|---|
| read-pk | 0.0072 | 0.0952 |
| read-50 | 0.0052 | 0.0663 |
| read-1000 | 0.00536 | 0.0682 |
| projection-1000 | 0.00475 | 0.0365 |
| filter | 0.0105 | 0.104 |
| filter-ne-null | 0.00868 | 0.0816 |
| aggregate | 0.00519 | 0.0441 |
| join-to-one | 0.000608 | 0.262 |
| prefetch-to-many | 0.000669 | 0.163 |
| m2m | 0.000605 | 0.267 |

## TypeScript on Bun

Three cases only. There is no `pg` row on Bun.

### Time per operation

| case | monorm (ms) | prisma (ms) | drizzle (ms) | monorm vs prisma | monorm vs drizzle |
|---|---|---|---|---|---|
| read-pk | 0.259 [0.239, 0.292] | 0.687 [0.643, 0.755] | 0.43 [0.359, 0.466] | 2.63× [2.45, 2.85] | 1.54× [1.30, 1.72] |
| read-1000 | 2.02 [2, 2.13] | 3.23 [3.19, 3.32] | 2.51 [2.44, 2.6] | 1.61× [1.50, 1.64] | 1.24× [1.18, 1.27] |
| bulk-insert-1000 | 17.5 [17.2, 17.9] | 63.7 [62.7, 64.8] | 28.8 [28.4, 29.2] | 3.61× [3.51, 3.71] | 1.63× [1.58, 1.68] |

## Rust

Skipped: there is no Rust runner.

## Notes

- Prisma `upsert-100` runs 100 upserts in one `$transaction`: Prisma has no bulk upsert. monorm and Django send one statement.
- Django async `concurrent` runs each `aget` through thread-sensitive `sync_to_async`: the calls run one after another on one thread.
- psycopg prepares a statement after 5 executions of it. Django inlines LIMIT/OFFSET, so the read-N statements differ per page and are not prepared.
- Prisma batches `findUnique` calls of one tick into one query (recalled from the Prisma docs, not checked here), so its first reads in `concurrent` can become about one round trip.
- django-sync `concurrent`: an untimed setup closes the main-thread connection first, so all 10 pooled connections serve the 50 threads (each task returns its connection with `close()`).
- Prisma `update-1` uses `updateMany` (count only), not `update` (returns the row): the same work as the other contenders.
- `pg` `join-to-one` uses an inner JOIN. `posts.author_id` is NOT NULL, so the rows equal the ORMs' joins.
- The JS `camelRow`/`data()` row conversion runs inside the timed operation: a user does the same work.
- Django `delete-100` (`QuerySet.delete()`) runs BEGIN, DELETE, COMMIT: 3 statements.
- monorm returns full rows from `bulk-insert-*`: `insertMany(...).returning()` has no key-only form. Prisma and Django return ids only or full instances as noted per contender below.
- Warm-up: every case runs 50 untimed calls before the timed calls, the same count for every contender. Timed calls use iterations 1..N, the warm-up uses distinct iteration numbers.
- Contenders run one per process and round, in a rotating order, on a database reset before every process.
- `monorm` prepared statements: engine default: prepared once per connection and cached
- `monorm` pool: 10
- `monorm` insert note: monorm returns full rows: insert_many(...).returning() has no key-only form
- `monorm` upsert note: on_conflict returns a row count, no RETURNING
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
- `sqlalchemy-sync` prepared statements: psycopg default: server-side prepare after 5 executions of one statement
- `sqlalchemy-sync` pool: QueuePool pool_size=10 max_overflow=0, one engine per process
- `sqlalchemy-sync` session note: one session per operation; the session begins a transaction on first use, so a read runs BEGIN, SELECT, ROLLBACK and a write runs BEGIN, the statement, COMMIT
- `sqlalchemy-sync` insert note: bulk-insert-* is insert(Comment).returning(Comment): full instances, as monorm returns
- `sqlalchemy-sync` upsert note: on_conflict_do_update adds RETURNING id: SQLAlchemy has no rowcount for a multi-row VALUES
- `sqlalchemy-sync` update note: update()/delete() run with synchronize_session='auto' (the default): bulk-update-100 falls back to 'fetch' and adds RETURNING id
- `sqlalchemy-sync` tx note: tx-5 flushes its two inserts as one INSERT statement (insertmanyvalues): 4 statements, not 5
- `sqlalchemy-async` prepared statements: asyncpg dialect default: a prepared-statement cache per connection (100 statements)
- `sqlalchemy-async` pool: QueuePool pool_size=10 max_overflow=0, one engine per process
- `sqlalchemy-async` session note: one session per operation; the session begins a transaction on first use, so a read runs BEGIN, SELECT, ROLLBACK and a write runs BEGIN, the statement, COMMIT
- `sqlalchemy-async` insert note: bulk-insert-* is insert(Comment).returning(Comment): full instances, as monorm returns
- `sqlalchemy-async` upsert note: on_conflict_do_update adds RETURNING id: SQLAlchemy has no rowcount for a multi-row VALUES
- `sqlalchemy-async` update note: update()/delete() run with synchronize_session='auto' (the default): bulk-update-100 falls back to 'fetch' and adds RETURNING id
- `sqlalchemy-async` tx note: tx-5 flushes its two inserts as one INSERT statement (insertmanyvalues): 4 statements, not 5
- `sqlalchemy-async` async note: AsyncSession runs the sync ORM in a greenlet per call
- `monorm-all` prepared statements: engine default: prepared once per connection and cached
- `monorm-all` pool: 10
- `monorm-all` insert note: monorm returns full rows: insert_many(...).returning() has no key-only form
- `monorm-all` upsert note: on_conflict returns a row count, no RETURNING
- `monorm` insert note: monorm returns full rows: insertMany(...).returning() has no key-only form
- `monorm` upsert note: onConflict returns a row count, no RETURNING
- `prisma` query engine: TypeScript query compiler (WASM) with the driver adapter; no Rust query engine
- `prisma` prepared statements: unnamed statements: the pg driver re-parses each call (adapter-pg without statementNameGenerator)
- `prisma` pool: 10
- `prisma` insert note: bulk inserts use createManyAndReturn with select { id: true }: ids only, monorm returns full rows
- `prisma` upsert note: upsert-100 is 100 upserts in one $transaction: Prisma has no bulk upsert, monorm and Django send one statement
- `prisma` find unique note: findUnique calls in one tick are batched by Prisma into one query; this runner issues them one at a time
- `prisma` update note: update-1 uses updateMany (no row read back); update() would add a SELECT
- `drizzle` prepared statements: Drizzle default: unnamed statements, no prepare (`.prepare(name)` is opt-in)
- `drizzle` pool: pg.Pool max=10
- `drizzle` relation note: join-to-one, prefetch-to-many and m2m use relational queries (db.query): one SQL statement with json subqueries per relation
- `drizzle` insert note: bulk-insert-* returns ids only (`returning({ id })`), as Prisma and pg do
- `pg` prepared statements: named prepared statements, one name per statement text
- `pg` pool: 10
- `pg` join note: join-to-one uses an inner JOIN: posts.author_id is NOT NULL, so the rows equal the ORMs' joins
- `monorm-all` insert note: monorm returns full rows: insertMany(...).returning() has no key-only form
- `monorm-all` upsert note: onConflict returns a row count, no RETURNING
