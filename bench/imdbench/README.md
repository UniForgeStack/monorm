# IMDBench

The three [IMDBench](https://github.com/geldata/imdbench) queries (`insert_movie`, `get_movie`, `get_user`), run with MonOrm in Python and Node against Django, SQLAlchemy, TypeORM, Prisma and the raw drivers, on one machine.

Latest results: [2026-10-30, `5e1379d`](results/2026-10-30-5e1379d/results.md) (MonOrm 0.1.1, measured).

| Language | MonOrm, requests/s | vs ORMs |
|---|---:|---|
| Python | 282.5 | 1.41× SQLAlchemy, 2.85× Django |
| Node | 383.0 | 1.65× Prisma, 2.18× TypeORM |

[`monorm.patch`](monorm.patch) adds the MonOrm contenders to the IMDBench repo, ready for an upstream PR.
How to run it: [RUNNING.md](RUNNING.md).
