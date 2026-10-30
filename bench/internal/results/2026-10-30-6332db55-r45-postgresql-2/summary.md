# Internal benchmark, 2026-10-30T20:26:15Z

- Commit: `6332db55acf01a3c01c81079e6cc57dd245ac0bb`. Backend: postgresql. Rounds: 45.
- Load average (1 minute): 1.35 at the start, 1.18 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Intel(R) Xeon(R) Processor @ 2.10GHz, Linux x86_64, runtime 3.13.17, 2026-10-30T20:25:16Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 528.7 | 325.9 | 314.5 | 117.0 | 135.9 |
| filter_order_50 | 833.0 | 602.8 | 567.1 | 303.5 | 305.4 |
| read_1000 | 2,374.2 | 2,120.9 | 2,143.1 | 1,364.2 | 2,104.2 |
| filtered_count | 548.3 | 453.2 | 442.7 | 419.8 | 219.6 |
| insert_one | 768.7 | n/a | 732.8 | 302.7 | 440.6 |
| prefetch_posts | 1,098.3 | 895.5 | 789.5 | 395.0 | 533.0 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 202.8 (38%) | 11.4 (2%) | 197.5 (37%) | -18.9 (-4%) | 135.9 (26%) |
| filter_order_50 | 230.2 (28%) | 35.7 (4%) | 263.6 (32%) | -1.9 (-0%) | 305.4 (37%) |
| read_1000 | 253.3 (11%) | -22.1 (-1%) | 778.9 (33%) | -740.1 (-31%) | 2,104.2 (89%) |
| filtered_count | 95.1 (17%) | 10.6 (2%) | 22.9 (4%) | 200.2 (37%) | 219.6 (40%) |
| insert_one | 35.9 (5%) | – | 430.1 (56%) | -137.9 (-18%) | 440.6 (57%) |
| prefetch_posts | 202.8 (18%) | 106.0 (10%) | 394.5 (36%) | -138.0 (-13%) | 533.0 (49%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: builder; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: engine; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Intel(R) Xeon(R) Processor @ 2.10GHz, linux x64, runtime 22.22.0, 2026-10-30T20:25:45Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 241.3 | 191.4 | 177.3 | 117.0 | 155.9 |
| filter_order_50 | 694.1 | 584.7 | 575.2 | 303.5 | 415.0 |
| read_1000 | 2,996.1 | 2,774.2 | 2,838.8 | 1,364.2 | 2,911.5 |
| filtered_count | 370.8 | 327.2 | 352.7 | 419.8 | 288.1 |
| insert_one | 665.9 | n/a | 531.0 | 302.7 | 480.2 |
| prefetch_posts | 936.7 | 774.9 | 734.6 | 395.0 | 598.7 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 49.9 (21%) | 14.1 (6%) | 60.3 (25%) | -38.9 (-16%) | 155.9 (65%) |
| filter_order_50 | 109.4 (16%) | 9.5 (1%) | 271.7 (39%) | -111.5 (-16%) | 415.0 (60%) |
| read_1000 | 221.8 (7%) | -64.6 (-2%) | 1,474.7 (49%) | -1,547.3 (-52%) | 2,911.5 (97%) |
| filtered_count | 43.6 (12%) | -25.5 (-7%) | -67.1 (-18%) | 131.8 (36%) | 288.1 (78%) |
| insert_one | 134.9 (20%) | – | 228.3 (34%) | -177.5 (-27%) | 480.2 (72%) |
| prefetch_posts | 161.8 (17%) | 40.4 (4%) | 339.5 (36%) | -203.6 (-22%) | 598.7 (64%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: engine; insert_one: hop + instances; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
