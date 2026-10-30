# Internal benchmark, 2026-10-30T19:57:09Z

- Commit: `35cde71df68f86385578ee908dfd7d63a20b5c5d`. Backend: sqlite. Rounds: 15.
- Load average (1 minute): 1.08 at the start, 1.07 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Intel(R) Xeon(R) Processor @ 2.10GHz, Linux x86_64, runtime 3.13.17, 2026-10-30T19:56:51Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 383.8 | 241.9 | 223.4 | 51.3 | 9.2 |
| filter_order_50 | 785.6 | 542.5 | 490.5 | 162.7 | 95.4 |
| read_1000 | 2,254.1 | 1,929.0 | 2,041.4 | 1,360.6 | 1,408.6 |
| filtered_count | 347.7 | 307.3 | 281.7 | 84.6 | 50.9 |
| insert_one | 1,787.9 | n/a | 1,578.3 | 1,072.3 | 1,073.5 |
| prefetch_posts | 997.4 | 776.1 | 685.0 | 248.6 | 185.7 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 141.9 (37%) | 18.5 (5%) | 172.1 (45%) | 42.1 (11%) | 9.2 (2%) |
| filter_order_50 | 243.1 (31%) | 52.0 (7%) | 327.7 (42%) | 67.3 (9%) | 95.4 (12%) |
| read_1000 | 325.1 (14%) | -112.5 (-5%) | 680.8 (30%) | -48.0 (-2%) | 1,408.6 (62%) |
| filtered_count | 40.4 (12%) | 25.7 (7%) | 197.1 (57%) | 33.7 (10%) | 50.9 (15%) |
| insert_one | 209.6 (12%) | – | 506.0 (28%) | -1.2 (-0%) | 1,073.5 (60%) |
| prefetch_posts | 221.3 (22%) | 91.1 (9%) | 436.5 (44%) | 62.9 (6%) | 185.7 (19%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Intel(R) Xeon(R) Processor @ 2.10GHz, linux x64, runtime 22.22.0, 2026-10-30T19:57:01Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 191.0 | 160.3 | 144.3 | 51.3 | 10.1 |
| filter_order_50 | 506.0 | 476.4 | 383.2 | 162.7 | 130.1 |
| read_1000 | 2,331.5 | 2,281.4 | 2,281.9 | 1,360.6 | 2,186.5 |
| filtered_count | 180.8 | 154.9 | 159.0 | 84.6 | 43.2 |
| insert_one | 1,427.9 | n/a | 1,322.6 | 1,072.3 | 862.2 |
| prefetch_posts | 794.7 | 690.1 | 626.2 | 248.6 | 226.7 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 30.7 (16%) | 16.0 (8%) | 93.0 (49%) | 41.2 (22%) | 10.1 (5%) |
| filter_order_50 | 29.5 (6%) | 93.2 (18%) | 220.5 (44%) | 32.6 (6%) | 130.1 (26%) |
| read_1000 | 50.1 (2%) | -0.5 (-0%) | 921.3 (40%) | -826.0 (-35%) | 2,186.5 (94%) |
| filtered_count | 25.9 (14%) | -4.1 (-2%) | 74.4 (41%) | 41.4 (23%) | 43.2 (24%) |
| insert_one | 105.3 (7%) | – | 250.2 (18%) | 210.2 (15%) | 862.2 (60%) |
| prefetch_posts | 104.6 (13%) | 63.8 (8%) | 377.7 (48%) | 21.9 (3%) | 226.7 (29%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
