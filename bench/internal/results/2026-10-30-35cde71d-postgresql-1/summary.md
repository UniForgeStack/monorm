# Internal benchmark, 2026-10-30T19:56:09Z

- Commit: `35cde71df68f86385578ee908dfd7d63a20b5c5d`. Backend: postgresql. Rounds: 15.
- Load average (1 minute): 0.81 at the start, 1.01 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Intel(R) Xeon(R) Processor @ 2.10GHz, Linux x86_64, runtime 3.13.17, 2026-10-30T19:55:50Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 436.5 | 293.0 | 262.5 | 114.1 | 136.1 |
| filter_order_50 | 764.1 | 551.3 | 490.3 | 196.9 | 324.2 |
| read_1000 | 2,304.7 | 2,016.0 | 2,081.4 | 1,300.3 | 2,123.7 |
| filtered_count | 507.8 | 444.0 | 408.5 | 264.7 | 207.1 |
| insert_one | 802.2 | n/a | 837.6 | 403.6 | 509.1 |
| prefetch_posts | 1,054.8 | 794.5 | 813.7 | 483.7 | 595.6 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 143.6 (33%) | 30.5 (7%) | 148.4 (34%) | -22.1 (-5%) | 136.1 (31%) |
| filter_order_50 | 212.8 (28%) | 61.0 (8%) | 293.4 (38%) | -127.3 (-17%) | 324.2 (42%) |
| read_1000 | 288.7 (13%) | -65.4 (-3%) | 781.1 (34%) | -823.4 (-36%) | 2,123.7 (92%) |
| filtered_count | 63.8 (13%) | 35.4 (7%) | 143.9 (28%) | 57.5 (11%) | 207.1 (41%) |
| insert_one | -35.4 (-4%) | – | 434.0 (54%) | -105.4 (-13%) | 509.1 (63%) |
| prefetch_posts | 260.3 (25%) | -19.2 (-2%) | 330.0 (31%) | -111.9 (-11%) | 595.6 (56%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Intel(R) Xeon(R) Processor @ 2.10GHz, linux x64, runtime 22.22.0, 2026-10-30T19:56:00Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 277.0 | 237.6 | 222.2 | 114.1 | 186.8 |
| filter_order_50 | 641.0 | 465.9 | 443.1 | 196.9 | 420.6 |
| read_1000 | 3,001.6 | 2,991.0 | 2,807.2 | 1,300.3 | 3,085.5 |
| filtered_count | 322.2 | 290.0 | 280.4 | 264.7 | 293.0 |
| insert_one | 701.3 | n/a | 606.8 | 403.6 | 542.0 |
| prefetch_posts | 844.5 | 773.3 | 843.1 | 483.7 | 699.6 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 39.5 (14%) | 15.4 (6%) | 108.1 (39%) | -72.8 (-26%) | 186.8 (67%) |
| filter_order_50 | 175.0 (27%) | 22.9 (4%) | 246.2 (38%) | -223.7 (-35%) | 420.6 (66%) |
| read_1000 | 10.6 (0%) | 183.8 (6%) | 1,506.9 (50%) | -1,785.2 (-59%) | 3,085.5 (103%) |
| filtered_count | 32.2 (10%) | 9.6 (3%) | 15.7 (5%) | -28.4 (-9%) | 293.0 (91%) |
| insert_one | 94.6 (13%) | – | 203.2 (29%) | -138.4 (-20%) | 542.0 (77%) |
| prefetch_posts | 71.3 (8%) | -69.9 (-8%) | 359.5 (43%) | -216.0 (-26%) | 699.6 (83%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: builder; insert_one: hop + instances; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
