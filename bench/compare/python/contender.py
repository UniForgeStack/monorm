"""The contract between the runner and a library under test."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

POST = ("id", "author_id", "category_id", "title", "body", "views", "published", "created_at")
USER = ("id", "email", "name", "created_at")
TAG = ("id", "name")


@dataclass
class Case:
    """One benchmark case of one contender.

    `setup(i, conn)` is untimed and returns the argument of `op`. `op(arg)` is the timed work, written
    as the library's docs show; it may be sync or async. `norm(raw)` turns the result into plain data for the
    equality check (untimed). `build(arg)` and `sql(arg)` are the CPU-only split: make the query object, and
    make it and render its SQL. The decode step is not reachable from outside either library.
    """

    op: Callable[[Any], Any]
    norm: Callable[[Any], Any]
    setup: Callable[[int, Any], Any] = lambda i, conn: i
    build: Callable[[Any], Any] | None = None
    sql: Callable[[Any], str] | None = None
    # `concurrent` only: the op returns (result, per-op latencies in ms)
    concurrent: bool = False


class Contender:
    name: str
    cases: dict[str, Case]

    async def start(self) -> None: ...

    async def stop(self) -> None: ...

    def info(self) -> dict[str, Any]:
        return {}
