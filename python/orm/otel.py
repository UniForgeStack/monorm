"""OpenTelemetry spans for the queries of a database: ``orm.otel.instrument(db)``.

Needs ``opentelemetry-api`` (``pip install 'orm[otel]'``); the ORM does not import it
anywhere else. Configure the SDK and its exporter as usual; the spans go to the tracer
provider of the process.
"""

from __future__ import annotations

import importlib
import re
from collections.abc import Callable
from typing import Any, Protocol

from .db import Database, QueryEvent

__all__ = ["Tracer", "instrument"]

_OPERATION = re.compile(r"[\s(]*(\w+)")
_MAX_TEXT = 4096


class Tracer(Protocol):
    """The part of ``opentelemetry.trace.Tracer`` the ORM uses."""

    def start_span(self, name: str, *, kind: Any = ..., attributes: Any = ..., start_time: int | None = ...) -> Any: ...


def instrument(db: Database, *, tracer: Tracer | None = None) -> Callable[[], None]:
    """Give each statement of ``db`` a client span; returns a function that stops it.

    The span starts and ends at the statement's own times and has the current span of
    the calling task as its parent. Its name is the SQL operation (``SELECT``), and it
    has ``db.system.name``, ``db.operation.name``, ``db.query.text`` (the SQL with
    placeholders for ORM-built SQL, as written for raw SQL; cut at 4096 characters then ``...``) and ``db.response.returned_rows``. A failed
    statement gets the error status and the database's message.
    """
    try:
        api = importlib.import_module("opentelemetry.trace")
    except ImportError as e:
        raise ImportError("orm.otel needs opentelemetry-api: pip install 'orm[otel]'") from e
    spans: Tracer = tracer if tracer is not None else api.get_tracer("orm")
    system = "sqlite" if db.url.startswith("sqlite:") else "postgresql"

    def hook(e: QueryEvent) -> None:
        found = _OPERATION.match(e.sql)
        operation = found.group(1).upper() if found else "SQL"
        start = int(e.start * 1e9)
        span = spans.start_span(
            operation,
            kind=api.SpanKind.CLIENT,
            attributes={
                "db.system.name": system,
                "db.operation.name": operation,
                "db.query.text": e.sql if len(e.sql) <= _MAX_TEXT else e.sql[:_MAX_TEXT] + "...",
                "db.response.returned_rows": e.rows,
            },
            start_time=start,
        )
        if e.error is not None:
            span.set_status(api.StatusCode.ERROR, e.error)
        span.end(end_time=start + int(e.duration * 1e9))

    return db.on_query(hook)
