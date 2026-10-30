"""``select()``: queries returning chosen columns and aggregates instead of instances.

::

    rows = await (
        Post.objects.filter(Post.published)
        .select(Post.author_id, func.count().label("posts"), func.sum(Post.views).label("views"))
        .group_by(Post.author_id)
        .having(func.count() > 2)
    )
    rows[0].posts; rows[0][1]; author_id, posts, views = rows[0]

    await Post.objects.select(func.max(Post.views)).scalar()           # int | None
    await Post.objects.filter(...).select(Post.id).scalars()            # list[int]
    await Post.objects.select(Post, func.count(Post.comments))          # rows of (Post, int)

Rows are :class:`Row`: tuples whose items can also be read by name. Columns are named
after their field, ``label()``-ed expressions after the label, functions after the
function and a selected model after the model (``post``).
"""

from __future__ import annotations

import asyncio
import json
from collections.abc import AsyncIterator, Awaitable, Generator, Mapping
from operator import itemgetter
from typing import TYPE_CHECKING, Any, Generic, TypeVar, TypeVarTuple, Unpack, cast, overload

from . import debug, pagination
from .inspection import QueryInspection, _inspect
from ._cache import cached
from .errors import DoesNotExist, MultipleObjectsReturned, QueryError
from .nested import NestedRef
from .expr import (
    ColumnRef,
    Condition,
    ConditionLike,
    Expression,
    Func,
    IRContext,
    Labeled,
    Ordering,
    Param,
    ScalarSubquery,
    SlotParams,
    _AGGREGATES,
    _Ctes,
    and_,
    resolve_null_rule,
)
from .pagination import Key, Page
from .protection import allowed_writes
from .query import OneRowQuery, PreparedOneRowQuery, _Compiled

if TYPE_CHECKING:
    from collections.abc import Callable

    from typing_extensions import Self

    from .cte import Cte
    from .db import Database
    from .model import Model
    from .query import QuerySet

T = TypeVar("T")
T_co = TypeVar("T_co", covariant=True)
Ts = TypeVarTuple("Ts")

__all__ = ["PreparedSelect", "Row", "Select"]


# -- rows --------------------------------------------------------------------------------


class Row(tuple[Unpack[Ts]]):
    """A result row: a tuple (``row[0]``, ``a, b = row``) whose items can also be read
    by name (``row.total``; typed ``Any``). ``row._asdict()`` gives a dict."""

    __slots__ = ()
    _fields: tuple[str, ...] = ()

    if TYPE_CHECKING:

        def __getattr__(self, name: str) -> Any: ...

    def _values(self) -> tuple[Any, ...]:
        """The items as a plain tuple."""
        return tuple(cast("tuple[Any, ...]", self))

    def _asdict(self) -> dict[str, Any]:
        return dict(zip(self._fields, self._values()))

    def __repr__(self) -> str:
        return "Row(" + ", ".join(f"{n}={v!r}" for n, v in zip(self._fields, self._values())) + ")"

    def __reduce__(self) -> tuple[Any, ...]:
        return (_make_row, (self._fields, self._values()))


_row_classes: dict[tuple[str, ...], type] = {}


def row_class(names: tuple[str, ...]) -> type:
    """The ``Row`` subclass for these column names (created once, then cached)."""
    cls = _row_classes.get(names)
    if cls is None:
        ns: dict[str, Any] = {"__slots__": (), "_fields": names}
        ns.update({n: property(itemgetter(i)) for i, n in enumerate(names)})
        cls = _row_classes[names] = type("Row", (Row,), ns)
    return cls


def _make_row(names: tuple[str, ...], values: tuple[Any, ...]) -> Any:
    return tuple.__new__(row_class(names), values)


def _column_name(item: Any, i: int) -> str:
    if isinstance(item, Labeled):
        return item._name
    if isinstance(item, NestedRef):
        return item._select_name()
    if isinstance(item, ColumnRef):
        return item._field.name
    if isinstance(item, Func):
        return item._name
    from .cte import CteColumn

    if isinstance(item, CteColumn):
        return item._name
    if isinstance(item, type):
        return item.__name__.lower()
    return f"_{i}"


# -- the query ---------------------------------------------------------------------------


def _with_path_rules(item: Any) -> Any:
    """A boolean value (``cond.label("x")``) with the path null rules of its condition applied; it never raises."""
    if isinstance(item, Labeled) and isinstance(item._inner, Condition):
        inner = resolve_null_rule(item._inner, strict=False)
        return item if inner is item._inner else Labeled(inner, item._name)
    return item


class Select(Generic[Unpack[Ts]]):
    """A query returning rows of chosen columns. Built by ``QuerySet.select(...)``;
    every method returns a new ``Select``, and nothing runs until it is awaited. Awaiting
    the same ``Select`` again gives the same rows without querying again."""

    __slots__ = ("_qs", "_items", "_names", "_group", "_having", "_distinct", "_distinct_on", "_label", "_result")

    def __init__(self, qs: QuerySet[Any], items: tuple[Any, ...]) -> None:
        if not items:
            raise TypeError("select() needs at least one column")
        if qs._related or qs._prefetch:
            raise QueryError("select() can't be combined with load() of relations")
        for item in items:
            if isinstance(item, type):
                if item is not qs.model:
                    raise TypeError(f"select() takes {qs.model.__name__} itself or expressions, not {item.__name__}")
            elif not isinstance(item, Expression):
                raise TypeError(f"select() takes columns and expressions, got {item!r}")
        items = tuple(_with_path_rules(item) for item in items)
        names = tuple(_column_name(item, i) for i, item in enumerate(items))
        dupes = sorted({n for n in names if names.count(n) > 1})
        if dupes:
            raise ValueError(f"select() has several columns named {', '.join(dupes)}; name them with .label()")
        self._qs = qs
        self._items = items
        self._names = names
        self._group: tuple[Expression[Any], ...] = ()
        self._having: tuple[Any, ...] = ()
        self._distinct = False
        self._distinct_on: tuple[ColumnRef[Any], ...] = ()
        self._label: str | None = None
        self._result: asyncio.Task[list[Any]] | None = None

    def _clone(self, **changes: Any) -> Self:
        new = object.__new__(type(self))
        for k in self.__slots__:
            setattr(new, k, changes.get(k, getattr(self, k)))
        new._result = None
        return new

    # -- building (the WHERE / ORDER BY / LIMIT part goes to the query set) ---------------

    def require(self) -> RequiredSelect[Unpack[Ts]]:
        """Reject queries returning no rows, while allowing NULL column values."""
        return cast("RequiredSelect[Unpack[Ts]]", self._clone(_qs=self._qs.require()))

    def all(self) -> Self:
        """Read the rows afresh, without using this query's result cache."""
        return self._clone()

    def filter(self, *conditions: ConditionLike) -> Self:
        return self._clone(_qs=self._qs.filter(*conditions))

    def exclude(self, *conditions: ConditionLike) -> Self:
        return self._clone(_qs=self._qs.exclude(*conditions))

    def order_by(self, *items: Expression[Any] | Ordering) -> Self:
        return self._clone(_qs=self._qs.order_by(*items))

    def limit(self, n: int | None) -> Self:
        return self._clone(_qs=self._qs.limit(n))

    def offset(self, n: int | None) -> Self:
        return self._clone(_qs=self._qs.offset(n))

    @overload
    def __getitem__(self, s: int) -> OneRowQuery[Row[Unpack[Ts]] | None]: ...
    @overload
    def __getitem__(self, s: slice) -> Self: ...
    def __getitem__(self, s: int | slice) -> OneRowQuery[Row[Unpack[Ts]] | None] | Self:
        """Slice rows, or lazily read the row at an index (an int or :func:`monorm.param`);
        ``[-1]`` is ``last()``. A grouped, aggregate or distinct select needs ``order_by()`` to read by index."""
        if isinstance(s, slice):
            return self._clone(_qs=self._qs[s])
        if isinstance(s, Param):
            return self._at(s)
        if not isinstance(s, int):
            raise TypeError("select indices must be integers or slices")
        if s == -1:
            return self.last()
        if s < 0:
            raise ValueError("only -1 or non-negative indices are supported")
        return self._at(s)

    def _edge_order(self, what: str) -> tuple[Ordering, ...]:
        """The order that decides which row is first or last: ``order_by()``, else (for a select
        without groups or distinct) the model's default order."""
        if self._qs._order:
            return self._qs._order
        if self._group or self._distinct or self._aggregates():
            raise TypeError(f"{what} needs order_by() on a grouped, aggregate or distinct select")
        return self._qs._default_order()

    def _aggregates(self) -> bool:
        """Whether an item of the select list is an aggregate call: without ``group_by()`` it folds all rows."""
        for item in self._items:
            item = item._inner if isinstance(item, Labeled) else item
            if isinstance(item, Func) and item._rel is None and item._name in _AGGREGATES:
                return True
        return False

    def _at(self, n: int | Param) -> OneRowQuery[Row[Unpack[Ts]] | None]:
        qs = self._qs
        qs._no_cuts("[n]")
        order = self._edge_order("[n]")
        one = qs._clone(_order=order, _offset=n if isinstance(n, Param) else n or None, _limit=1, _cuts=("slice",))
        return OneSelectRow(self._clone(_qs=one))

    def join(self, cte: Cte, on: ConditionLike, *, outer: bool = False) -> Self:
        """``JOIN <cte> ON <on>`` (``LEFT JOIN`` with ``outer``), as :meth:`QuerySet.join`:
        the CTE's columns (``cte.c.<name>``) work in the select list, ``group_by()``,
        ``having()`` and ``order_by()``."""
        return self._clone(_qs=self._qs.join(cte, on, outer=outer))

    def from_(self, cte: Cte) -> Self:
        """Read the rows from ``cte`` instead of the model's table, as :meth:`QuerySet.from_`."""
        return self._clone(_qs=self._qs.from_(cte))

    def using(self, db: Database | None) -> Self:
        return self._clone(_qs=self._qs.using(db))

    def group_by(self, *exprs: Expression[Any] | type[Model]) -> Self:
        """``GROUP BY``: aggregates in the select list then summarize each group. The
        model itself (``group_by(Post)``) groups by its primary key."""
        out = []
        for e in exprs:
            if isinstance(e, type):
                if e is not self._qs.model:
                    raise TypeError(f"group_by() takes {self._qs.model.__name__} itself or expressions")
                out.extend(e._meta.key_refs())
            elif isinstance(e, Expression):
                out.append(e)
            else:
                raise TypeError(f"group_by() takes columns and expressions, got {e!r}")
        return self._clone(_group=(*self._group, *out))

    def having(self, *conditions: ConditionLike) -> Self:
        """Keep groups matching all ``conditions``: ``having(func.count() > 2)``."""
        if not conditions:
            return self._clone()
        condition = and_(*conditions)
        condition = resolve_null_rule(condition)
        return self._clone(_having=(*self._having, condition))

    def distinct(self, *on: ColumnRef[Any]) -> Self:
        """``SELECT DISTINCT``; with columns, Postgres' ``DISTINCT ON (...)``: the first
        row (by ``order_by``) of each distinct value."""
        return self._clone(_distinct=True, _distinct_on=on)

    def label(self, name: str) -> Select[Unpack[Ts]]:
        """For a select of a relation query set: ``User.objects.load(User.posts.objects.select(...).label("rows"))``
        stores the rows (a list of ``Row``) in ``user.rows``."""
        self._qs._bound("label")
        if not isinstance(name, str) or not name.isidentifier() or name.startswith("_"):
            raise ValueError(f"label {name!r} must be an identifier not starting with '_'")
        return self._clone(_label=name)

    def _relation_set(self) -> QuerySet[Any]:
        """The labeled relation query set that ``load()`` takes in place of this select."""
        if self._qs._via is None:
            raise TypeError("load() takes a select of a relation query set such as User.posts.objects.select(...)")
        if self._label is None:
            raise TypeError("a select() in load() needs .label(name)")
        if any(isinstance(item, NestedRef) for item in self._items):
            raise TypeError("a select() in load() can't select a nested column")
        return self._qs._clone(_label=self._label, _rows=self)

    # -- IR -------------------------------------------------------------------------------

    def _build(
        self,
        params: list[Any],
        outer: IRContext | None = None,
        ctes: _Ctes | None = None,
        op: str = "select",
        *,
        relation: bool = False,
    ) -> tuple[dict[str, Any], IRContext]:
        if relation:
            ir, ctx = self._qs._query_ir(op, params, outer, ctes, prefetch=False)
        else:
            self._qs._standalone()
            ir, ctx = self._qs._query_ir(op, params, outer, ctes)
        ir.pop("annotations", None)
        ir["columns"] = [
            {"t": "model"} if isinstance(item, type) else {"t": "expr", "expr": item._ir(ctx), "name": name}
            for item, name in zip(self._items, self._names)
        ]
        if self._group:
            ir["group_by"] = [g._ir(ctx) for g in self._group]
        if self._having:
            ir["having"] = [h._ir(ctx) for h in self._having]
        if self._distinct_on:
            ir["distinct_on"] = [c._ir(ctx) for c in self._distinct_on]
        elif self._distinct:
            ir["distinct"] = True
        ctx.add_windows(ir)
        return ir, ctx

    def _ir(self, params: list[Any], op: str = "select") -> dict[str, Any]:
        ir, ctx = self._build(params, op=op)
        return ctx.finish(ir)

    def _subquery_ir(self, ctx: IRContext, what: str) -> dict[str, Any]:
        if what != "exists()" and (len(self._items) != 1 or isinstance(self._items[0], type)):
            raise QueryError(f"{what} takes a query that selects exactly one column")
        if self._qs._lock is not None:
            raise QueryError("a subquery can't lock rows")
        return self._build(ctx.params, outer=ctx)[0]

    def _cte_ir(self, params: list[Any], ctes: _Ctes) -> dict[str, Any]:
        return self._build(params, ctes=ctes)[0]

    def as_scalar(self: Select[T]) -> ScalarSubquery[T]:
        """This one-column query as a value in another query: ``(SELECT ...)``. It must
        return at most one row (slice it with ``[:1]``); no row is ``NULL``. Correlate it
        with :func:`monorm.outer`::

            latest = (Post.objects.filter(Post.author_id == outer(User.id))
                      .order_by(Post.created_at.desc()).select(Post.title)[:1].as_scalar())
            await User.objects.select(User, latest.label("latest"))
        """
        self._one_column("as_scalar")
        return ScalarSubquery(self)

    def cte(
        self,
        name: str,
        *,
        recursive: Callable[[Cte], Any] | None = None,
        distinct: bool = False,
        materialized: bool | None = None,
    ) -> Cte:
        """This query as a CTE (``WITH <name> AS (...)``) whose columns are the selected
        items' names (``cte.c.<name>``; a selected model gives its fields). See
        :mod:`monorm.cte`."""
        from .cte import Cte

        return Cte(name, self, recursive=recursive, distinct=distinct, materialized=materialized)

    def inspect(self) -> QueryInspection:
        """Inspect the generated SELECT structure without executing it."""
        params: list[Any] = []
        return _inspect(self._qs, self._ir(params), params)

    def sql(self) -> str:
        """The SELECT this runs, with parameters inlined (for debugging)."""
        params: list[Any] = []
        from .db import _with_scope

        ir = self._ir(params)
        sql: str = self._qs._native().sql(*_with_scope(json.dumps(ir), params))
        return sql

    async def explain(self, analyze: bool = False) -> str:
        """The database's plan for this query's SELECT; see :meth:`QuerySet.explain`."""
        params: list[Any] = []
        return await self._qs._explain_ir(self._ir(params), params, analyze)

    def prepare(self) -> PreparedSelect[Unpack[Ts]]:
        """This query compiled once, to run many times with :func:`monorm.param` values::

            by_author = Post.objects.filter(Post.author_id == monorm.param("author")).select(Post.id).prepare()
            rows = await by_author(author=3)
        """
        self._qs._standalone()
        return PreparedSelect(self)

    # -- execution ------------------------------------------------------------------------

    async def _fetch(self) -> list[Any]:
        qs = self._qs
        params: list[Any] = []
        ir = self._ir(params)
        qs._check_lock()
        rows: list[Any] = await qs._run(ir, params, row_class(self._names))
        return self._decoded_rows(rows)

    def _decoded_rows(self, rows: list[Any]) -> list[Any]:
        nested = [(i, item) for i, item in enumerate(self._items) if isinstance(item, NestedRef)]
        return [self._decoded(row, nested) for row in rows] if nested else rows

    def _finish(self, rows: list[Any]) -> list[Any]:
        """The rows of a read: decoded, and non-empty when the select is required."""
        if self._qs._required and not rows:
            raise DoesNotExist("the query returned no row")
        return self._decoded_rows(rows)

    async def _rows(self) -> list[Any]:
        rows = await self._fetch()
        if self._qs._required and not rows:
            raise DoesNotExist("the query returned no row")
        return rows

    @staticmethod
    def _decoded(row: Any, nested: list[tuple[int, NestedRef]]) -> Any:
        values = list(row)
        for i, item in nested:
            values[i] = item._decode(values[i])
        return tuple.__new__(type(row), values)

    def __await__(self) -> Generator[Any, None, list[Row[Unpack[Ts]]]]:
        return cached(self, self._rows).__await__()

    async def __aiter__(self) -> AsyncIterator[Row[Unpack[Ts]]]:
        for row in await self:
            yield row

    def first(self) -> OneRowQuery[Row[Unpack[Ts]] | None]:
        """Lazy first row (by ``order_by``; unordered otherwise), or ``None``."""
        self._qs._no_cuts("first()")
        return OneSelectRow(self[:1])

    def last(self) -> OneRowQuery[Row[Unpack[Ts]] | None]:
        """Lazy last row by ``order_by()`` reversed, or ``None``. A grouped, aggregate or distinct select needs ``order_by()``."""
        qs = self._qs
        qs._no_cuts("last()")
        order = self._edge_order("last()")
        if self._distinct_on and len(order) > len(self._distinct_on):
            raise TypeError("last() can't reverse an order that goes past the distinct(on) columns: it would change the kept row")
        order = tuple(o.reversed() for o in order)
        return OneSelectRow(self._clone(_qs=qs._clone(_order=order, _limit=1, _cuts=("slice",))))

    def get(self, *conditions: ConditionLike) -> OneRowQuery[Row[Unpack[Ts]] | None]:
        """Lazy single matching row, or None; multiple matches always raise.

        ``require().get()`` also raises ``DoesNotExist`` when absent."""
        self._qs._no_cuts("get()")
        return OneSelectRow(self.filter(*conditions).limit(2), unique=True)

    async def scalar(self: Select[T]) -> T | None:
        """The single column of the first row, or ``None`` without rows:
        ``await Post.objects.select(func.max(Post.views)).scalar()``."""
        self._one_column("scalar")
        rows = await self[:1]._rows()
        return rows[0][0] if rows else None

    async def scalars(self: Select[T]) -> list[T]:
        """The single column of every row: ``await qs.select(Post.id).scalars()``."""
        self._one_column("scalars")
        return [r[0] for r in await self._rows()]

    def _one_column(self, what: str) -> None:
        if len(self._items) != 1:
            raise QueryError(f"{what}() needs a query selecting one column, this one selects {len(self._items)}")

    async def count(self) -> int:
        """The number of rows this select returns: ``SELECT count(*) FROM (<select>)``, so
        groups, ``having()``, ``distinct()`` and slices decide the rows counted."""
        params: list[Any] = []
        n: int = await self._qs._run(self._ir(params, "count"), params)
        return n

    async def exists(self) -> bool:
        """Whether the select returns a row: ``SELECT EXISTS (<select>)``."""
        params: list[Any] = []
        b: bool = await self._qs._run(self._ir(params, "exists"), params)
        return b

    # -- paging ---------------------------------------------------------------------------

    def _page_keys(self) -> tuple[list[Key], list[Ordering]]:
        """The keyset columns of a page and the full order of its read. The order is unique
        by its group keys or ``distinct(on)`` columns, else by the primary key."""
        qs = self._qs
        model = qs.model
        if self._distinct and not self._distinct_on:
            raise QueryError("paginate() can't be used with distinct(); use distinct(on) with the columns that make a row unique")
        if self._distinct_on:
            on = self._distinct_on
            order = qs._order or tuple(Ordering(c, desc=False) for c in on)
            lead = order[: len(on)]
            if len(lead) < len(on) or any(
                not isinstance(o.expr, ColumnRef) or o.expr._path or o.expr._field is not c._field
                for o, c in zip(lead, on)
            ):
                raise QueryError("paginate() needs order_by() to start with the distinct(on) columns")
            return pagination.keyset(model, tuple(lead), add_key=False), list(order)
        if self._group:
            for g in self._group:
                if not isinstance(g, ColumnRef) or g._path or g._root is not model:
                    raise QueryError(f"paginate() groups by columns of {model.__name__} itself, not by {g!r}")
            named = {o.expr._field.name for o in qs._order if isinstance(o.expr, ColumnRef) and not o.expr._path}
            extra = tuple(
                Ordering(g, desc=False) for g in self._group if isinstance(g, ColumnRef) and g._field.name not in named
            )
            keys = pagination.keyset(model, (*qs._order, *extra), add_key=False)
            return keys, [o for _, o in keys]
        keys = pagination.keyset(model, qs._default_order())
        return keys, [o for _, o in keys]

    def _key_getters(self, keys: list[Key]) -> list[Callable[[Any], Any]]:
        """For each key column, how to read its value from a row: the column must be in the select list."""
        model = self._qs.model
        getters: list[Callable[[Any], Any]] = []
        for f, o in keys:
            for i, item in enumerate(self._items):
                inner = item._inner if isinstance(item, Labeled) else item
                if inner is model:
                    getters.append(lambda row, i=i, name=f.name: row[i]._field_value(name))  # type: ignore[misc]
                elif isinstance(inner, ColumnRef) and not inner._path and inner._root is model and inner._field is f:
                    getters.append(itemgetter(i))
                else:
                    continue
                break
            else:
                raise QueryError(f"paginate() reads the cursor from the rows: select {o.expr!r} too")
        return getters

    async def paginate(
        self, *, first: int | None = None, after: str | None = None, last: int | None = None, before: str | None = None
    ) -> Page[Row[Unpack[Ts]]]:
        """A page of rows by keyset, as :meth:`QuerySet.paginate`. The order must be unique:
        ``order_by()`` plus the ``group_by()`` columns (added when missing) or the
        ``distinct(on)`` columns, else the primary key. Every order column is a column
        of the model itself and is in the select list; ``distinct()`` without columns and
        aggregate order terms are rejected. ``last=`` can't read back over ``distinct(on)`` with
        order terms after the ``on`` columns."""
        size, forward = pagination.check_paging(first, after, last, before)
        qs = self._qs
        if qs._limit is not None or qs._offset is not None:
            raise QueryError("paginate() can't be used on a sliced select")
        keys, order = self._page_keys()
        if not forward and len(order) > len(keys):
            raise QueryError("paginate(last=) over distinct(on) needs an order of the distinct(on) columns only")
        getters = self._key_getters(keys)
        fp = pagination.fingerprint(qs.model, keys)
        page_order = [o if forward else o.reversed() for o in order]
        cursor = after if forward else before
        read = self
        if cursor is not None:
            values = pagination.decode_cursor(qs.model, cursor, fp, keys)
            read = read.filter(pagination.after(qs.model, page_order[: len(keys)], values))
        rows = await read.order_by(*page_order)[: size + 1]._fetch()
        more = len(rows) > size
        rows = rows[:size] if forward else rows[:size][::-1]

        def cursor_of(row: Any) -> str:
            return pagination.encode_cursor(fp, keys, [get(row) for get in getters])

        return Page(
            rows,
            has_next=more if forward else cursor is not None,
            has_previous=cursor is not None if forward else more,
            next_cursor=cursor_of(rows[-1]) if rows else None,
            previous_cursor=cursor_of(rows[0]) if rows else None,
        )

    async def batches(self, size: int = 1000) -> AsyncIterator[list[Row[Unpack[Ts]]]]:
        """The rows in lists of ``size``, read page by page with :meth:`paginate` (same
        order rules), so memory stays flat::

            async for batch in Post.objects.select(Post.id, Post.title).batches(500):
                ...
        """
        if size < 1:
            raise ValueError("batch size must be at least 1")
        cursor: str | None = None
        seen: set[str] = set()
        while True:
            # No `yield` inside: the loop scope must not reach the consumer's queries.
            with debug.internal_loop(seen):
                page = await self.paginate(first=size, after=cursor)
            if page.items:
                yield page.items
            if not page.has_next:
                return
            cursor = page.next_cursor

    async def iterate(self, batch_size: int = 1000) -> AsyncIterator[Row[Unpack[Ts]]]:
        """Every row, fetched ``batch_size`` at a time (see :meth:`batches`)."""
        async for batch in self.batches(batch_size):
            for row in batch:
                yield row

    def __repr__(self) -> str:
        parts = [f"select({', '.join(self._names)})"]
        if self._group:
            parts.append(f"group_by{self._group!r}")
        if self._having:
            parts.append(f"having{self._having!r}")
        return f"<Select {self._qs!r} {'.'.join(parts)}>"


class OneSelectRow(OneRowQuery[T_co]):
    """A lazy one-row read of a select (``first()``, ``last()``, ``get()``, ``[n]``).
    Each await runs afresh."""

    def label(self, name: str) -> QuerySet[Any]:
        raise TypeError("the row of a select can't be loaded per parent")

    def _result(self, rows: list[Any]) -> T_co:
        if self._unique and len(rows) > 1:
            raise MultipleObjectsReturned("the query returned more than one row")
        return cast(T_co, rows[0] if rows else None)

    async def _read(self) -> T_co:
        return self._result(await self._qs._rows())

    def prepare(self) -> PreparedOneRowQuery[T_co]:
        return PreparedOneRowQuery(self, self._qs.prepare())


class PreparedSelect(Generic[Unpack[Ts]]):
    """A select compiled by :meth:`Select.prepare`. Await a call to get the rows
    (``await q(author=3)``), or use :meth:`first`, :meth:`get`, :meth:`scalar`,
    :meth:`scalars`, :meth:`count` and :meth:`exists`; each takes the
    :func:`monorm.param` values as keywords."""

    __slots__ = ("_select", "_compiled", "_names")

    def __init__(self, select: Select[Unpack[Ts]]) -> None:
        self._select = select
        self._compiled: dict[str, _Compiled] = {}
        self._names = self._statement("select").names

    def _statement(self, kind: str) -> _Compiled:
        c = self._compiled.get(kind)
        if c is None:
            sel: Select[Unpack[Ts]] = self._select
            op = "select"
            if kind == "get":
                sel._qs._no_cuts("get()")
                sel = sel.limit(2)
            elif kind in ("first", "scalar"):
                if kind == "scalar":
                    sel._one_column("scalar")
                sel._qs._no_cuts("first()")
                sel = sel[:1]
            elif kind in ("count", "exists"):
                op = kind
            params = SlotParams()
            c = self._compiled[kind] = _Compiled(sel._ir(params, op), params)
        return c

    def _start(self, kind: str, values: Mapping[str, Any]) -> Awaitable[Any]:
        from .db import _with_scope, resolve

        c = self._statement(kind)
        params = c.bind(values, self._names)
        qs = self._select._qs
        if qs._lock is not None:
            qs._check_lock()
        db = resolve(qs._db)
        op, params = _with_scope(c.json, params)
        row_cls = None if kind in ("count", "exists") else row_class(self._select._names)
        engine, tx, allowed = db._reader(), db._tx(), allowed_writes()
        return db._call(lambda t: engine.run(op, params, tx, row_cls, qs._db, allowed, t))

    def require(self) -> RequiredPreparedSelect[Unpack[Ts]]:
        """Require at least one row from each prepared read."""
        return cast("RequiredPreparedSelect[Unpack[Ts]]", PreparedSelect(self._select.require()))

    async def all(self, **values: Any) -> list[Row[Unpack[Ts]]]:
        rows: list[Row[Unpack[Ts]]] = self._select._finish(await self._start("select", values))
        return rows

    def __call__(self, **values: Any) -> Awaitable[list[Row[Unpack[Ts]]]]:
        """The rows, like awaiting the select."""
        return self.all(**values)

    async def get(self, **values: Any) -> Row[Unpack[Ts]] | None:
        rows = self._select._finish(await self._start("get", values))
        if len(rows) > 1:
            raise MultipleObjectsReturned("the query returned more than one row")
        return cast("Row[Unpack[Ts]] | None", rows[0] if rows else None)

    async def first(self, **values: Any) -> Row[Unpack[Ts]] | None:
        rows = self._select._finish(await self._start("first", values))
        return cast("Row[Unpack[Ts]] | None", rows[0] if rows else None)

    async def scalar(self: PreparedSelect[T], **values: Any) -> T | None:
        """The single column of the first row, or ``None`` without rows."""
        rows = self._select._finish(await self._start("scalar", values))
        return cast("T | None", rows[0][0] if rows else None)

    async def scalars(self: PreparedSelect[T], **values: Any) -> list[T]:
        """The single column of every row."""
        self._select._one_column("scalars")
        return [r[0] for r in self._select._finish(await self._start("select", values))]

    async def count(self, **values: Any) -> int:
        n: int = await self._start("count", values)
        return n

    async def exists(self, **values: Any) -> bool:
        b: bool = await self._start("exists", values)
        return b

    def inspect(self, **values: Any) -> QueryInspection:
        """Inspect the prepared SELECT for these bound parameter values."""
        from .db import _with_scope
        from .inspection import _native as inspection_native

        c = self._statement("select")
        return QueryInspection._from_json(
            inspection_native(self._select._qs).inspect(*_with_scope(c.json, c.bind(values, self._names)))
        )

    def sql(self, **values: Any) -> str:
        """The SELECT for these values, parameters inlined (for debugging)."""
        from .db import _with_scope

        c = self._statement("select")
        sql: str = self._select._qs._native().sql(*_with_scope(c.json, c.bind(values, self._names)))
        return sql

    @property
    def params(self) -> frozenset[str]:
        """The names of the values each call takes."""
        return self._names

    def __repr__(self) -> str:
        return f"<PreparedSelect {self._select!r}>"


if TYPE_CHECKING:

    class RequiredSelect(Select[Unpack[Ts]]):
        """The type of a selection whose row reads require a result."""

        def get(self, *conditions: ConditionLike) -> OneRowQuery[Row[Unpack[Ts]]]: ...
        def first(self) -> OneRowQuery[Row[Unpack[Ts]]]: ...
        def last(self) -> OneRowQuery[Row[Unpack[Ts]]]: ...
        @overload
        def __getitem__(self, s: int) -> OneRowQuery[Row[Unpack[Ts]]]: ...
        @overload
        def __getitem__(self, s: slice) -> Self: ...
        def __getitem__(self, s: int | slice) -> OneRowQuery[Row[Unpack[Ts]]] | Self: ...
        def prepare(self) -> RequiredPreparedSelect[Unpack[Ts]]: ...

    class RequiredPreparedSelect(PreparedSelect[Unpack[Ts]]):
        async def get(self, **values: Any) -> Row[Unpack[Ts]]: ...
        async def first(self, **values: Any) -> Row[Unpack[Ts]]: ...
