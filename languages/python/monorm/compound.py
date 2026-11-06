"""``union()`` and ``union_all()`` of ``select()`` queries: one statement, rows of the chosen columns.

::

    rows = await (
        Post.objects.select(Post.id.label("id"), Post.title.label("name"))
        .union(Page.objects.select(Page.id.label("id"), Page.title.label("name")))
        .order_by("-id")[:10]
    )

Each side keeps its own filters, order and slice. ``order_by()``, ``limit()``, ``offset()``
and slices of the compound apply to the whole result; ``order_by()`` takes output column names.
Both sides select the same names in the same order, with the same types (a column may be
nullable on one side only). ``union`` drops duplicate rows; ``union_all`` keeps them.
"""

from __future__ import annotations

import asyncio
import json
from collections.abc import AsyncIterator, Generator
from typing import TYPE_CHECKING, Any, Generic, NoReturn, TypeVar, TypeVarTuple, Unpack, cast, overload

from ._cache import cached
from .errors import DoesNotExist, QueryError
from .expr import Labeled
from .inspection import QueryInspection, _inspect
from .nested import NestedRef
from .query import OneRowQuery, _add_cut, _sync_iteration_error
from .select import OneSelectRow, Row, Select, row_class

if TYPE_CHECKING:
    from typing_extensions import Self

    from .query import QuerySet

T = TypeVar("T")
Ts = TypeVarTuple("Ts")
A1 = TypeVar("A1")
A2 = TypeVar("A2")
A3 = TypeVar("A3")
A4 = TypeVar("A4")
A5 = TypeVar("A5")
A6 = TypeVar("A6")
A7 = TypeVar("A7")
B1 = TypeVar("B1")
B2 = TypeVar("B2")
B3 = TypeVar("B3")
B4 = TypeVar("B4")
B5 = TypeVar("B5")
B6 = TypeVar("B6")
B7 = TypeVar("B7")

__all__ = ["CompoundSelect"]


def _operand(item: Any) -> None:
    """Reject a side that a set operation can't take; Rust checks the column types."""
    if isinstance(item, CompoundSelect):
        if item._required:
            raise QueryError("a set operation can't take a require()d query; call require() on the result")
        return
    if not isinstance(item, Select):
        raise TypeError(f"a set operation takes select(...) queries, got {item!r}")
    qs = item._qs
    qs._standalone()
    if qs._required:
        raise QueryError("a set operation can't take a require()d query; call require() on the result")
    if qs._lock is not None:
        raise QueryError("lock() can't be used in a set operation")
    if qs._from is not None or qs._joins:
        raise QueryError("a set operation can't use a CTE")
    for column in item._items:
        inner = column._inner if isinstance(column, Labeled) else column
        if isinstance(inner, type):
            raise QueryError("a set operation takes columns and expressions, not a model")
        if isinstance(inner, NestedRef):
            raise QueryError("a set operation can't return a nested column")


class CompoundSelect(Generic[Unpack[Ts]]):
    """``select(...).union(other)``: rows of two ``select()`` queries in one statement. Every
    method returns a new query; nothing runs until it is awaited, and awaiting the same
    query again gives the same rows."""

    __slots__ = ("_left", "_right", "_operator", "_names", "_order", "_limit", "_offset", "_cuts", "_required", "_result")

    def __init__(self, operator: str, left: Any, right: Any) -> None:
        _operand(left)
        _operand(right)
        if left._names != right._names:
            raise QueryError(
                f"{operator}() needs both queries to select the same columns in the same order, "
                f"got ({', '.join(left._names)}) and ({', '.join(right._names)}); name them with .label()"
            )
        self._left = left
        self._right = right
        self._operator = operator
        self._names: tuple[str, ...] = left._names
        self._order: tuple[str, ...] = ()
        self._limit: int | None = None
        self._offset: int | None = None
        self._cuts: tuple[str, ...] = ()
        self._required = False
        self._result: asyncio.Task[list[Any]] | None = None
        self._target(resolved=False)

    def _clone(self, **changes: Any) -> Self:
        new = object.__new__(type(self))
        for k in self.__slots__:
            setattr(new, k, changes.get(k, getattr(self, k)))
        new._result = None
        return new

    def _leaves(self) -> list[Select[Any]]:
        out: list[Select[Any]] = []
        for side in (self._left, self._right):
            out.extend(side._leaves() if isinstance(side, CompoundSelect) else [side])
        return out

    def _target(self, *, resolved: bool) -> QuerySet[Any]:
        """The query set that routes and runs the whole statement; every side must agree on registry and database."""
        from .db import resolve

        leaves = self._leaves()
        registry = leaves[0]._qs.model._meta.registry
        if any(leaf._qs.model._meta.registry is not registry for leaf in leaves):
            raise QueryError("the queries of a set operation must use the same model registry")
        dbs = [resolve(leaf._qs._db) if resolved else leaf._qs._db for leaf in leaves]
        known = [db for db in dbs if db is not None]
        if any(db is not known[0] for db in known):
            raise QueryError("the queries of a set operation must run on the same database; call using() on each query before union()")
        return next((leaf._qs for leaf in leaves if leaf._qs._db is not None), leaves[0]._qs)

    # -- building --------------------------------------------------------------------------

    @overload
    def union(self: CompoundSelect[A1], other: Select[B1] | CompoundSelect[B1]) -> CompoundSelect[A1 | B1]: ...
    @overload
    def union(self: CompoundSelect[A1, A2], other: Select[B1, B2] | CompoundSelect[B1, B2]) -> CompoundSelect[A1 | B1, A2 | B2]: ...
    @overload
    def union(self: CompoundSelect[A1, A2, A3], other: Select[B1, B2, B3] | CompoundSelect[B1, B2, B3]) -> CompoundSelect[A1 | B1, A2 | B2, A3 | B3]: ...
    @overload
    def union(self: CompoundSelect[A1, A2, A3, A4], other: Select[B1, B2, B3, B4] | CompoundSelect[B1, B2, B3, B4]) -> CompoundSelect[A1 | B1, A2 | B2, A3 | B3, A4 | B4]: ...
    @overload
    def union(self: CompoundSelect[A1, A2, A3, A4, A5], other: Select[B1, B2, B3, B4, B5] | CompoundSelect[B1, B2, B3, B4, B5]) -> CompoundSelect[A1 | B1, A2 | B2, A3 | B3, A4 | B4, A5 | B5]: ...
    @overload
    def union(self: CompoundSelect[A1, A2, A3, A4, A5, A6], other: Select[B1, B2, B3, B4, B5, B6] | CompoundSelect[B1, B2, B3, B4, B5, B6]) -> CompoundSelect[A1 | B1, A2 | B2, A3 | B3, A4 | B4, A5 | B5, A6 | B6]: ...
    @overload
    def union(
        self: CompoundSelect[A1, A2, A3, A4, A5, A6, A7, Unpack[tuple[Any, ...]]],
        other: Select[B1, B2, B3, B4, B5, B6, B7, Unpack[tuple[Any, ...]]] | CompoundSelect[B1, B2, B3, B4, B5, B6, B7, Unpack[tuple[Any, ...]]],
    ) -> CompoundSelect[A1 | B1, A2 | B2, A3 | B3, A4 | B4, A5 | B5, A6 | B6, A7 | B7, Unpack[tuple[Any, ...]]]: ...
    def union(self, other: Any) -> Any:
        """Rows of both sides without duplicates; the rows of a side keep their own filters, order and slice."""
        return CompoundSelect("union", self, other)

    @overload
    def union_all(self: CompoundSelect[A1], other: Select[B1] | CompoundSelect[B1]) -> CompoundSelect[A1 | B1]: ...
    @overload
    def union_all(self: CompoundSelect[A1, A2], other: Select[B1, B2] | CompoundSelect[B1, B2]) -> CompoundSelect[A1 | B1, A2 | B2]: ...
    @overload
    def union_all(self: CompoundSelect[A1, A2, A3], other: Select[B1, B2, B3] | CompoundSelect[B1, B2, B3]) -> CompoundSelect[A1 | B1, A2 | B2, A3 | B3]: ...
    @overload
    def union_all(self: CompoundSelect[A1, A2, A3, A4], other: Select[B1, B2, B3, B4] | CompoundSelect[B1, B2, B3, B4]) -> CompoundSelect[A1 | B1, A2 | B2, A3 | B3, A4 | B4]: ...
    @overload
    def union_all(self: CompoundSelect[A1, A2, A3, A4, A5], other: Select[B1, B2, B3, B4, B5] | CompoundSelect[B1, B2, B3, B4, B5]) -> CompoundSelect[A1 | B1, A2 | B2, A3 | B3, A4 | B4, A5 | B5]: ...
    @overload
    def union_all(self: CompoundSelect[A1, A2, A3, A4, A5, A6], other: Select[B1, B2, B3, B4, B5, B6] | CompoundSelect[B1, B2, B3, B4, B5, B6]) -> CompoundSelect[A1 | B1, A2 | B2, A3 | B3, A4 | B4, A5 | B5, A6 | B6]: ...
    @overload
    def union_all(
        self: CompoundSelect[A1, A2, A3, A4, A5, A6, A7, Unpack[tuple[Any, ...]]],
        other: Select[B1, B2, B3, B4, B5, B6, B7, Unpack[tuple[Any, ...]]] | CompoundSelect[B1, B2, B3, B4, B5, B6, B7, Unpack[tuple[Any, ...]]],
    ) -> CompoundSelect[A1 | B1, A2 | B2, A3 | B3, A4 | B4, A5 | B5, A6 | B6, A7 | B7, Unpack[tuple[Any, ...]]]: ...
    def union_all(self, other: Any) -> Any:
        """Rows of both sides, duplicates kept."""
        return CompoundSelect("union_all", self, other)

    def order_by(self, *names: str) -> Self:
        """Replace the ordering of the whole result. Names are output columns; ``"-name"`` is descending."""
        for name in names:
            if not isinstance(name, str) or not name.lstrip("-"):
                raise TypeError(f"a set operation orders by output column names such as 'id' or '-id', got {name!r}")
        return self._clone(_order=tuple(names))

    def limit(self, n: int | None) -> Self:
        """At most ``n`` rows of the whole result."""
        self._number(n, "limit")
        return self._clone(_limit=n, _cuts=_add_cut(self._cuts, "limit"))

    def offset(self, n: int | None) -> Self:
        """Skip ``n`` rows of the whole result."""
        self._number(n, "offset")
        return self._clone(_offset=n or None, _cuts=_add_cut(self._cuts, "offset"))

    @staticmethod
    def _number(n: Any, what: str) -> None:
        if n is not None and (isinstance(n, bool) or not isinstance(n, int) or n < 0):
            raise ValueError(f"{what}() takes a non-negative integer, got {n!r}")

    def __getitem__(self, s: slice) -> Self:
        """Slice the whole result; the same rules as a query set slice."""
        if not isinstance(s, slice):
            raise TypeError("a set operation takes slices; use first() to read one row")
        cuts = _add_cut(self._cuts, "slice")
        if s.step is not None or (s.start or 0) < 0 or (s.stop is not None and s.stop < 0):
            raise ValueError("only non-negative slices without a step are supported")
        start = s.start or 0
        return self._clone(_offset=start or None, _limit=None if s.stop is None else max(s.stop - start, 0), _cuts=cuts)

    def all(self) -> Self:
        """Read the rows afresh, without using this query's result cache."""
        return self._clone()

    def require(self) -> RequiredCompoundSelect[Unpack[Ts]]:
        """Reject a result without rows. It applies to the final result only."""
        return cast("RequiredCompoundSelect[Unpack[Ts]]", self._clone(_required=True))

    # -- IR ---------------------------------------------------------------------------------

    def _node(self, params: list[Any]) -> dict[str, Any]:
        node: dict[str, Any] = {
            "t": "set",
            "operator": self._operator,
            "left": _side(self._left, params),
            "right": _side(self._right, params),
        }
        if self._order:
            node["order"] = list(self._order)
        for key, value in (("limit", self._limit), ("offset", self._offset)):
            if value is not None:
                params.append(value)
                node[key] = {"t": "param", "i": len(params) - 1}
        return node

    def _ir(self, params: list[Any], terminal: str = "read") -> dict[str, Any]:
        return {"op": "compound", "terminal": terminal, "expr": self._node(params)}

    # -- inspection -------------------------------------------------------------------------

    def inspect(self) -> QueryInspection:
        """Inspect the generated statement structure without executing it."""
        params: list[Any] = []
        return _inspect(self._target(resolved=False), self._ir(params), params)

    def sql(self) -> str:
        """The statement this runs, with parameters inlined (for debugging)."""
        from .db import _with_scope

        params: list[Any] = []
        ir = self._ir(params)
        sql: str = self._target(resolved=False)._native().sql(*_with_scope(json.dumps(ir), params))
        return sql

    async def explain(self, analyze: bool = False) -> str:
        """The database's plan for this statement; see :meth:`QuerySet.explain`."""
        params: list[Any] = []
        return await self._target(resolved=True)._explain_ir(self._ir(params), params, analyze)

    # -- execution --------------------------------------------------------------------------

    async def _run(self, terminal: str) -> Any:
        qs = self._target(resolved=True)
        params: list[Any] = []
        ir = self._ir(params, terminal)
        return await qs._run(ir, params, row_class(self._names) if terminal == "read" else None)

    async def _rows(self) -> list[Any]:
        rows: list[Any] = await self._run("read")
        if self._required and not rows:
            raise DoesNotExist("the query returned no row")
        return rows

    def __await__(self) -> Generator[Any, None, list[Row[Unpack[Ts]]]]:
        return cached(self, self._rows).__await__()

    async def __aiter__(self) -> AsyncIterator[Row[Unpack[Ts]]]:
        for row in await self:
            yield row

    def __iter__(self) -> NoReturn:
        raise _sync_iteration_error()

    def __contains__(self, item: object) -> NoReturn:
        raise _sync_iteration_error()

    def _no_cuts(self, what: str) -> None:
        if self._cuts:
            raise QueryError(f"{what} can't be used on a query with a limit, offset or slice")

    def first(self) -> OneRowQuery[Row[Unpack[Ts]] | None]:
        """Lazy first row (by ``order_by``; unordered otherwise), or ``None``."""
        self._no_cuts("first()")
        return OneSelectRow(self[:1])

    def get(self) -> OneRowQuery[Row[Unpack[Ts]] | None]:
        """Lazy single row, or ``None``; more than one row raises."""
        self._no_cuts("get()")
        return OneSelectRow(self.limit(2), unique=True)

    async def scalar(self: CompoundSelect[T]) -> T | None:
        """The single column of the first row, or ``None`` without rows."""
        self._one_column("scalar")
        rows = await self[:1]._rows()
        return cast("T | None", rows[0][0] if rows else None)

    async def scalars(self: CompoundSelect[T]) -> list[T]:
        """The single column of every row."""
        self._one_column("scalars")
        return [r[0] for r in await self._rows()]

    def _one_column(self, what: str) -> None:
        if len(self._names) != 1:
            raise QueryError(f"{what}() needs a query selecting one column, this one selects {len(self._names)}")

    async def count(self) -> int:
        """The number of rows of the result: ``SELECT count(*) FROM (<union>)``."""
        n: int = await self._run("count")
        return n

    async def exists(self) -> bool:
        """Whether the result has a row."""
        b: bool = await self._run("exists")
        return b

    def __repr__(self) -> str:
        return f"<CompoundSelect {self._left!r} {self._operator} {self._right!r}>"


def _side(side: Select[Any] | CompoundSelect[Any], params: list[Any]) -> dict[str, Any]:
    if isinstance(side, CompoundSelect):
        return side._node(params)
    ir, ctx = side._build(params)
    if ctx.ctes.items:
        raise QueryError("a set operation can't use a CTE")
    ir.pop("op")
    ir["t"] = "leaf"
    return ir


if TYPE_CHECKING:

    class RequiredCompoundSelect(CompoundSelect[Unpack[Ts]]):
        """The type of a compound query whose row reads require a result."""

        def get(self) -> OneRowQuery[Row[Unpack[Ts]]]: ...
        def first(self) -> OneRowQuery[Row[Unpack[Ts]]]: ...
