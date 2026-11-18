"""Runs the host functions of the admin service on the application's event loop.

The Rust service calls :func:`submit` once for each host call. The function runs inside
``Database.bind_request``, so it sees the transaction and the scope of the request.
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import json
from collections.abc import Awaitable, Callable
from typing import Any

HostFunction = Callable[[Any, Any], Awaitable[Any]]


async def run(fn: HostFunction, database: Any, tx: Any, payload: str, scope: str) -> str:
    args = json.loads(payload)
    if tx is None:  # a service owns its transactions: Rust binds no request transaction for it
        return json.dumps(await fn(database, args))
    with database.bind_request(tx, scope=json.loads(scope)):
        return json.dumps(await fn(database, args))


def submit(
    loop: asyncio.AbstractEventLoop,
    fn: HostFunction,
    database: Any,
    tx: Any,
    payload: str,
    scope: str,
) -> concurrent.futures.Future[str]:
    """Schedules the call on ``loop`` from the Rust thread. Cancelling the future cancels the task."""
    return asyncio.run_coroutine_threadsafe(run(fn, database, tx, payload, scope), loop)
