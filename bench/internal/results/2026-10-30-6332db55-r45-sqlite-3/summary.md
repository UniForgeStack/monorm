# Internal benchmark, 2026-10-30T20:28:49Z

- Commit: `6332db55acf01a3c01c81079e6cc57dd245ac0bb`. Backend: sqlite. Rounds: 45.
- Load average (1 minute): 1.17 at the start, 1.12 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Intel(R) Xeon(R) Processor @ 2.10GHz, Linux x86_64, runtime 3.13.17, 2026-10-30T20:27:59Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 327.9 | 234.4 | 221.3 | 45.9 | 9.2 |
| filter_order_50 | 732.2 | 519.4 | 486.6 | 163.7 | 93.4 |
| read_1000 | 2,279.0 | 1,990.1 | 1,960.7 | 1,341.4 | 1,405.5 |
| filtered_count | 300.9 | 266.5 | 249.3 | 82.5 | 50.1 |
| insert_one | 1,555.4 | n/a | 1,468.3 | 1,032.9 | 955.6 |
| prefetch_posts | 971.5 | 741.0 | 706.8 | 246.5 | 190.5 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 93.5 (29%) | 13.1 (4%) | 175.4 (54%) | 36.6 (11%) | 9.2 (3%) |
| filter_order_50 | 212.8 (29%) | 32.8 (4%) | 322.9 (44%) | 70.3 (10%) | 93.4 (13%) |
| read_1000 | 288.9 (13%) | 29.3 (1%) | 619.3 (27%) | -64.0 (-3%) | 1,405.5 (62%) |
| filtered_count | 34.5 (11%) | 17.2 (6%) | 166.7 (55%) | 32.5 (11%) | 50.1 (17%) |
| insert_one | 87.1 (6%) | – | 435.5 (28%) | 77.3 (5%) | 955.6 (61%) |
| prefetch_posts | 230.6 (24%) | 34.1 (4%) | 460.4 (47%) | 55.9 (6%) | 190.5 (20%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Intel(R) Xeon(R) Processor @ 2.10GHz, linux x64, runtime 22.22.0, 2026-10-30T20:28:25Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 168.6 | 137.8 | 136.2 | 45.9 | 9.5 |
| filter_order_50 | 455.7 | 375.7 | 339.0 | 163.7 | 132.7 |
| read_1000 | 2,261.8 | 2,232.3 | 2,218.1 | 1,341.4 | 1,883.2 |
| filtered_count | 160.0 | 147.4 | 139.9 | 82.5 | 42.5 |
| insert_one | 1,268.3 | n/a | 1,237.4 | 1,032.9 | 935.6 |
| prefetch_posts | 657.2 | 550.1 | 542.3 | 246.5 | 216.2 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 30.8 (18%) | 1.5 (1%) | 90.4 (54%) | 36.3 (22%) | 9.5 (6%) |
| filter_order_50 | 80.0 (18%) | 36.7 (8%) | 175.4 (38%) | 31.0 (7%) | 132.7 (29%) |
| read_1000 | 29.4 (1%) | 14.2 (1%) | 876.7 (39%) | -541.7 (-24%) | 1,883.2 (83%) |
| filtered_count | 12.6 (8%) | 7.5 (5%) | 57.4 (36%) | 40.1 (25%) | 42.5 (27%) |
| insert_one | 30.8 (2%) | – | 204.6 (16%) | 97.2 (8%) | 935.6 (74%) |
| prefetch_posts | 107.0 (16%) | 7.9 (1%) | 295.8 (45%) | 30.2 (5%) | 216.2 (33%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
