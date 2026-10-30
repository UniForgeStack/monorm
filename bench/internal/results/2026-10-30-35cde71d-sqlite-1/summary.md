# Internal benchmark, 2026-10-30T19:56:29Z

- Commit: `35cde71df68f86385578ee908dfd7d63a20b5c5d`. Backend: sqlite. Rounds: 15.
- Load average (1 minute): 1.01 at the start, 1.09 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Intel(R) Xeon(R) Processor @ 2.10GHz, Linux x86_64, runtime 3.13.17, 2026-10-30T19:56:10Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 368.8 | 242.3 | 241.2 | 45.6 | 10.4 |
| filter_order_50 | 777.2 | 549.8 | 488.1 | 232.0 | 101.9 |
| read_1000 | 2,449.0 | 2,011.1 | 2,156.8 | 1,411.8 | 1,493.1 |
| filtered_count | 344.4 | 283.8 | 299.3 | 84.9 | 51.5 |
| insert_one | 1,845.7 | n/a | 1,800.8 | 1,092.0 | 1,157.9 |
| prefetch_posts | 1,000.3 | 773.7 | 763.6 | 303.0 | 194.1 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 126.5 (34%) | 1.1 (0%) | 195.6 (53%) | 35.3 (10%) | 10.4 (3%) |
| filter_order_50 | 227.4 (29%) | 61.7 (8%) | 256.1 (33%) | 130.1 (17%) | 101.9 (13%) |
| read_1000 | 437.9 (18%) | -145.7 (-6%) | 745.1 (30%) | -81.3 (-3%) | 1,493.1 (61%) |
| filtered_count | 60.5 (18%) | -15.5 (-4%) | 214.5 (62%) | 33.4 (10%) | 51.5 (15%) |
| insert_one | 44.9 (2%) | – | 708.8 (38%) | -65.8 (-4%) | 1,157.9 (63%) |
| prefetch_posts | 226.6 (23%) | 10.2 (1%) | 460.6 (46%) | 108.9 (11%) | 194.1 (19%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Intel(R) Xeon(R) Processor @ 2.10GHz, linux x64, runtime 22.22.0, 2026-10-30T19:56:20Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 212.1 | 160.6 | 157.0 | 45.6 | 10.7 |
| filter_order_50 | 534.7 | 454.1 | 384.9 | 232.0 | 138.1 |
| read_1000 | 2,369.6 | 2,387.5 | 2,241.5 | 1,411.8 | 1,993.6 |
| filtered_count | 174.3 | 157.9 | 155.9 | 84.9 | 43.2 |
| insert_one | 1,455.6 | n/a | 1,409.6 | 1,092.0 | 1,068.3 |
| prefetch_posts | 776.4 | 645.1 | 699.3 | 303.0 | 220.7 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 51.4 (24%) | 3.6 (2%) | 111.4 (53%) | 35.0 (16%) | 10.7 (5%) |
| filter_order_50 | 80.6 (15%) | 69.2 (13%) | 152.9 (29%) | 93.9 (18%) | 138.1 (26%) |
| read_1000 | -18.0 (-1%) | 146.0 (6%) | 829.8 (35%) | -581.8 (-25%) | 1,993.6 (84%) |
| filtered_count | 16.4 (9%) | 2.0 (1%) | 71.0 (41%) | 41.7 (24%) | 43.2 (25%) |
| insert_one | 46.0 (3%) | – | 317.6 (22%) | 23.7 (2%) | 1,068.3 (73%) |
| prefetch_posts | 131.3 (17%) | -54.2 (-7%) | 396.3 (51%) | 82.2 (11%) | 220.7 (28%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
