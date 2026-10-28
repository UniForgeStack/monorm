"""Small, immutable planner results for query structure assertions."""

from __future__ import annotations

import json
from dataclasses import dataclass, fields
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .query import QuerySet


@dataclass(frozen=True, kw_only=True)
class StatementInspection:
    """Structure of one planned statement.

    A subquery has a shape: the way the outer query uses its result.
    ``scalar_*`` is a value.
    ``set_*`` is ``IN``, ``EXISTS``, ``ANY`` or ``ALL``.
    ``derived_*`` is a ``FROM`` or ``JOIN`` item (``LATERAL`` included).
    Each shape counts ``_correlated`` and ``_uncorrelated`` subqueries.
    A CTE counts in ``cte_count``.
    """

    has_batching: bool = False
    joins_count: int = 0
    scalar_correlated: int = 0
    scalar_uncorrelated: int = 0
    set_correlated: int = 0
    set_uncorrelated: int = 0
    derived_correlated: int = 0
    derived_uncorrelated: int = 0
    cte_count: int = 0

    def __repr__(self) -> str:
        shown = ", ".join(f"{f.name}={v!r}" for f in fields(self) if (v := getattr(self, f.name)))
        return f"StatementInspection({shown})"


@dataclass(frozen=True)
class QueryInspection:
    """Planned statement templates, including prefetch templates.

    Execution may skip templates for empty parents or repeat them for key batches.
    Use ``Database.on_query()`` to count statements actually sent. Correlation is a
    dependency on enclosing columns, not a physical execution strategy; use
    ``explain()`` to see the database's strategy.
    """

    statement_count: int
    has_batching: bool
    statements: tuple[StatementInspection, ...]

    @classmethod
    def _from_json(cls, payload: str) -> QueryInspection:
        data = json.loads(payload)
        return cls(
            data["statement_count"],
            data["has_batching"],
            tuple(
                StatementInspection(**{f.name: s[f.name] for f in fields(StatementInspection)})
                for s in data["statements"]
            ),
        )


def _native(qs: QuerySet[Any]) -> Any:
    # Use the same planner capabilities as execution when a matching connection
    # exists. Standalone inspection still works without a connection.
    from .db import get_database
    from .errors import NotConnected

    db = qs._db
    if db is None:
        try:
            default = get_database()
        except NotConnected:
            pass
        else:
            try:
                registered = default._registry.get(qs.model._meta.name)
            except LookupError:
                pass
            else:
                if registered is qs.model:
                    db = default
    return db._engine if db is not None else qs._native()


def _inspect(qs: QuerySet[Any], ir: dict[str, Any] | None, params: list[Any]) -> QueryInspection:
    from .db import _with_scope

    if ir is None:
        return QueryInspection(0, False, ())
    return QueryInspection._from_json(_native(qs).inspect(*_with_scope(json.dumps(ir), params)))
