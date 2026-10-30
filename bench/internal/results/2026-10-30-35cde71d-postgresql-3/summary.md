# Internal benchmark, 2026-10-30T19:57:28Z

- Commit: `35cde71df68f86385578ee908dfd7d63a20b5c5d`. Backend: postgresql. Rounds: 15.
- Load average (1 minute): 1.07 at the start, 1.06 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Intel(R) Xeon(R) Processor @ 2.10GHz, Linux x86_64, runtime 3.13.17, 2026-10-30T19:57:10Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 400.0 | 337.7 | 289.7 | 115.1 | 132.6 |
| filter_order_50 | 766.3 | 499.7 | 425.7 | 281.5 | 277.6 |
| read_1000 | 2,307.3 | 2,064.3 | 1,987.9 | 1,223.8 | 2,053.4 |
| filtered_count | 539.9 | 449.0 | 396.2 | 213.9 | 204.4 |
| insert_one | 836.4 | n/a | 692.5 | 389.5 | 458.1 |
| prefetch_posts | 1,023.1 | 821.8 | 806.2 | 438.4 | 495.9 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 62.4 (16%) | 48.0 (12%) | 174.6 (44%) | -17.6 (-4%) | 132.6 (33%) |
| filter_order_50 | 266.6 (35%) | 74.0 (10%) | 144.2 (19%) | 4.0 (1%) | 277.6 (36%) |
| read_1000 | 243.0 (11%) | 76.4 (3%) | 764.2 (33%) | -829.7 (-36%) | 2,053.4 (89%) |
| filtered_count | 90.9 (17%) | 52.8 (10%) | 182.3 (34%) | 9.5 (2%) | 204.4 (38%) |
| insert_one | 143.9 (17%) | – | 303.0 (36%) | -68.6 (-8%) | 458.1 (55%) |
| prefetch_posts | 201.3 (20%) | 15.6 (2%) | 367.8 (36%) | -57.5 (-6%) | 495.9 (48%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: builder; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Intel(R) Xeon(R) Processor @ 2.10GHz, linux x64, runtime 22.22.0, 2026-10-30T19:57:19Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 249.3 | 206.5 | 182.4 | 115.1 | 182.9 |
| filter_order_50 | 530.3 | 465.9 | 416.4 | 281.5 | 379.9 |
| read_1000 | 2,733.6 | 2,862.9 | 2,609.2 | 1,223.8 | 2,965.1 |
| filtered_count | 287.8 | 251.7 | 252.7 | 213.9 | 196.0 |
| insert_one | 543.0 | n/a | 482.0 | 389.5 | 347.9 |
| prefetch_posts | 882.9 | 754.5 | 782.2 | 438.4 | 623.4 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 42.9 (17%) | 24.1 (10%) | 67.3 (27%) | -67.9 (-27%) | 182.9 (73%) |
| filter_order_50 | 64.3 (12%) | 49.5 (9%) | 134.9 (25%) | -98.4 (-19%) | 379.9 (72%) |
| read_1000 | -129.3 (-5%) | 253.7 (9%) | 1,385.4 (51%) | -1,741.3 (-64%) | 2,965.1 (108%) |
| filtered_count | 36.1 (13%) | -1.0 (-0%) | 38.8 (13%) | 17.8 (6%) | 196.0 (68%) |
| insert_one | 60.9 (11%) | – | 92.5 (17%) | 41.6 (8%) | 347.9 (64%) |
| prefetch_posts | 128.4 (15%) | -27.7 (-3%) | 343.8 (39%) | -185.1 (-21%) | 623.4 (71%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
