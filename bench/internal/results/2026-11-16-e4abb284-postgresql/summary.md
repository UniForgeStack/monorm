# Internal benchmark, 2026-11-16T06:47:15Z

- Commit: `e4abb2846f5506587620cc3bd7f2a22e361092fa`. Backend: postgresql. Rounds: 15.
- Load average (1 minute): 1.89 at the start, 1.98 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Apple M5, Darwin arm64, runtime 3.14.7, 2026-11-16T06:46:58Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 307.9 | 178.1 | 222.5 | 105.1 | 159.6 |
| filter_order_50 | 376.6 | 264.9 | 286.9 | 221.4 | 235.9 |
| read_1000 | 3,460.2 | 2,557.7 | 2,581.6 | 1,184.9 | 3,362.6 |
| filtered_count | 306.8 | 268.2 | 284.8 | 158.2 | 200.0 |
| insert_one | 333.4 | n/a | 310.1 | 164.7 | 220.3 |
| prefetch_posts | 656.5 | 406.0 | 410.5 | 360.9 | 345.0 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 129.7 (42%) | -44.4 (-14%) | 117.4 (38%) | -54.5 (-18%) | 159.6 (52%) |
| filter_order_50 | 111.8 (30%) | -22.1 (-6%) | 65.6 (17%) | -14.5 (-4%) | 235.9 (63%) |
| read_1000 | 902.5 (26%) | -24.0 (-1%) | 1,396.8 (40%) | -2,177.7 (-63%) | 3,362.6 (97%) |
| filtered_count | 38.6 (13%) | -16.5 (-5%) | 126.5 (41%) | -41.7 (-14%) | 200.0 (65%) |
| insert_one | 23.3 (7%) | – | 145.4 (44%) | -55.7 (-17%) | 220.3 (66%) |
| prefetch_posts | 250.5 (38%) | -4.5 (-1%) | 49.6 (8%) | 15.8 (2%) | 345.0 (53%) |

Admin cases in µs per call (the route runs auth, the list or the export, and one audit row for an export):

| case | admin route | host encode | driver only | route ÷ driver |
|---|---|---|---|---|
| admin_list | 1,072.2 | – | 240.4 | 4.5× |
| admin_export | 4,867.8 | 14,044.6 | 1,994.3 | 2.4× |

`host encode` reads the same rows with the query set (the to-one author loaded) and writes the CSV in the host language; it is only in the export case.

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: builder; filter_order_50: builder; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: builder.

## TypeScript on Node

Apple M5, darwin arm64, runtime 26.9.0, 2026-11-16T06:47:06Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 176.7 | 137.5 | 161.7 | 105.1 | 144.2 |
| filter_order_50 | 314.7 | 301.7 | 333.6 | 221.4 | 397.6 |
| read_1000 | 4,675.5 | 3,927.4 | 4,838.9 | 1,184.9 | 6,162.0 |
| filtered_count | 210.4 | 166.6 | 191.0 | 158.2 | 158.2 |
| insert_one | 238.0 | n/a | 225.6 | 164.7 | 179.2 |
| prefetch_posts | 613.9 | 466.0 | 781.3 | 360.9 | 616.4 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 39.2 (22%) | -24.2 (-14%) | 56.6 (32%) | -39.0 (-22%) | 144.2 (82%) |
| filter_order_50 | 13.1 (4%) | -32.0 (-10%) | 112.2 (36%) | -176.2 (-56%) | 397.6 (126%) |
| read_1000 | 748.1 (16%) | -911.5 (-19%) | 3,654.0 (78%) | -4,977.1 (-106%) | 6,162.0 (132%) |
| filtered_count | 43.9 (21%) | -24.5 (-12%) | 32.8 (16%) | 0.0 (0%) | 158.2 (75%) |
| insert_one | 12.4 (5%) | – | 60.9 (26%) | -14.5 (-6%) | 179.2 (75%) |
| prefetch_posts | 147.9 (24%) | -315.3 (-51%) | 420.4 (68%) | -255.6 (-42%) | 616.4 (100%) |

Admin cases in µs per call (the route runs auth, the list or the export, and one audit row for an export):

| case | admin route | host encode | driver only | route ÷ driver |
|---|---|---|---|---|
| admin_list | 633.0 | – | 229.9 | 2.8× |
| admin_export | 4,779.6 | 10,639.6 | 2,652.7 | 1.8× |

`host encode` reads the same rows with the query set (the to-one author loaded) and writes the CSV in the host language; it is only in the export case.

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: builder; insert_one: hop + instances; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
