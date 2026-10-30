# Internal benchmark, 2026-10-30T19:56:51Z

- Commit: `35cde71df68f86385578ee908dfd7d63a20b5c5d`. Backend: postgresql. Rounds: 15.
- Load average (1 minute): 1.09 at the start, 1.08 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Intel(R) Xeon(R) Processor @ 2.10GHz, Linux x86_64, runtime 3.13.17, 2026-10-30T19:56:29Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 540.1 | 319.3 | 310.0 | 132.9 | 138.9 |
| filter_order_50 | 827.2 | 662.7 | 520.7 | 278.1 | 311.6 |
| read_1000 | 2,532.4 | 2,400.6 | 2,314.3 | 1,481.9 | 2,128.8 |
| filtered_count | 621.9 | 519.4 | 478.6 | 229.0 | 224.5 |
| insert_one | 868.4 | n/a | 789.1 | 380.0 | 420.5 |
| prefetch_posts | 1,176.0 | 931.1 | 898.6 | 392.0 | 604.3 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 220.8 (41%) | 9.3 (2%) | 177.0 (33%) | -6.0 (-1%) | 138.9 (26%) |
| filter_order_50 | 164.5 (20%) | 142.0 (17%) | 242.6 (29%) | -33.5 (-4%) | 311.6 (38%) |
| read_1000 | 131.8 (5%) | 86.3 (3%) | 832.4 (33%) | -646.9 (-26%) | 2,128.8 (84%) |
| filtered_count | 102.4 (16%) | 40.8 (7%) | 249.6 (40%) | 4.5 (1%) | 224.5 (36%) |
| insert_one | 79.3 (9%) | – | 409.1 (47%) | -40.5 (-5%) | 420.5 (48%) |
| prefetch_posts | 244.9 (21%) | 32.5 (3%) | 506.6 (43%) | -212.3 (-18%) | 604.3 (51%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: builder; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Intel(R) Xeon(R) Processor @ 2.10GHz, linux x64, runtime 22.22.0, 2026-10-30T19:56:40Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 320.6 | 258.9 | 231.7 | 132.9 | 199.8 |
| filter_order_50 | 703.6 | 502.3 | 561.4 | 278.1 | 621.0 |
| read_1000 | 3,166.8 | 2,955.9 | 2,982.9 | 1,481.9 | 3,349.3 |
| filtered_count | 331.9 | 283.1 | 282.1 | 229.0 | 256.8 |
| insert_one | 739.0 | n/a | 585.1 | 380.0 | 594.1 |
| prefetch_posts | 1,028.1 | 935.3 | 838.0 | 392.0 | 780.3 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 61.7 (19%) | 27.2 (8%) | 98.8 (31%) | -66.9 (-21%) | 199.8 (62%) |
| filter_order_50 | 201.3 (29%) | -59.1 (-8%) | 283.3 (40%) | -342.9 (-49%) | 621.0 (88%) |
| read_1000 | 210.9 (7%) | -27.0 (-1%) | 1,501.0 (47%) | -1,867.4 (-59%) | 3,349.3 (106%) |
| filtered_count | 48.8 (15%) | 0.9 (0%) | 53.1 (16%) | -27.8 (-8%) | 256.8 (77%) |
| insert_one | 154.0 (21%) | – | 205.1 (28%) | -214.1 (-29%) | 594.1 (80%) |
| prefetch_posts | 92.7 (9%) | 97.4 (9%) | 446.0 (43%) | -388.3 (-38%) | 780.3 (76%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
