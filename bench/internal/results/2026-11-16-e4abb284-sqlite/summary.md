# Internal benchmark, 2026-11-16T06:44:39Z

- Commit: `e4abb2846f5506587620cc3bd7f2a22e361092fa`. Backend: sqlite. Rounds: 15.
- Load average (1 minute): 1.99 at the start, 1.78 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Apple M5, Darwin arm64, runtime 3.14.7, 2026-11-16T06:44:15Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 362.4 | 198.4 | 219.2 | 73.0 | 30.2 |
| filter_order_50 | 705.3 | 405.6 | 486.0 | 288.6 | 247.6 |
| read_1000 | 4,010.0 | 3,039.6 | 2,909.5 | 1,485.0 | 2,350.5 |
| filtered_count | 396.7 | 343.4 | 355.3 | 203.4 | 191.5 |
| insert_one | 1,342.7 | n/a | 1,334.9 | 1,075.0 | 1,058.4 |
| prefetch_posts | 1,036.7 | 772.8 | 794.7 | 269.9 | 423.8 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 164.0 (45%) | -20.8 (-6%) | 146.2 (40%) | 42.8 (12%) | 30.2 (8%) |
| filter_order_50 | 299.8 (43%) | -80.5 (-11%) | 197.4 (28%) | 41.0 (6%) | 247.6 (35%) |
| read_1000 | 970.4 (24%) | 130.1 (3%) | 1,424.5 (36%) | -865.4 (-22%) | 2,350.5 (59%) |
| filtered_count | 53.4 (13%) | -11.9 (-3%) | 151.9 (38%) | 12.0 (3%) | 191.5 (48%) |
| insert_one | 7.8 (1%) | – | 259.9 (19%) | 16.6 (1%) | 1,058.4 (79%) |
| prefetch_posts | 263.9 (25%) | -21.9 (-2%) | 524.7 (51%) | -153.9 (-15%) | 423.8 (41%) |

Admin cases in µs per call (the route runs auth, the list or the export, and one audit row for an export):

| case | admin route | host encode | driver only | route ÷ driver |
|---|---|---|---|---|
| admin_list | 916.3 | – | 187.9 | 4.9× |
| admin_export | 4,899.4 | 16,711.8 | 2,423.1 | 2.0× |

`host encode` reads the same rows with the query set (the to-one author loaded) and writes the CSV in the host language; it is only in the export case.

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: builder; filter_order_50: builder; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Apple M5, darwin arm64, runtime 26.9.0, 2026-11-16T06:44:27Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 173.8 | 128.3 | 159.9 | 73.0 | 31.4 |
| filter_order_50 | 491.6 | 408.2 | 448.3 | 288.6 | 342.7 |
| read_1000 | 4,079.3 | 3,700.2 | 3,711.0 | 1,485.0 | 4,792.0 |
| filtered_count | 288.5 | 235.5 | 268.8 | 203.4 | 191.9 |
| insert_one | 1,268.6 | n/a | 1,209.5 | 1,075.0 | 1,012.0 |
| prefetch_posts | 800.9 | 674.9 | 730.5 | 269.9 | 587.5 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 45.5 (26%) | -31.6 (-18%) | 86.9 (50%) | 41.6 (24%) | 31.4 (18%) |
| filter_order_50 | 83.4 (17%) | -40.1 (-8%) | 159.7 (32%) | -54.1 (-11%) | 342.7 (70%) |
| read_1000 | 379.1 (9%) | -10.8 (-0%) | 2,225.9 (55%) | -3,307.0 (-81%) | 4,792.0 (117%) |
| filtered_count | 53.0 (18%) | -33.3 (-12%) | 65.4 (23%) | 11.5 (4%) | 191.9 (67%) |
| insert_one | 59.1 (5%) | – | 134.6 (11%) | 63.0 (5%) | 1,012.0 (80%) |
| prefetch_posts | 126.0 (16%) | -55.6 (-7%) | 460.5 (57%) | -317.6 (-40%) | 587.5 (73%) |

Admin cases in µs per call (the route runs auth, the list or the export, and one audit row for an export):

| case | admin route | host encode | driver only | route ÷ driver |
|---|---|---|---|---|
| admin_list | 771.5 | – | 251.9 | 3.1× |
| admin_export | 4,965.2 | 9,485.2 | 3,753.1 | 1.3× |

`host encode` reads the same rows with the query set (the to-one author loaded) and writes the CSV in the host language; it is only in the export case.

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
