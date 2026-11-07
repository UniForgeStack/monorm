"""Prepared writes: ``prepare()`` on an insert, update, delete or batch write.

The statement compiles once, and the engine holds its SQL text and parameter map. A call
binds the :func:`monorm.param` values, and a batch call converts its rows::

    add = Author.objects.insert(name=param("n"), bio=param("b")).prepare()
    author = await add(n="A", bio="x")

    add_many = Author.objects.insert_many(param("rows", fields=(Author.name, Author.bio))).prepare()
    count = await add_many(rows=[{"name": "A", "bio": "x"}, {"name": "B", "bio": "y"}])
"""

from __future__ import annotations

import json
from collections.abc import Awaitable, Callable, Mapping, Sequence
from operator import itemgetter
from typing import TYPE_CHECKING, Any, Generic, TypeVar

from .db import _scope, resolve
from .errors import QueryError
from .expr import Param, SlotParams, _Slot
from .inspection import QueryInspection, _native as _inspection_native
from .protection import allowed_writes
from .query import _Compiled
from .write import InsertMany, _Call

if TYPE_CHECKING:
    from .query import QuerySet

R_co = TypeVar("R_co", covariant=True)

__all__ = ["PreparedSet", "PreparedRows"]


async def _done(value: Any) -> Any:
    return value


async def _first(pending: Awaitable[Any], *, required: bool) -> Any:
    rows = await pending
    return rows[0] if rows or required else None


def _known(names: frozenset[str], values: Mapping[str, Any]) -> None:
    if missing := names - values.keys():
        raise TypeError(f"missing values for {', '.join(sorted(missing))}")
    if unknown := values.keys() - names:
        raise TypeError(f"the prepared write has no param {', '.join(sorted(unknown))}")


def _scope_values(scope: Mapping[str, Any] | None) -> dict[str, Any]:
    """The scope values of a call: ``scope=`` over the ``monorm.scope()`` context."""
    merged = _scope.get()
    return {**merged, **scope} if scope else merged


class _CompiledWrite(_Compiled):
    """The held statement of an update or a delete. ``None`` binds as NULL; the engine
    refuses it where the statement compares."""

    __slots__ = ()

    def bind(self, values: Mapping[str, Any], known: frozenset[str]) -> list[Any]:
        if missing := self.names - values.keys():
            raise TypeError(f"missing values for {', '.join(sorted(missing))}")
        if unknown := values.keys() - known:
            raise TypeError(f"the prepared write has no param {', '.join(sorted(unknown))}")
        params = self.params.copy()
        for i, name, transform in self.slots:
            v = values[name]
            params[i] = v if transform is None or v is None else transform(v)
        return params

    def native(self, engine: Any, scope: Mapping[str, Any]) -> Any:
        key = (engine, tuple(scope))
        write = self.natives.get(key)
        if write is None:
            op = self.json
            if scope:
                index = json.dumps({name: len(self.params) + i for i, name in enumerate(scope)})
                op = f'{op[:-1]},"scope":{index}}}'
            declared = [*self.declared, *([None] * len(scope))]
            write = self.natives[key] = engine.prepare_write(op, len(declared), declared)
        return write

    def start(
        self, qs: Any, known: frozenset[str], values: Mapping[str, Any], scope: Mapping[str, Any] | None, row_cls: Any = None
    ) -> Awaitable[Any]:
        params, merged = self.call_values(values, known, scope)
        db = resolve(qs._db)
        write = self.native(db._engine, merged)
        tx, allowed = db._tx(), allowed_writes()
        return db._call(lambda t: write.run(params, tx, row_cls, qs._db, allowed, t))


class PreparedSet(Generic[R_co]):
    """An update or a delete compiled by ``prepare()``. A call gives the row count, or with
    ``returning()`` the rows: ``await q(id=3, name="x")``. ``scope=`` sets scope values for the call."""

    __slots__ = ("_qs", "_compiled", "_names", "_empty")

    def __init__(self, qs: QuerySet[Any], ir: dict[str, Any] | None, params: list[Any]) -> None:
        self._qs = qs
        self._empty: Any = [] if ir is not None and ir.get("returning") else 0
        self._compiled: _CompiledWrite | None = None
        self._names: frozenset[str] = frozenset()
        if ir is not None:  # None: an update without values
            self._compiled = c = _CompiledWrite(ir, params)
            self._names = c.names
            c.validate(qs)

    def __call__(self, *, scope: Mapping[str, Any] | None = None, **values: Any) -> Awaitable[R_co]:
        if self._compiled is None:
            _known(self._names, values)
            return _done(self._empty)
        return self._compiled.start(self._qs, self._names, values, scope)

    def inspect(self, *, scope: Mapping[str, Any] | None = None, **values: Any) -> QueryInspection:
        if self._compiled is None:
            return QueryInspection(0, False, ())
        return self._compiled.inspect(self._qs, self._names, values, scope)

    def sql(self, *, scope: Mapping[str, Any] | None = None, **values: Any) -> str:
        if self._compiled is None:
            return ""
        return self._compiled.sql(self._qs, self._names, values, scope)

    def __repr__(self) -> str:
        return f"<PreparedSet {self._qs!r}>"


# -- rows -----------------------------------------------------------------------------------


class OneRow:
    """The row of a prepared ``insert()``: constants, ``DEFAULT``, and the cells a call fills."""

    __slots__ = ("template", "slots", "calls", "names")

    def __init__(self, template: list[Any]) -> None:
        self.template = template
        self.slots = [(i, c.name) for i, c in enumerate(template) if isinstance(c, Param)]
        self.calls = [(i, c.fn) for i, c in enumerate(template) if isinstance(c, _Call)]
        self.names = frozenset(name for _, name in self.slots)

    def rows(self, values: Mapping[str, Any]) -> list[Any]:
        row = list(self.template)
        for i, name in self.slots:
            row[i] = values[name]
        if self.calls:
            for i, fn in self.calls:
                row[i] = fn()
        return [row]


class ManyRows:
    """The rows of a prepared ``insert_many()`` or ``update_many()``: dicts with the
    ``given`` keys. ``fields`` is the statement's column order; ``calls`` are the callable
    defaults of the fields no row gives. The first ``keys`` fields are the primary key of an update."""

    __slots__ = ("name", "given", "fields", "calls", "keys", "names", "_get", "_slow")

    def __init__(self, name: str, given: Sequence[str], fields: Sequence[str], calls: Mapping[str, Callable[[], Any]], keys: int = 0) -> None:
        self.name = name
        self.given = frozenset(given)
        self.fields = list(fields)
        self.calls = dict(calls)
        self.keys = keys
        self.names = frozenset([name])
        picked = [f for f in self.fields if f in self.given]
        self._slow = bool(self.calls)
        self._get: Callable[[Any], Any]
        if len(picked) == 1:
            only = picked[0]
            self._get = lambda r: (r[only],)
        else:
            self._get = itemgetter(*picked)

    def _shape(self, rows: Sequence[Mapping[str, Any]]) -> None:
        """Raises for the first row with a missing or an extra key."""
        for i, row in enumerate(rows):
            if not isinstance(row, Mapping):
                raise TypeError(f"{self.name}[{i}] must be a dict, got {type(row).__name__}")
            if missing := self.given - row.keys():
                raise ValueError(f"{self.name}[{i}] has no {', '.join(sorted(missing))}")
            if extra := row.keys() - self.given:
                raise ValueError(f"{self.name}[{i}] has the key {', '.join(sorted(extra))}, which the prepared write does not set")

    def rows(self, values: Mapping[str, Any]) -> list[Any]:
        rows = values[self.name]
        if not isinstance(rows, (list, tuple)):
            rows = list(rows)
        try:
            if self._slow:
                cells = [[r[f] if f in self.given else self.calls[f]() for f in self.fields] for r in rows]
            else:
                cells = [self._get(r) for r in rows]
            same = len(set(map(len, rows))) <= 1 and (not rows or len(rows[0]) == len(self.given))
        except (KeyError, TypeError):
            self._shape(rows)
            raise
        if not same:
            self._shape(rows)
        if self.keys:
            self._check_keys(cells)
        return cells

    def _check_keys(self, cells: list[Any]) -> None:
        n = self.keys
        keys = [c[0] if n == 1 else tuple(c[:n]) for c in cells]
        for i, key in enumerate(keys):
            if key is None or (n > 1 and None in key):
                raise ValueError(f"{self.name}[{i}] has no {', '.join(self.fields[:n])}")
        if len(set(keys)) != len(keys):
            seen: set[Any] = set()
            for key in keys:
                if key in seen:
                    raise ValueError(f"update_many: {', '.join(self.fields[:n])}={key!r} appears twice")
                seen.add(key)


class _CompiledRows:
    """The held statement of a batch write for each engine and set of scope names."""

    __slots__ = ("kind", "model", "fields", "options", "filters", "params", "slots", "names", "declared", "returning", "batch", "natives")

    def __init__(
        self,
        kind: str,
        model: str,
        fields: list[str],
        options: dict[str, Any],
        filters: list[Any] | None,
        params: SlotParams,
        returning: bool,
        batch: int | None,
    ) -> None:
        self.kind = kind
        self.model = model
        self.fields = fields
        self.options = options
        self.filters = filters
        self.params = list(params)
        self.slots = [(i, p.name) for i, p in enumerate(params) if isinstance(p, _Slot)]
        self.names = frozenset(name for _, name in self.slots)
        self.declared: list[str | None] = [p.type if isinstance(p, _Slot) else None for p in params]
        self.returning = returning
        self.batch = batch
        self.natives: dict[tuple[Any, tuple[str, ...]], Any] = {}

    def bind(self, values: Mapping[str, Any], merged: Mapping[str, Any]) -> list[Any]:
        params = self.params.copy()
        for i, name in self.slots:
            params[i] = values[name]
        params.extend(merged.values())
        return params

    def scoped(self, merged: Mapping[str, Any]) -> dict[str, Any]:
        """The options with the indexes of the scope values."""
        options = dict(self.options)
        index = {name: len(self.params) + i for i, name in enumerate(merged)}
        if self.kind == "insert":
            if index:
                options["scope"] = index
        else:
            options["filters"] = {"filters": self.filters, "scope": index} if index else self.filters
        return options

    def native(self, engine: Any, merged: Mapping[str, Any]) -> Any:
        key = (engine, tuple(merged))
        held = self.natives.get(key)
        if held is None:
            declared = [*self.declared, *([None] * len(merged))]
            prepare = engine.prepare_insert if self.kind == "insert" else engine.prepare_update_many
            held = self.natives[key] = prepare(
                self.model, self.fields, json.dumps(self.scoped(merged)), len(declared), declared, self.returning, self.batch
            )
        return held


class PreparedRows(Generic[R_co]):
    """An insert, ``insert_many`` or ``update_many`` compiled by ``prepare()``.

    ``shape`` is what a call gives: the row count (``"count"``), one instance (``"one"``),
    an instance or ``None`` (``"optional"``), or the instances (``"list"``)."""

    __slots__ = ("_qs", "_plan", "_compiled", "_names", "_shape", "_key", "_empty", "_first")

    def __init__(
        self,
        qs: QuerySet[Any],
        plan: OneRow | ManyRows,
        compiled: _CompiledRows,
        shape: str,
        key: tuple[Any, ...],
    ) -> None:
        self._qs = qs
        self._plan = plan
        self._compiled = compiled
        self._names = plan.names | compiled.names
        self._shape = shape
        self._key = key
        self._empty: Any = [] if shape in ("list", "optional") else 0
        self._first = shape == "one" or shape == "optional"
        try:
            compiled.native(_inspection_native(qs), _scope_values(None))  # a statement that cannot be held fails here
        except QueryError as e:
            if "which is not set" not in str(e):  # a scope that is not set yet waits for the first call
                raise

    def __call__(self, *, scope: Mapping[str, Any] | None = None, **values: Any) -> Awaitable[R_co]:
        if values.keys() != self._names:
            _known(self._names, values)
        c = self._compiled
        rows = self._plan.rows(values)
        if not rows:
            return _done(self._empty)
        merged = _scope_values(scope)
        params = c.bind(values, merged)
        db = resolve(self._qs._db)
        held = c.native(db._engine, merged)
        tx, allowed = db._tx(), allowed_writes()
        pending = db._call(lambda t: held.run(rows, params, tx, self._qs._db, allowed, t), self._key)
        return _first(pending, required=self._shape == "one") if self._first else pending

    def inspect(self, *, scope: Mapping[str, Any] | None = None, **values: Any) -> QueryInspection:
        """The statements a call with these values plans, as ``inspect()`` of the same write gives them."""
        _known(self._names, values)
        c = self._compiled
        rows = self._plan.rows(values)
        if not rows:
            return QueryInspection(0, False, ())
        merged = _scope_values(scope)
        params = c.bind(values, merged)
        engine = _inspection_native(self._qs)
        returning = self._shape != "count"
        options = c.scoped(merged)
        if c.kind == "insert":
            text = engine.inspect_insert(c.model, c.fields, rows, json.dumps(options), params, c.batch, returning)
        else:
            text = engine.inspect_update_many(
                c.model, c.fields, rows, json.dumps(options["filters"]), params, c.batch, returning,
                model_fields=options.get("model_fields"), without_defaults=json.dumps(options.get("without_defaults", False)),
            )
        return QueryInspection._from_json(text)

    def __repr__(self) -> str:
        return f"<PreparedRows {self._compiled.kind} {self._qs.model.__name__} ({', '.join(self._compiled.fields)})>"


def prepare_insert(qs: QuerySet[Any], insert: Any, shape: str) -> PreparedRows[Any]:
    """The held form of ``insert`` (an ``InsertOne`` or ``InsertMany`` of ``write.py``)."""
    model = qs.model
    plan = insert._prep
    if plan is None:
        if isinstance(insert, InsertMany):
            raise TypeError("prepare() takes the rows as param('rows', fields=(...)): insert_many(param('rows', fields=(Model.a, Model.b)))")
        plan = OneRow(insert._rows[0])
    fields = list(insert._fields)
    options = {
        "conflict": insert._conflict,
        "update": insert._update,
        "set": insert._set[0] if insert._set else [],
        "indexes": insert._indexes,
        "without_defaults": qs._without_defaults,
        "model_fields": insert._names,
    }
    params = insert._set[1] if insert._set else SlotParams()
    compiled = _CompiledRows(
        "insert", model._meta.name, fields, options, None, params, shape != "count", insert._batch_size
    )
    return PreparedRows(qs, plan, compiled, shape, ("insert", model._meta.name, fields, insert._conflict))


def prepare_update_many(stmt: Any, shape: str) -> PreparedRows[Any]:
    """The held form of an ``UpdateMany`` whose rows are a ``param(..., fields=...)``."""
    qs = stmt._qs
    plan = stmt._plan
    if plan is None:
        raise TypeError("prepare() takes the rows as param('rows', fields=(...)): update_many(param('rows', fields=(Model.id, Model.a)))")
    params = SlotParams()
    ir = qs._mutation_ir("update", params, guard=False)
    if "with" in ir:
        raise QueryError("update_many() filters can't read CTEs")
    options = {"model_fields": stmt._names, "without_defaults": qs._without_defaults}
    compiled = _CompiledRows(
        "update_many", qs.model._meta.name, list(stmt._fields), options, ir["filters"], params, shape != "count", stmt._batch_size
    )
    return PreparedRows(qs, plan, compiled, shape, ("update_many", qs.model._meta.name, list(stmt._fields)))
