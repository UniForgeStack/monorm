# Internal benchmark, 2026-10-30T20:25:15Z

- Commit: `6332db55acf01a3c01c81079e6cc57dd245ac0bb`. Backend: sqlite. Rounds: 45.
- Load average (1 minute): 1.19 at the start, 1.35 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Intel(R) Xeon(R) Processor @ 2.10GHz, Linux x86_64, runtime 3.13.17, 2026-10-30T20:24:26Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 412.1 | 252.3 | 227.9 | 47.1 | 9.7 |
| filter_order_50 | 764.8 | 563.8 | 510.0 | 162.1 | 95.8 |
| read_1000 | 2,242.2 | 1,976.2 | 2,010.3 | 1,350.6 | 1,406.3 |
| filtered_count | 320.4 | 264.5 | 253.7 | 83.5 | 48.8 |
| insert_one | 1,545.6 | n/a | 1,378.9 | 1,063.3 | 906.0 |
| prefetch_posts | 962.3 | 763.6 | 666.2 | 254.0 | 186.9 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 159.8 (39%) | 24.4 (6%) | 180.8 (44%) | 37.5 (9%) | 9.7 (2%) |
| filter_order_50 | 200.9 (26%) | 53.9 (7%) | 347.9 (45%) | 66.4 (9%) | 95.8 (13%) |
| read_1000 | 266.1 (12%) | -34.1 (-2%) | 659.7 (29%) | -55.7 (-2%) | 1,406.3 (63%) |
| filtered_count | 56.0 (17%) | 10.7 (3%) | 170.3 (53%) | 34.7 (11%) | 48.8 (15%) |
| insert_one | 166.7 (11%) | – | 315.7 (20%) | 157.2 (10%) | 906.0 (59%) |
| prefetch_posts | 198.7 (21%) | 97.3 (10%) | 412.2 (43%) | 67.2 (7%) | 186.9 (19%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Intel(R) Xeon(R) Processor @ 2.10GHz, linux x64, runtime 22.22.0, 2026-10-30T20:24:52Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 151.4 | 126.8 | 122.5 | 47.1 | 9.2 |
| filter_order_50 | 415.1 | 319.4 | 300.4 | 162.1 | 128.2 |
| read_1000 | 2,267.8 | 2,193.1 | 2,155.3 | 1,350.6 | 1,808.9 |
| filtered_count | 158.5 | 144.1 | 145.8 | 83.5 | 42.4 |
| insert_one | 1,233.3 | n/a | 1,183.0 | 1,063.3 | 888.9 |
| prefetch_posts | 667.7 | 584.3 | 540.0 | 254.0 | 210.2 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 24.6 (16%) | 4.3 (3%) | 75.3 (50%) | 37.9 (25%) | 9.2 (6%) |
| filter_order_50 | 95.7 (23%) | 19.0 (5%) | 138.3 (33%) | 33.9 (8%) | 128.2 (31%) |
| read_1000 | 74.7 (3%) | 37.8 (2%) | 804.7 (35%) | -458.3 (-20%) | 1,808.9 (80%) |
| filtered_count | 14.4 (9%) | -1.7 (-1%) | 62.4 (39%) | 41.0 (26%) | 42.4 (27%) |
| insert_one | 50.3 (4%) | – | 119.7 (10%) | 174.4 (14%) | 888.9 (72%) |
| prefetch_posts | 83.4 (12%) | 44.3 (7%) | 286.0 (43%) | 43.9 (7%) | 210.2 (31%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: engine; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
