"""Custom admin routes: the host functions of ``Admin.route`` and of the streams that a route returns.

Rust runs the session, the CSRF check and the permission check, then calls ``route.<METHOD>.<path>`` with the request.
A route that returns a stream gets an id. Rust then calls ``route.__next`` once per chunk (the client reads at its own pace)
and ``route.__close`` at the end, so no layer holds the whole body.
"""

from __future__ import annotations

import asyncio
import base64
import contextvars
import datetime
import decimal
import inspect
import itertools
import uuid
from collections.abc import AsyncIterable, AsyncIterator, Awaitable, Callable, Iterable, Iterator, Mapping
from dataclasses import dataclass, field
from typing import Any

from ._admin_write import Context, is_sync

NEXT = "route.__next"
CLOSE = "route.__close"
_COALESCE = 16 * 1024


@dataclass(frozen=True)
class RouteRequest:
    """The request that a route function gets: ``params`` are the ``{name}`` parts of the path, ``query`` maps a query
    name to a string (a list when the name repeats), and ``body`` is the JSON body (``None`` when empty or on a GET)."""

    method: str
    path: str
    params: Mapping[str, str] = field(default_factory=dict)
    query: Mapping[str, Any] = field(default_factory=dict)
    body: Any = None


@dataclass(frozen=True)
class Reply:
    """A JSON answer with its own status code (200 by default)."""

    json: Any = None
    status: int = 200


@dataclass(frozen=True)
class Stream:
    """A body that the admin sends chunk by chunk. ``chunks`` is an iterable or an async iterable of ``bytes`` or ``str``.
    ``filename`` makes the browser save it. A route function that fails after the first chunk cuts the response."""

    chunks: Iterable[bytes | str] | AsyncIterable[bytes | str]
    content_type: str = "application/octet-stream"
    filename: str | None = None
    status: int = 200


RouteFunction = Callable[[Context, RouteRequest], Any]


def _plain(value: Any) -> Any:
    if isinstance(value, (datetime.datetime, datetime.date, datetime.time)):
        return value.isoformat()
    if isinstance(value, (uuid.UUID, decimal.Decimal)):
        return str(value)
    if isinstance(value, bytes):
        return base64.b64encode(value).decode("ascii")
    raise TypeError(f"a route returned {type(value).__name__}, which is not JSON")


def _jsonable(value: Any) -> Any:
    import json

    return json.loads(json.dumps(value, default=_plain))


class _Open:
    """One open stream: the iterator and the way to pull the next piece from it."""

    def __init__(self, chunks: Any) -> None:
        self._async: AsyncIterator[bytes | str] | None = None
        self._sync: Iterator[bytes | str] | None = None
        if hasattr(chunks, "__aiter__"):
            self._async = chunks.__aiter__()
        else:
            self._sync = iter(chunks)

    async def next(self) -> bytes | None:
        """The next bytes, or ``None`` at the end. A sync iterator is read in a worker thread, and small chunks join up to 16 KiB."""
        if self._async is not None:
            try:
                return _bytes(await self._async.__anext__())
            except StopAsyncIteration:
                return None
        sync = self._sync
        assert sync is not None

        def pull() -> bytes | None:
            out = bytearray()
            for piece in sync:
                out += _bytes(piece)
                if len(out) >= _COALESCE:
                    break
            return bytes(out) if out else None

        return await asyncio.get_running_loop().run_in_executor(None, contextvars.copy_context().run, pull)

    async def close(self) -> None:
        if self._async is not None:
            aclose = getattr(self._async, "aclose", None)
            if aclose is not None:
                await aclose()
        else:
            close = getattr(self._sync, "close", None)
            if close is not None:
                close()


def _bytes(piece: Any) -> bytes:
    if isinstance(piece, str):
        return piece.encode()
    if isinstance(piece, (bytes, bytearray, memoryview)):
        return bytes(piece)
    raise TypeError(f"a stream yielded {type(piece).__name__}: yield bytes or str")


_ids = itertools.count(1)


class Streams:
    """The open streams of one admin, by id. Rust closes each one, also when the client leaves early."""

    def __init__(self) -> None:
        self._open: dict[str, _Open] = {}

    def open(self, chunks: Any) -> str:
        sid = str(next(_ids))
        self._open[sid] = _Open(chunks)
        return sid

    async def next(self, _database: Any, args: dict[str, Any]) -> dict[str, Any]:
        stream = self._open.get(args["stream"])
        if stream is None:
            return {"chunk": None}
        chunk = await stream.next()
        return {"chunk": None if chunk is None else base64.b64encode(chunk).decode("ascii")}

    async def close(self, _database: Any, args: dict[str, Any]) -> None:
        stream = self._open.pop(args["stream"], None)
        if stream is not None:
            await stream.close()


def route_function(fn: RouteFunction, streams: Streams) -> Callable[[Any, dict[str, Any]], Awaitable[Any]]:
    """The host function of one route: it builds the context and the request, runs ``fn`` and wraps the answer
    as ``{"json", "status"}`` or ``{"stream", "content_type", "filename", "status"}``."""
    sync = is_sync(fn)

    async def call(database: Any, args: dict[str, Any]) -> Any:
        ctx = Context(user=args.get("user"), request_id=args.get("request_id", ""), database=database, ip=args.get("ip"), user_agent=args.get("user_agent"))
        ctx._loop = asyncio.get_running_loop()
        ctx._by_admin = True
        request = RouteRequest(args["method"], args["path"], args.get("params") or {}, args.get("query") or {}, args.get("body"))
        if sync:
            scope = contextvars.copy_context()
            result = await asyncio.get_running_loop().run_in_executor(None, lambda: scope.run(fn, ctx, request))
        else:
            result = await fn(ctx, request)
        if inspect.isawaitable(result):
            result = await result
        if isinstance(result, Stream):
            out: dict[str, Any] = {"stream": streams.open(result.chunks), "content_type": result.content_type, "status": result.status, "audit_keys": ctx._audit_keys}
            if result.filename is not None:
                out["filename"] = result.filename
            return out
        if isinstance(result, Reply):
            return {"json": _jsonable(result.json), "status": result.status, "audit_keys": ctx._audit_keys}
        return {"json": _jsonable(result), "audit_keys": ctx._audit_keys}

    return call


def host(routes: Mapping[tuple[str, str], RouteFunction]) -> dict[str, Any]:
    """The host entries of a set of routes, keyed by ``(METHOD, path)``, with their stream functions."""
    streams = Streams()
    out: dict[str, Any] = {f"route.{method}.{path}": route_function(fn, streams) for (method, path), fn in routes.items()}
    out[NEXT] = streams.next
    out[CLOSE] = streams.close
    return out
