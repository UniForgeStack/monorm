# Internal benchmark, 2026-10-30T19:57:45Z

- Commit: `35cde71df68f86385578ee908dfd7d63a20b5c5d`. Backend: sqlite. Rounds: 15.
- Load average (1 minute): 1.06 at the start, 0.97 at the end (measured).
- Every number is measured: the median of the timed batches, in µs per operation.

## Python

Intel(R) Xeon(R) Processor @ 2.10GHz, Linux x86_64, runtime 3.13.17, 2026-10-30T19:57:28Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 333.0 | 228.9 | 214.4 | 45.5 | 9.2 |
| filter_order_50 | 757.5 | 501.4 | 460.1 | 160.7 | 96.8 |
| read_1000 | 2,336.0 | 2,003.0 | 1,966.3 | 1,296.4 | 1,493.6 |
| filtered_count | 319.9 | 274.3 | 255.5 | 82.5 | 50.5 |
| insert_one | 1,590.2 | n/a | 1,435.7 | 951.0 | 968.3 |
| prefetch_posts | 916.2 | 648.7 | 621.2 | 245.0 | 187.3 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 104.2 (31%) | 14.5 (4%) | 168.9 (51%) | 36.3 (11%) | 9.2 (3%) |
| filter_order_50 | 256.1 (34%) | 41.3 (5%) | 299.4 (40%) | 63.9 (8%) | 96.8 (13%) |
| read_1000 | 332.9 (14%) | 36.7 (2%) | 669.9 (29%) | -197.1 (-8%) | 1,493.6 (64%) |
| filtered_count | 45.7 (14%) | 18.8 (6%) | 173.0 (54%) | 32.0 (10%) | 50.5 (16%) |
| insert_one | 154.4 (10%) | – | 484.7 (30%) | -17.3 (-1%) | 968.3 (61%) |
| prefetch_posts | 267.5 (29%) | 27.4 (3%) | 376.3 (41%) | 57.6 (6%) | 187.3 (20%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

## TypeScript on Node

Intel(R) Xeon(R) Processor @ 2.10GHz, linux x64, runtime 22.22.0, 2026-10-30T19:57:37Z.

| case | unprepared | prepared | direct IR | engine only | driver only |
|---|---|---|---|---|---|
| pk_get | 184.3 | 150.4 | 141.9 | 45.5 | 10.0 |
| filter_order_50 | 515.8 | 427.8 | 383.6 | 160.7 | 130.8 |
| read_1000 | 2,202.6 | 2,169.0 | 2,179.5 | 1,296.4 | 1,922.1 |
| filtered_count | 181.8 | 166.9 | 162.2 | 82.5 | 44.2 |
| insert_one | 1,294.8 | n/a | 1,195.7 | 951.0 | 940.8 |
| prefetch_posts | 727.9 | 633.8 | 613.9 | 245.0 | 216.2 |

- prepared n/a: a write has no IR to prepare

Layers in µs per op, with the share of the unprepared call:

| case | builder | value binding | hop + instances | engine | driver |
|---|---|---|---|---|---|
| pk_get | 33.9 (18%) | 8.5 (5%) | 96.4 (52%) | 35.5 (19%) | 10.0 (5%) |
| filter_order_50 | 88.1 (17%) | 44.1 (9%) | 222.9 (43%) | 29.9 (6%) | 130.8 (25%) |
| read_1000 | 33.7 (2%) | -10.6 (-0%) | 883.1 (40%) | -625.7 (-28%) | 1,922.1 (87%) |
| filtered_count | 14.9 (8%) | 4.7 (3%) | 79.7 (44%) | 38.3 (21%) | 44.2 (24%) |
| insert_one | 99.1 (8%) | – | 244.7 (19%) | 10.2 (1%) | 940.8 (73%) |
| prefetch_posts | 94.1 (13%) | 19.9 (3%) | 368.9 (51%) | 28.7 (4%) | 216.2 (30%) |

Biggest monorm layer per case (the driver is the floor, not counted): pk_get: hop + instances; filter_order_50: hop + instances; read_1000: hop + instances; filtered_count: hop + instances; insert_one: hop + instances; prefetch_posts: hop + instances.

A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.
The prefetch case reads the user and the posts of the user in one engine call that runs two statements.
