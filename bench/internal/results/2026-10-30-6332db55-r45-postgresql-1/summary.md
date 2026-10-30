# Internal benchmark, 2026-10-30T20:24:25Z

- Commit: `6332db55acf01a3c01c81079e6cc57dd245ac0bb`. Backend: postgresql. Rounds: 45.
- Load average (1 minute): 0.79 at the start, 1.2 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Intel(R) Xeon(R) Processor @ 2.10GHz, Linux x86_64, runtime 3.13.17, 2026-10-30T20:23:28Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 427.9 | 308.4 | 274.2 | 106.9 | 130.5 |
| filter_order_50 | 771.1 | 511.7 | 507.5 | 247.4 | 298.4 |
| read_1000 | 2,331.5 | 2,124.1 | 2,066.0 | 1,298.6 | 2,127.7 |
| filtered_count | 573.0 | 487.3 | 445.4 | 198.3 | 228.7 |
| insert_one | 875.0 | n/a | 790.3 | 355.4 | 473.6 |
| prefetch_posts | 1,110.2 | 905.5 | 861.0 | 402.0 | 608.3 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 119.5 (28%) | 34.2 (8%) | 167.3 (39%) | -23.6 (-6%) | 130.5 (31%) |
| filter_order_50 | 259.4 (34%) | 4.2 (1%) | 260.2 (34%) | -51.0 (-7%) | 298.4 (39%) |
| read_1000 | 207.4 (9%) | 58.1 (2%) | 767.4 (33%) | -829.0 (-36%) | 2,127.7 (91%) |
| filtered_count | 85.7 (15%) | 41.9 (7%) | 247.1 (43%) | -30.3 (-5%) | 228.7 (40%) |
| insert_one | 84.7 (10%) | – | 434.9 (50%) | -118.2 (-14%) | 473.6 (54%) |
| prefetch_posts | 204.7 (18%) | 44.4 (4%) | 459.0 (41%) | -206.3 (-19%) | 608.3 (55%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Intel(R) Xeon(R) Processor @ 2.10GHz, linux x64, runtime 22.22.0, 2026-10-30T20:23:58Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 228.4 | 184.8 | 172.1 | 106.9 | 146.3 |
| filter_order_50 | 571.2 | 435.5 | 423.6 | 247.4 | 366.7 |
| read_1000 | 3,059.3 | 2,838.7 | 2,860.1 | 1,298.6 | 3,197.6 |
| filtered_count | 378.2 | 332.6 | 323.5 | 198.3 | 271.5 |
| insert_one | 563.5 | n/a | 508.5 | 355.4 | 464.9 |
| prefetch_posts | 896.5 | 723.5 | 745.8 | 402.0 | 575.2 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 43.6 (19%) | 12.8 (6%) | 65.2 (29%) | -39.4 (-17%) | 146.3 (64%) |
| filter_order_50 | 135.8 (24%) | 11.8 (2%) | 176.2 (31%) | -119.3 (-21%) | 366.7 (64%) |
| read_1000 | 220.5 (7%) | -21.3 (-1%) | 1,561.4 (51%) | -1,899.0 (-62%) | 3,197.6 (105%) |
| filtered_count | 45.6 (12%) | 9.1 (2%) | 125.1 (33%) | -73.2 (-19%) | 271.5 (72%) |
| insert_one | 55.0 (10%) | – | 153.1 (27%) | -109.6 (-19%) | 464.9 (83%) |
| prefetch_posts | 173.0 (19%) | -22.3 (-2%) | 343.8 (38%) | -173.1 (-19%) | 575.2 (64%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
