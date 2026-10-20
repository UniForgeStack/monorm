"""Shared pieces of the Python benchmark: parameters per case, database reset, state checksum, plain results."""

from __future__ import annotations

import asyncio
import datetime as dt
import inspect
import os
import pathlib
from collections.abc import Callable, Iterable
from typing import Any

import psycopg

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent
DEFAULT_URL = "postgres://postgres:postgres@localhost:55432/bench_nfx1"
POOL_SIZE = 10
TABLES = ("users", "categories", "tags", "posts", "comments", "post_tags")

CASES = (
    "read-pk",
    "read-50",
    "read-1000",
    "projection-1000",
    "filter",
    "filter-ne-null",
    "count",
    "aggregate",
    "join-to-one",
    "prefetch-to-many",
    "m2m",
    "insert-1",
    "bulk-insert-100",
    "bulk-insert-1000",
    "update-1",
    "bulk-update-100",
    "upsert-100",
    "delete-100",
    "tx-5",
    "concurrent",
)
CONCURRENT_TASKS = 50
CONCURRENT_OPS_PER_TASK = 4

# Fixed values, so contenders write the same rows (the state checksum includes every column).
EPOCH = dt.datetime(2027, 1, 1, tzinfo=dt.UTC)


def ts(i: int) -> dt.datetime:
    return EPOCH + dt.timedelta(seconds=i)


def database_url() -> str:
    return os.environ.get("BENCH_DATABASE_URL", DEFAULT_URL)


def pk_for(i: int) -> int:
    return (i * 7919) % 10_000 + 1


def comment_rows(i: int, n: int) -> list[dict[str, Any]]:
    """`n` new comments for iteration `i`: the same rows for every contender."""
    return [
        {"post_id": (i * n + k) % 10_000 + 1, "author_id": None if k % 5 == 0 else k % 1000 + 1, "body": f"new {i}-{k}", "created_at": ts(k)}
        for k in range(n)
    ]


def upsert_rows(i: int) -> list[dict[str, Any]]:
    """100 users: 50 with an existing email (conflict) and 50 with a new one."""
    rows = []
    for k in range(50):
        e = (i * 50 + k) % 1000 + 1
        rows.append({"email": f"user{e}@example.com", "name": f"Renamed {i}-{k}", "created_at": ts(e)})
    for k in range(50):
        rows.append({"email": f"new{i}-{k}@example.com", "name": f"New {i}-{k}", "created_at": ts(k)})
    return rows


def update_range(i: int) -> tuple[int, int]:
    lo = (i * 100) % 9_900 + 1
    return lo, lo + 99


def connect() -> psycopg.Connection[Any]:
    return psycopg.connect(database_url(), autocommit=True)


def reset(conn: psycopg.Connection[Any]) -> None:
    """Back to the seed: empty every table, restart the identities, reload seed.sql."""
    conn.execute(f"TRUNCATE {', '.join(TABLES)} RESTART IDENTITY CASCADE")
    conn.execute((ROOT / "seed.sql").read_text())


def state(conn: psycopg.Connection[Any]) -> dict[str, str]:
    """One checksum per table: equal checksums mean equal rows."""
    out = {}
    for t in TABLES:
        row = conn.execute(f"SELECT count(*), md5(coalesce(string_agg(x::text, '|' ORDER BY id), '')) FROM {t} AS x").fetchone()
        assert row is not None
        out[t] = f"{row[0]}:{row[1]}"
    return out


def plain(value: Any) -> Any:
    if isinstance(value, dt.datetime):
        return value.astimezone(dt.UTC).isoformat()
    return value


def objs(rows: Iterable[Any], fields: tuple[str, ...]) -> list[list[Any]]:
    return [[plain(getattr(o, f)) for f in fields] for o in rows]


async def call(fn: Callable[..., Any], *args: Any) -> Any:
    """Call a sync or async function and await the result when there is one."""
    out = fn(*args)
    return await out if inspect.isawaitable(out) else out


def p99(values: list[float]) -> float:
    s = sorted(values)
    return s[min(len(s) - 1, int(len(s) * 0.99))]


async def gather(coros: list[Any]) -> list[Any]:
    return list(await asyncio.gather(*coros))
