# Machine

## This VM (all numbers in `monorm-results.md` and every "(this VM)" and MonOrm entry)

| | |
|---|---|
| CPU | Intel(R) Xeon(R) Processor @ 2.80GHz (KVM guest) |
| Cores | 4 vCPUs (1 socket, 4 cores, 1 thread per core) |
| RAM | 15 GiB, no swap |
| Disk | 256 GB virtio disk (`/dev/vda`), overlay root |
| OS | Ubuntu 24.04.5 LTS |
| Kernel | Linux 6.18.44-fc-v185 (x86_64) |
| Python | 3.13.17 (venv) |
| Node.js | 22.22.0 |
| PostgreSQL server | 14.24 (Docker image `postgres:14`, as in the upstream `Makefile`) |
| PostgreSQL client | psql 16.15 |
| Docker | 29.9.2 |
| MonOrm | `monorm` 0.1.1 (PyPI), `@ufstack/monorm` 0.1.1 (npm, alias `monorm`) |
| References | Django 4.2.30, SQLAlchemy 2.0.55, asyncpg 0.32.0, TypeORM 0.3.16, Prisma 5.6.0, pg 8.11.0 |
| Dataset | `make new-dataset` with upstream defaults (100000 people, 100000 users, 500000 reviews, 25000 movies), synth 0.6.9 |

The database server, the benchmark clients and Docker ran on this VM; nothing else ran during the timed runs.
The load average was under 1 before each run (see `raw/runs.log`).

No network delay was simulated.
The kernel has no `netem` qdisc (`CONFIG_NET_SCH_NETEM` is off, no modules), so `tc qdisc add dev br-webapp-bench root netem delay 1ms` from the upstream `DEVELOP.rst` failed.
`--net-latency 1` was passed as the procedure says, but `bench.py` only records this value in the JSON; it adds no delay.

## Upstream run (all other entries of `py.json`, `js.json`, `py.html`, `js.html`)

The `platform` block of the upstream `docs/py.json` (2023):

```json
{"cpu": "AMD Ryzen Threadripper 3970X 32-Core Processor", "arch": "x86_64", "system": "Linux 6.3.3+", "distribution": "Gentoo 2.13"}
```

AMD Ryzen Threadripper 3970X, 32 cores, run in May 2023.
