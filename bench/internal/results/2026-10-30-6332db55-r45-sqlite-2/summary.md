# Internal benchmark, 2026-10-30T20:27:03Z

- Commit: `6332db55acf01a3c01c81079e6cc57dd245ac0bb`. Backend: sqlite. Rounds: 45.
- Load average (1 minute): 1.18 at the start, 1.29 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Intel(R) Xeon(R) Processor @ 2.10GHz, Linux x86_64, runtime 3.13.17, 2026-10-30T20:26:15Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 317.1 | 226.6 | 210.1 | 49.1 | 10.0 |
| filter_order_50 | 661.4 | 459.6 | 424.5 | 165.8 | 92.6 |
| read_1000 | 2,213.7 | 1,968.8 | 1,896.8 | 1,346.3 | 1,390.5 |
| filtered_count | 266.4 | 237.8 | 223.9 | 87.7 | 48.7 |
| insert_one | 1,397.0 | n/a | 1,329.1 | 999.7 | 832.8 |
| prefetch_posts | 886.8 | 667.9 | 635.5 | 237.7 | 185.6 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 90.5 (29%) | 16.5 (5%) | 161.0 (51%) | 39.1 (12%) | 10.0 (3%) |
| filter_order_50 | 201.8 (31%) | 35.1 (5%) | 258.7 (39%) | 73.2 (11%) | 92.6 (14%) |
| read_1000 | 244.9 (11%) | 72.0 (3%) | 550.5 (25%) | -44.2 (-2%) | 1,390.5 (63%) |
| filtered_count | 28.6 (11%) | 14.0 (5%) | 136.2 (51%) | 39.0 (15%) | 48.7 (18%) |
| insert_one | 67.9 (5%) | – | 329.4 (24%) | 166.9 (12%) | 832.8 (60%) |
| prefetch_posts | 218.9 (25%) | 32.4 (4%) | 397.8 (45%) | 52.1 (6%) | 185.6 (21%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Intel(R) Xeon(R) Processor @ 2.10GHz, linux x64, runtime 22.22.0, 2026-10-30T20:26:39Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 153.1 | 132.6 | 126.5 | 49.1 | 9.4 |
| filter_order_50 | 419.8 | 320.7 | 340.7 | 165.8 | 128.1 |
| read_1000 | 2,279.7 | 2,256.4 | 2,259.8 | 1,346.3 | 1,886.1 |
| filtered_count | 177.0 | 157.3 | 151.8 | 87.7 | 42.8 |
| insert_one | 1,249.4 | n/a | 1,207.5 | 999.7 | 852.2 |
| prefetch_posts | 730.1 | 652.5 | 602.1 | 237.7 | 218.1 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 20.5 (13%) | 6.2 (4%) | 77.3 (50%) | 39.8 (26%) | 9.4 (6%) |
| filter_order_50 | 99.1 (24%) | -20.0 (-5%) | 174.9 (42%) | 37.6 (9%) | 128.1 (31%) |
| read_1000 | 23.3 (1%) | -3.4 (-0%) | 913.5 (40%) | -539.8 (-24%) | 1,886.1 (83%) |
| filtered_count | 19.7 (11%) | 5.5 (3%) | 64.1 (36%) | 44.8 (25%) | 42.8 (24%) |
| insert_one | 41.9 (3%) | – | 207.8 (17%) | 147.5 (12%) | 852.2 (68%) |
| prefetch_posts | 77.5 (11%) | 50.5 (7%) | 364.4 (50%) | 19.7 (3%) | 218.1 (30%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
