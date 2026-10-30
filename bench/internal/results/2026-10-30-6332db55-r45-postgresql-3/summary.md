# Internal benchmark, 2026-10-30T20:27:59Z

- Commit: `6332db55acf01a3c01c81079e6cc57dd245ac0bb`. Backend: postgresql. Rounds: 45.
- Load average (1 minute): 1.29 at the start, 1.17 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Intel(R) Xeon(R) Processor @ 2.10GHz, Linux x86_64, runtime 3.13.17, 2026-10-30T20:27:03Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 469.9 | 295.1 | 283.7 | 116.1 | 134.5 |
| filter_order_50 | 755.6 | 517.3 | 483.5 | 280.5 | 301.1 |
| read_1000 | 2,475.5 | 2,153.3 | 2,133.3 | 1,399.3 | 2,148.5 |
| filtered_count | 440.2 | 393.3 | 359.0 | 415.0 | 208.4 |
| insert_one | 772.7 | n/a | 661.3 | 452.9 | 417.4 |
| prefetch_posts | 1,021.1 | 808.4 | 737.6 | 400.8 | 575.5 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 174.8 (37%) | 11.5 (2%) | 167.5 (36%) | -18.3 (-4%) | 134.5 (29%) |
| filter_order_50 | 238.3 (32%) | 33.8 (4%) | 203.0 (27%) | -20.6 (-3%) | 301.1 (40%) |
| read_1000 | 322.2 (13%) | 20.0 (1%) | 734.0 (30%) | -749.2 (-30%) | 2,148.5 (87%) |
| filtered_count | 47.0 (11%) | 34.3 (8%) | -56.0 (-13%) | 206.5 (47%) | 208.4 (47%) |
| insert_one | 111.4 (14%) | – | 208.5 (27%) | 35.4 (5%) | 417.4 (54%) |
| prefetch_posts | 212.6 (21%) | 70.8 (7%) | 336.9 (33%) | -174.8 (-17%) | 575.5 (56%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: builder; filter_order_50: builder; read_1000: hop + instances; filtered_count: engine; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Intel(R) Xeon(R) Processor @ 2.10GHz, linux x64, runtime 22.22.0, 2026-10-30T20:27:30Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 225.6 | 176.3 | 181.8 | 116.1 | 153.2 |
| filter_order_50 | 665.9 | 562.4 | 544.1 | 280.5 | 382.0 |
| read_1000 | 2,945.3 | 2,940.5 | 2,806.0 | 1,399.3 | 2,862.2 |
| filtered_count | 403.2 | 363.2 | 336.1 | 415.0 | 262.5 |
| insert_one | 498.0 | n/a | 461.9 | 452.9 | 410.1 |
| prefetch_posts | 860.6 | 651.6 | 706.1 | 400.8 | 525.0 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 49.3 (22%) | -5.5 (-2%) | 65.7 (29%) | -37.1 (-16%) | 153.2 (68%) |
| filter_order_50 | 103.5 (16%) | 18.4 (3%) | 263.6 (40%) | -101.5 (-15%) | 382.0 (57%) |
| read_1000 | 4.8 (0%) | 134.6 (5%) | 1,406.7 (48%) | -1,462.9 (-50%) | 2,862.2 (97%) |
| filtered_count | 40.0 (10%) | 27.1 (7%) | -78.8 (-20%) | 152.5 (38%) | 262.5 (65%) |
| insert_one | 36.1 (7%) | – | 9.1 (2%) | 42.8 (9%) | 410.1 (82%) |
| prefetch_posts | 209.0 (24%) | -54.4 (-6%) | 305.3 (35%) | -124.2 (-14%) | 525.0 (61%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: engine; insert_one: engine; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
