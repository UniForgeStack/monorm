"""Runs the host functions of the admin service on the application's event loop.

The Rust service calls :func:`submit` once for each host call. The function runs inside
``Database.bind_request``, so it sees the transaction, the scope and the tenant of the request.
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import json
from collections.abc import Awaitable, Callable
from typing import Any

HostFunction = Callable[[Any, Any], Awaitable[Any]]


async def run(
    fn: HostFunction, database: Any, tx: Any, payload: str, scope: str, tenant: str | None
) -> str:
    args = json.loads(payload)
    tenant_id = None if tenant is None else json.loads(tenant)
    with database.bind_request(tx, scope=json.loads(scope), tenant=tenant_id):
        return json.dumps(await fn(database, args))


def submit(
    loop: asyncio.AbstractEventLoop,
    fn: HostFunction,
    database: Any,
    tx: Any,
    payload: str,
    scope: str,
    tenant: str | None,
) -> concurrent.futures.Future[str]:
    """Schedules the call on ``loop`` from the Rust thread. Cancelling the future cancels the task."""
    return asyncio.run_coroutine_threadsafe(run(fn, database, tx, payload, scope, tenant), loop)
