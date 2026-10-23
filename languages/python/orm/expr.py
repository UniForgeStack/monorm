"""Expression tree for filters, ordering and updates.

``User.email`` evaluates to a :class:`ColumnRef`; comparing it builds a
:class:`Condition`. ``User.posts`` evaluates to a :class:`RelationPath`, whose
attributes are the related model's columns and relations, so
``User.posts.created_at < yesterday`` is a condition on a column reached through a
relation. Nothing here talks to the database: expressions compile to the IR
dictionaries consumed by the native engine (see ``crates/core/src/ir.rs``).
"""

from __future__ import annotations

from collections.abc import Callable, Iterable, Sequence
from datetime import datetime
from decimal import Decimal
from typing import TYPE_CHECKING, Any, Generic, Self, TypeVar, Union, Unpack, overload
from typing import Literal as _Literal

from .errors import QueryError

if TYPE_CHECKING:
    from .cte import Cte
    from .fields import Field
    from .model import Model
    from .query import QuerySet
    from .select import Select

T = TypeVar("T")
E = TypeVar("E")
M = TypeVar("M", bound="Model")

IR = dict[str, Any]
# Where NULLs go in an ordering.
NullsPlace = _Literal["first", "last"]
NullRule = _Literal["or_null", "and_not_null"]

__all__ = [
    "Expression",
    "ColumnRef",
    "RelationPath",
    "Condition",
    "Ordering",
    "Literal",
    "Param",
    "param",
    "Excluded",
    "excluded",
    "Func",
    "Case",
    "JsonPath",
    "TsVector",
    "TsQuery",
    "Labeled",
    "Window",
    "WindowDef",
    "window",
    "ScalarSubquery",
    "func",
    "outer",
    "exists",
    "and_",
    "or_",
    "not_",
]


class _Ctes:
    """The CTEs one statement uses, in dependency order, compiled once each."""

    __slots__ = ("items", "building")

    def __init__(self) -> None:
        self.items: list[tuple[Cte, IR]] = []
        self.building: list[Cte] = []

    def use(self, cte: Cte, params: list[Any]) -> None:
        for c, _ in self.items:
            if c is cte:
                return
            if c.name == cte.name:
                raise ValueError(f"two different CTEs are named {cte.name!r} in one query")
        if any(c is cte for c in self.building):
            return  # the recursive part of `cte` reading `cte`
        self.building.append(cte)
        try:
            ir = cte._body_ir(params, self)
        finally:
            self.building.pop()
        self.items.append((cte, ir))


class IRContext:
    """Collects literal parameters and CTEs while compiling, and checks that every
    column belongs to the query's root model (or CTE). ``outer`` is the context of the
    enclosing query, for subqueries."""

    __slots__ = ("root", "params", "outer", "ctes", "windows", "_owner", "_aliases")

    def __init__(
        self, root: type[Model] | Cte, params: list[Any], outer: IRContext | None = None, ctes: _Ctes | None = None
    ) -> None:
        self.root = root
        self.params = params
        self.outer = outer
        self._owner = outer is None and ctes is None
        self.ctes: _Ctes = ctes if ctes is not None else (outer.ctes if outer is not None else _Ctes())
        # Named windows of this query (not of enclosing ones), with their IR.
        self.windows: list[tuple[WindowDef, IR]] = []
        # `alias()` tokens by first use, so equal queries give equal IR (cache keys).
        self._aliases: dict[Alias, int] = {}

    def alias_tags(self, alias: tuple[Alias | None, ...]) -> list[int] | None:
        """The IR tags of a path's hops: 0 for an untagged hop, else the query-local number of its alias."""
        if not any(alias):
            return None
        return [0 if a is None else self._aliases.setdefault(a, len(self._aliases) + 1) for a in alias]

    def param(self, value: Any) -> IR:
        if isinstance(value, Ordering):
            raise TypeError(f"{value!r} is an ordering, for order_by(); write 0 - {value.expr!r} to negate a value")
        self.params.append(value)
        return {"t": "param", "i": len(self.params) - 1}

    def use_cte(self, cte: Cte) -> None:
        self.ctes.use(cte, self.params)

    def window_name(self, w: WindowDef) -> str:
        """The name of ``w`` in this query's ``WINDOW`` clause (declared on first use)."""
        for i, (known, _) in enumerate(self.windows):
            if known is w:
                return f"w{i + 1}"
        name = f"w{len(self.windows) + 1}"
        self.windows.append((w, {"name": name, **w._spec_ir(self)}))
        return name

    def add_windows(self, ir: IR) -> None:
        if self.windows:
            ir["windows"] = [w for _, w in self.windows]

    def finish(self, ir: IR) -> IR:
        """Declares the CTEs the statement uses (``WITH``), if this context compiles the
        statement itself rather than a part of it."""
        if self._owner and self.ctes.items:
            ir["with"] = [c for _, c in self.ctes.items]
        return ir


def _bool_misuse(self: object) -> bool:
    raise TypeError(
        "ORM expressions have no truth value; combine conditions with & | ~ "
        "instead of and / or / not, and compare with None using == / !=."
    )


class Node:
    __slots__ = ()

    def _ir(self, ctx: IRContext) -> IR:
        raise NotImplementedError

    __bool__ = _bool_misuse


def _wrap(value: Any) -> Node:
    return value if isinstance(value, Node) else Literal(value)


def _wrap_value(value: Any) -> Node:
    """`_wrap` for a value that holds a condition (a function argument, a `case()` value): the
    path null rules apply to it, and the strict check does not, as in `case()` conditions."""
    node = _wrap(value)
    return resolve_null_rule(node, strict=False) if isinstance(node, Condition) else node


class Expression(Node, Generic[T]):
    """A SQL value expression whose Python type is ``T``."""

    __slots__ = ()

    # Comparisons -----------------------------------------------------------------------
    # `__eq__` / `__ne__` return conditions, SQLAlchemy style.

    def __eq__(self, other: T | Expression[T] | None) -> Condition:  # type: ignore[override]
        if other is None:
            return IsNull(self, neg=False)
        return Comparison("eq", self, _wrap(other))

    def __ne__(self, other: T | Expression[T] | None) -> Condition:  # type: ignore[override]
        if other is None:
            return IsNull(self, neg=True)
        return Comparison("ne", self, _wrap(other))

    def __lt__(self, other: T | Expression[T]) -> Condition:
        return Comparison("lt", self, _wrap(other))

    def __le__(self, other: T | Expression[T]) -> Condition:
        return Comparison("le", self, _wrap(other))

    def __gt__(self, other: T | Expression[T]) -> Condition:
        return Comparison("gt", self, _wrap(other))

    def __ge__(self, other: T | Expression[T]) -> Condition:
        return Comparison("ge", self, _wrap(other))

    __hash__ = object.__hash__

    def in_(self, values: Iterable[T] | Select[T]) -> Condition:
        """``IN (values)``, or ``IN (SELECT ...)`` for a one-column ``select()`` query."""
        from .select import Select

        if isinstance(values, Select):
            return InSelect(self, values, neg=False)
        if isinstance(values, Param):
            raise TypeError("in_() can't take a param(); the number of values is part of the query")
        values = list(values)
        if not values:
            return Const(False, self)
        return In(self, [_wrap(v) for v in values], neg=False)

    def not_in(self, values: Iterable[T] | Select[T]) -> Condition:
        from .select import Select

        if isinstance(values, Select):
            return InSelect(self, values, neg=True)
        if isinstance(values, Param):
            raise TypeError("not_in() can't take a param(); the number of values is part of the query")
        values = list(values)
        if not values:
            return Const(True, self)
        return In(self, [_wrap(v) for v in values], neg=True)

    def is_null(self) -> Condition:
        return IsNull(self, neg=False)

    def is_not_null(self) -> Condition:
        return IsNull(self, neg=True)

    def between(self, low: T | Expression[T], high: T | Expression[T]) -> Condition:
        return Together([self >= low, self <= high])

    # String matching ---------------------------------------------------------------------

    def like(self: Expression[str] | Expression[str | None], pattern: str) -> Condition:
        return Like(self, pattern, ci=False)

    def ilike(self: Expression[str] | Expression[str | None], pattern: str) -> Condition:
        return Like(self, pattern, ci=True)

    def contains(self: Expression[str] | Expression[str | None], text: str) -> Condition:
        return Like(self, _pattern(text, _contains), ci=False)

    def icontains(self: Expression[str] | Expression[str | None], text: str) -> Condition:
        return Like(self, _pattern(text, _contains), ci=True)

    def startswith(self: Expression[str] | Expression[str | None], text: str) -> Condition:
        return Like(self, _pattern(text, _startswith), ci=False)

    def endswith(self: Expression[str] | Expression[str | None], text: str) -> Condition:
        return Like(self, _pattern(text, _endswith), ci=False)

    # Arrays ---------------------------------------------------------------------------------

    def has(self: Expression[list[E]] | Expression[list[E] | None], value: E) -> Condition:
        """Array columns: ``value`` is one of the elements (``col @> ARRAY[value]``)."""
        if isinstance(value, Param):
            return Comparison("contains", self, _Bound(value, _one_element))
        return Comparison("contains", self, Literal([value]))

    def has_all(self: Expression[list[E]] | Expression[list[E] | None], values: Iterable[E]) -> Condition:
        """Array columns: every one of ``values`` is an element (``col @> values``)."""
        return Comparison("contains", self, Literal(list(values)))

    def has_any(self: Expression[list[E]] | Expression[list[E] | None], values: Iterable[E]) -> Condition:
        """Array columns: at least one of ``values`` is an element (``col && values``)."""
        return Comparison("overlaps", self, Literal(list(values)))

    def contained_by(self: Expression[list[E]] | Expression[list[E] | None], values: Iterable[E]) -> Condition:
        """Array columns: every element is one of ``values`` (``col <@ values``)."""
        return Comparison("contained_by", self, Literal(list(values)))

    @overload
    def __getitem__(self: Expression[list[E]], index: int) -> Func[E | None]: ...
    @overload
    def __getitem__(self: Expression[list[E] | None], index: int) -> Func[E | None]: ...
    @overload
    def __getitem__(self: Expression[Any], index: str) -> JsonPath: ...
    def __getitem__(self: Expression[Any], index: int | str) -> Expression[Any]:
        """Array columns: the element at SQL's 1-based ``index`` (``col[1]`` is the first),
        ``None`` out of range. JSON columns: the value under a key or at a 0-based array
        index (``meta["tags"][0]``, ``meta -> 'tags' -> 0``). PostgreSQL only."""
        if isinstance(self, JsonPath) or (isinstance(self, ColumnRef) and self._field.type_name == "json"):
            return JsonPath(self, (index,))
        if isinstance(index, str):
            raise TypeError(f"{self!r} is not a JSON column; a string key needs one")
        return Func("element", (self, _Int(index)))

    # ``__getitem__`` alone would make every expression iterable.
    __iter__ = None

    # JSON -----------------------------------------------------------------------------------

    def json_contains(self, value: Any) -> Condition:
        """JSON: the value contains ``value`` at the top level (``col @> value``):
        ``Post.meta.json_contains({"tags": ["a"]})``. PostgreSQL only."""
        return Comparison("contains", self, _json_operand(value, "json_contains"))

    def json_contained_by(self, value: Any) -> Condition:
        """JSON: ``value`` contains the value (``col <@ value``). PostgreSQL only."""
        return Comparison("contained_by", self, _json_operand(value, "json_contained_by"))

    def has_key(self, key: str) -> Condition:
        """JSON: the object has the top-level key ``key``, or the array the string element
        (``col ? key``). PostgreSQL only."""
        if not isinstance(key, str):
            raise TypeError(f"has_key() takes a string, got {key!r}")
        return Comparison("has_key", self, Literal(key))

    def json_merge(self, value: Any) -> Expression[Any]:
        """JSON: ``col || value``: the objects merged (the keys of ``value`` win), or the
        arrays joined. For updates: ``update(meta=Post.meta.json_merge({"seen": True}))``.
        PostgreSQL only."""
        _not_none(value, "json_merge")
        return Arith("json_merge", self, value if isinstance(value, Node) else Literal(value))

    # Full-text search ------------------------------------------------------------------------

    def matches(self: Expression[TsVector], query: Expression[TsQuery] | str) -> Condition:
        """``vector @@ query``: the document matches the search. A plain string is
        ``plainto_tsquery(<config of the vector>, query)``. PostgreSQL only."""
        if isinstance(query, (str, Param)):
            config = (
                self._args[0]
                if isinstance(self, Func) and self._name == "to_tsvector" and len(self._args) == 2
                else None
            )
            query = Func("plainto_tsquery", (query,) if config is None else (config, query))
        return Comparison("match", self, query)

    # Strings -------------------------------------------------------------------------------

    @overload
    def concat(self: Expression[str], other: str | Expression[str]) -> Expression[str]: ...  # pyright: ignore[reportOverlappingOverload]
    @overload
    def concat(
        self: Expression[str] | Expression[str | None], other: str | Expression[str] | Expression[str | None]
    ) -> Expression[str | None]: ...
    def concat(self: Expression[Any], other: Any) -> Expression[Any]:
        """``self || other``: ``NULL`` when either side is ``NULL``. :func:`func.concat`
        reads ``NULL`` as an empty string instead."""
        return Arith("concat", self, _wrap(other))

    # Arithmetic ----------------------------------------------------------------------------

    def __add__(self, other: T | Expression[T]) -> Expression[T]:
        return Arith("add", self, _wrap(other))

    def __sub__(self, other: T | Expression[T]) -> Expression[T]:
        return Arith("sub", self, _wrap(other))

    def __mul__(self, other: T | Expression[T]) -> Expression[T]:
        return Arith("mul", self, _wrap(other))

    def __truediv__(self, other: T | Expression[T]) -> Expression[T]:
        return Arith("div", self, _wrap(other))

    def __radd__(self, other: T) -> Expression[T]:
        return Arith("add", _wrap(other), self)

    def __rsub__(self, other: T) -> Expression[T]:
        return Arith("sub", _wrap(other), self)

    def __rmul__(self, other: T) -> Expression[T]:
        return Arith("mul", _wrap(other), self)

    # Boolean columns used directly as conditions: `filter(Post.published & ...)`.

    def __and__(self: Expression[bool], other: Condition | Expression[bool]) -> Condition:
        return as_condition(self) & other

    def __or__(self: Expression[bool], other: Condition | Expression[bool]) -> Condition:
        return as_condition(self) | other

    def __invert__(self: Expression[bool]) -> Condition:
        return ~as_condition(self)

    # Naming ----------------------------------------------------------------------------------

    def label(self, name: str) -> Labeled[T]:
        """The name of this expression in ``select()`` rows: ``row.<name>``."""
        return Labeled(self, name)

    # Ordering --------------------------------------------------------------------------------

    def asc(self, *, nulls: NullsPlace | None = None) -> Ordering:
        """Ascending; ``nulls="first"`` / ``"last"`` places NULLs (the database's default otherwise)."""
        return Ordering(self, desc=False, nulls=nulls)

    def desc(self, *, nulls: NullsPlace | None = None) -> Ordering:
        """Descending; ``nulls="first"`` / ``"last"`` places NULLs (the database's default otherwise)."""
        return Ordering(self, desc=True, nulls=nulls)


def _not_none(value: Any, method: str) -> None:
    if value is None:
        raise TypeError(f"{method}() takes a JSON value, not None")


def _json_operand(value: Any, method: str) -> Node:
    """A JSON value (or a :func:`param`) as the operand of a containment test."""
    _not_none(value, method)
    if isinstance(value, Param):
        return value
    if isinstance(value, Node):
        raise TypeError(f"{method}() takes a JSON value or a param(), not the expression {value!r}")
    return Literal(value)


def _escape_like(text: str) -> str:
    return text.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")


def _contains(text: str) -> str:
    return f"%{_escape_like(text)}%"


def _startswith(text: str) -> str:
    return f"{_escape_like(text)}%"


def _endswith(text: str) -> str:
    return f"%{_escape_like(text)}"


def _one_element(value: Any) -> list[Any]:
    return [value]


def _pattern(text: Any, make: Callable[[str], str]) -> Any:
    """The LIKE pattern for ``text``, or for a :func:`param` once its value is known."""
    return _Bound(text, make) if isinstance(text, Param) else make(text)


class _Bound(Expression[Any]):
    """A :func:`param` whose value goes through ``transform`` when bound."""

    __slots__ = ("param", "transform")

    def __init__(self, param: Param, transform: Callable[[Any], Any]) -> None:
        self.param = param
        self.transform = transform

    def _ir(self, ctx: IRContext) -> IR:
        return self.param._slot(ctx.params, self.transform)


class Literal(Expression[Any]):
    __slots__ = ("value",)

    def __init__(self, value: Any) -> None:
        self.value = value

    def _ir(self, ctx: IRContext) -> IR:
        return ctx.param(self.value)

    def __repr__(self) -> str:
        return repr(self.value)


class _Slot:
    """A placeholder in a prepared query's parameter list, filled in per call:
    ``transform`` turns the call's value into the parameter (a LIKE pattern, ...)."""

    __slots__ = ("name", "transform")

    def __init__(self, name: str, transform: Callable[[Any], Any] | None = None) -> None:
        self.name = name
        self.transform = transform

    def __repr__(self) -> str:
        return f"param({self.name!r})"


class SlotParams(list[Any]):
    """The parameter list of a query compiled by ``prepare()``: the only one that
    accepts :func:`param` placeholders."""

    __slots__ = ()


class Param(Expression[Any]):
    """A value supplied when a prepared query runs; see :func:`param`."""

    __slots__ = ("name",)

    def __init__(self, name: str) -> None:
        if not name.isidentifier():
            raise ValueError(f"param() takes an identifier, got {name!r}")
        self.name = name

    def _slot(self, ctx_params: list[Any], transform: Callable[[Any], Any] | None = None) -> IR:
        if not isinstance(ctx_params, SlotParams):
            raise QueryError(f"{self!r} is a placeholder of a prepared query; call .prepare() on the query set")
        ctx_params.append(_Slot(self.name, transform))
        return {"t": "param", "i": len(ctx_params) - 1}

    def _ir(self, ctx: IRContext) -> IR:
        return self._slot(ctx.params)

    def __repr__(self) -> str:
        return f"param({self.name!r})"


def param(name: str) -> Any:
    """A placeholder for a value given each time a prepared query runs::

        by_author = Post.objects.filter(Post.author_id == param("author")).limit(param("n")).prepare()
        posts = await by_author(author=3, n=10)

    It stands for a value in comparisons, arithmetic, ``like`` / ``contains`` / ...,
    ``has()``, ``limit()`` and ``offset()``. Typed as ``Any`` so it fits wherever a
    value does."""
    return Param(name)


def _same_table_model(column_root: type[Model], root: type[Model] | Cte) -> type[Model] | None:
    """``root`` when it is a model other than ``column_root`` that stores its rows in the same table."""
    if isinstance(root, type) and root._meta.table == column_root._meta.table:
        return root
    return None


class Alias:
    """One ``User.posts.alias()`` call: the related row it names is one row for the whole
    query, and another row than the one of any other alias or of the plain path."""

    __slots__ = ()


ALIAS_PLACES = (
    "alias() marks another related row in a filter condition; "
    "it works only in filter() and exclude()"
)


def no_alias(item: Any) -> None:
    """Raise when ``item`` (a column or a path) carries an ``alias()``: the place has no related row to name."""
    if any(getattr(item, "_alias", ())):
        raise QueryError(f"{item!r}: {ALIAS_PLACES}")


class ColumnRef(Expression[T]):
    """A column of ``root``'s model, or of a model reached from it through ``path``."""

    __slots__ = ("_root", "_path", "_field", "_rule", "_alias")
    _rule: NullRule | None

    def __init__(
        self,
        root: type[Model],
        path: tuple[str, ...],
        field: Field[Any],
        rule: NullRule | None = None,
        alias: tuple[Alias | None, ...] = (),
    ) -> None:
        self._root = root
        self._path = path
        self._field = field
        self._rule = rule
        self._alias = alias

    def _ir(self, ctx: IRContext) -> IR:
        if self._root is not ctx.root:
            own = _same_table_model(self._root, ctx.root)
            if own is not None:
                # Models on one table share their columns: the query's own model resolves the name.
                if self._path:
                    if self._path[0] not in own._meta.relations:
                        raise LookupError(f"{own.__name__} has no relation {self._path[0]!r}")
                    return {"t": "col", "path": list(self._path), "name": self._field.name}
                field = own._meta.fields.get(self._field.name)
                if field is None:
                    raise LookupError(f"{own.__name__} has no field {self._field.name!r}")
                return {"t": "col", "path": [], "name": field.name}
            root = ctx.root.__name__ if isinstance(ctx.root, type) else f"CTE {ctx.root.name}"
            hint = f"reach it through a relation of {root} instead"
            c = ctx.outer
            while c is not None:
                if c.root is self._root:
                    hint = f"use outer({self!r}) for a column of the enclosing query"
                    break
                c = c.outer
            raise ValueError(f"{self!r} belongs to {self._root.__name__}, not to a {root} query; {hint}")
        ir: IR = {"t": "col", "path": list(self._path), "name": self._field.name}
        if (tags := ctx.alias_tags(self._alias)) is not None:
            ir["alias"] = tags
        return ir

    @property
    def _key(self) -> str:
        """``repr()`` plus the aliases: two aliases of one column are two columns."""
        return repr(self) + "".join(f"#{id(a)}" for a in self._alias if a is not None)

    @property
    def _nullable(self) -> bool:
        """Whether the ORM cannot prove the column non-null."""
        return bool(self._field.nullable)

    def __neg__(self) -> Ordering:
        """``-Post.created_at`` is ``Post.created_at.desc()``, for ``order_by()``. It is
        never SQL negation; write ``0 - Post.views`` for that."""
        return self.desc()

    def __repr__(self) -> str:
        return ".".join((self._root.__name__, *self._path, self._field.name))


class Excluded(Expression[T]):
    """``excluded(Post.views)``: the value a conflicting insert proposed for a column,
    usable in ``on_conflict(..., update=True, update_values={"views": Post.views + excluded(Post.views)})``."""

    __slots__ = ("_column",)

    def __init__(self, column: ColumnRef[T]) -> None:
        if not isinstance(column, ColumnRef) or column._path:
            raise TypeError(f"excluded() takes a column of the inserted model, got {column!r}")
        self._column = column

    def _ir(self, ctx: IRContext) -> IR:
        self._column._ir(ctx)  # checks the model
        return {"t": "excluded", "name": self._column._field.name}

    def __repr__(self) -> str:
        return f"excluded({self._column!r})"


class Labeled(Expression[T]):
    __slots__ = ("_inner", "_name")

    def __init__(self, inner: Node, name: str) -> None:
        if not name.isidentifier() or name.startswith("_"):
            raise ValueError(f"label {name!r} must be an identifier not starting with '_'")
        self._inner = inner
        self._name = name

    def _ir(self, ctx: IRContext) -> IR:
        return self._inner._ir(ctx)

    def __repr__(self) -> str:
        return f"{self._inner!r}.label({self._name!r})"


class Func(Expression[T]):
    """A SQL function call; build it with :data:`func`."""

    __slots__ = ("_name", "_args", "_rel", "_distinct", "_filter", "_order")

    def __init__(
        self,
        name: str,
        args: tuple[Any, ...] = (),
        rel: RelationPath[Any] | None = None,
        distinct: bool = False,
        filter: ConditionLike | None = None,
        order_by: Expression[Any] | Ordering | Iterable[Expression[Any] | Ordering] | None = None,
    ) -> None:
        self._name = name
        self._args = tuple(_wrap_value(a) for a in args)
        self._rel = rel
        self._distinct = distinct
        self._filter = None if filter is None else resolve_null_rule(as_condition(filter))
        self._order = _orderings(order_by)

    def _ir(self, ctx: IRContext) -> IR:
        ir: IR = {"t": "func", "name": self._name, "args": [a._ir(ctx) for a in self._args]}
        if self._rel is not None:
            if self._rel._root is not ctx.root:
                root = ctx.root.__name__ if isinstance(ctx.root, type) else ctx.root.name
                raise ValueError(f"{self._rel!r} does not start at {root}")
            no_alias(self._rel)
            ir["rel"] = list(self._rel._path)
        if self._distinct:
            ir["distinct"] = True
        if self._filter is not None:
            ir["filter"] = self._filter._ir(ctx)
        if self._order:
            ir["order_by"] = [o._ir(ctx) for o in self._order]
        return ir

    def __repr__(self) -> str:
        args = [repr(a) for a in self._args] + ([repr(self._rel)] if self._rel is not None else [])
        if self._order:
            args.append(f"order_by={self._order!r}")
        if self._filter is not None:
            args.append(f"filter={self._filter!r}")
        return f"func.{self._name}({', '.join(args)})"

    def over(
        self,
        window: WindowDef | Expression[Any] | Iterable[Expression[Any]] | None = None,
        /,
        order_by: Expression[Any] | Ordering | Iterable[Expression[Any] | Ordering] | None = None,
        *,
        partition_by: Expression[Any] | Iterable[Expression[Any]] | None = None,
        rows: tuple[int | None, int | None] | None = None,
        range: tuple[int | None, int | None] | None = None,
    ) -> Window[T]:
        """``<function> OVER (...)``: the function computed over a window of rows related
        to each row, without grouping them::

            func.row_number().over(partition_by=Post.author_id, order_by=Post.views.desc())
            func.sum(Post.views).over(order_by=Post.created_at, rows=(None, 0))  # running total

            w = window(partition_by=Post.author_id, order_by=Post.created_at)  # shared
            Post.objects.select(func.sum(Post.views).over(w), func.avg(Post.views).over(w))

        Over a :func:`window`, ``order_by`` and ``rows`` / ``range`` may extend it when it
        has none of its own (``OVER (w ROWS ...)``); its partitioning is fixed. ``rows`` /
        ``range`` set the frame as ``(start, end)``: ``None`` is unbounded, ``0`` the
        current row, ``-n`` n preceding, ``n`` n following. A first positional argument
        that isn't a window is ``partition_by``.
        """
        frame = _frame(rows, range)
        if not isinstance(window, WindowDef):
            if window is not None:
                if partition_by is not None:
                    raise TypeError("over() got partition_by twice")
                partition_by = window
            return Window(self, None, _many(partition_by), _orderings(order_by), frame)
        if partition_by is not None:
            raise ValueError("over(window) can't add partition_by: the window defines it")
        if order_by is not None and window._order:
            raise ValueError("over(window, order_by=...) needs a window without its own order_by")
        if frame is not None and window._frame is not None:
            raise ValueError("over(window, rows/range=...) needs a window without its own frame")
        return Window(self, window, [], _orderings(order_by), frame)


def _frame(rows: tuple[int | None, int | None] | None, range_: tuple[int | None, int | None] | None) -> IR | None:
    if rows is not None and range_ is not None:
        raise ValueError("over() takes rows or range, not both")
    frame = None
    for kind, bounds in (("rows", rows), ("range", range_)):
        if bounds is not None:
            start, end = bounds
            for b in (start, end):
                if b is not None and (isinstance(b, bool) or not isinstance(b, int)):
                    raise TypeError(f"frame bounds are ints or None, got {b!r}")
            frame = {"kind": kind, "start": start, "end": end}
    return frame


class WindowDef:
    """A window definition shared by several window functions of a query: built by
    :func:`window`, used with ``func.<name>(...).over(w)``. It becomes the query's
    ``WINDOW w1 AS (...)`` clause."""

    __slots__ = ("_partition", "_order", "_frame")

    def __init__(self, partition: list[Expression[Any]], order: list[Ordering], frame: IR | None) -> None:
        self._partition = partition
        self._order = order
        self._frame = frame

    def _spec_ir(self, ctx: IRContext) -> IR:
        ir: IR = {}
        if self._partition:
            ir["partition_by"] = [p._ir(ctx) for p in self._partition]
        if self._order:
            ir["order_by"] = [o._ir(ctx) for o in self._order]
        if self._frame is not None:
            ir["frame"] = self._frame
        return ir

    def __repr__(self) -> str:
        return f"window(partition_by={self._partition!r}, order_by={self._order!r})"


def window(
    partition_by: Expression[Any] | Iterable[Expression[Any]] | None = None,
    order_by: Expression[Any] | Ordering | Iterable[Expression[Any] | Ordering] | None = None,
    *,
    rows: tuple[int | None, int | None] | None = None,
    range: tuple[int | None, int | None] | None = None,
) -> WindowDef:
    """A named window (``WINDOW w AS (PARTITION BY ... ORDER BY ...)``) for several
    window functions to share: ``func.sum(x).over(w)``, ``func.avg(x).over(w)``."""
    return WindowDef(_many(partition_by), _orderings(order_by), _frame(rows, range))


def _many(items: Any) -> list[Any]:
    if items is None:
        return []
    if isinstance(items, (Expression, Ordering)):
        return [items]
    return list(items)


def _orderings(items: Any) -> list[Ordering]:
    return [i if isinstance(i, Ordering) else Ordering(i, desc=False) for i in _many(items)]


class Window(Expression[T]):
    """``func.<name>(...).over(...)``: a window function call."""

    __slots__ = ("_func", "_base", "_partition", "_order", "_frame")

    def __init__(
        self,
        fn: Func[T],
        base: WindowDef | None,
        partition: list[Expression[Any]],
        order: list[Ordering],
        frame: IR | None,
    ) -> None:
        self._func = fn
        self._base = base
        self._partition = partition
        self._order = order
        self._frame = frame

    def _ir(self, ctx: IRContext) -> IR:
        ir: IR = {"t": "window", "func": self._func._ir(ctx)}
        if self._base is not None:
            ir["base"] = ctx.window_name(self._base)
        if self._partition:
            ir["partition_by"] = [p._ir(ctx) for p in self._partition]
        if self._order:
            ir["order_by"] = [o._ir(ctx) for o in self._order]
        if self._frame is not None:
            ir["frame"] = self._frame
        return ir

    def __repr__(self) -> str:
        return f"{self._func!r}.over(partition_by={self._partition!r}, order_by={self._order!r})"


class _Int(Expression[int]):
    """An integer written into the SQL text (window function arguments)."""

    __slots__ = ("value",)

    def __init__(self, value: int) -> None:
        if isinstance(value, bool) or not isinstance(value, int):
            raise TypeError(f"expected an int, got {value!r}")
        self.value = value

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "int", "value": self.value}

    def __repr__(self) -> str:
        return repr(self.value)


class Outer(Expression[T]):
    """``outer(User.id)``: a column of an enclosing query, in a subquery."""

    __slots__ = ("_column",)

    def __init__(self, column: ColumnRef[T]) -> None:
        if not isinstance(column, ColumnRef):
            raise TypeError(f"outer() takes a column of a model, got {column!r}")
        no_alias(column)
        self._column = column

    def _ir(self, ctx: IRContext) -> IR:
        depth, c = 1, ctx.outer
        while c is not None:
            if c.root is self._column._root:
                return {
                    "t": "outer",
                    "depth": depth,
                    "path": list(self._column._path),
                    "name": self._column._field.name,
                }
            depth, c = depth + 1, c.outer
        raise ValueError(f"{self!r} is not a column of an enclosing query")

    def __repr__(self) -> str:
        return f"outer({self._column!r})"


class Case(Expression[T]):
    """``func.case((cond, value), ..., default=v)``: ``CASE WHEN ... END``."""

    __slots__ = ("_whens", "_default")

    def __init__(self, whens: tuple[tuple[Any, Any], ...], default: Any) -> None:
        if not whens:
            raise TypeError("case() needs at least one (condition, value) branch")
        for w in whens:
            if not isinstance(w, tuple) or len(w) != 2:
                raise TypeError(f"case() branches are (condition, value) tuples, got {w!r}")
        self._whens = [(resolve_null_rule(as_condition(c), strict=False), _wrap_value(v)) for c, v in whens]
        self._default = None if default is None else _wrap_value(default)

    def _ir(self, ctx: IRContext) -> IR:
        ir: IR = {"t": "case", "whens": [{"cond": c._ir(ctx), "value": v._ir(ctx)} for c, v in self._whens]}
        if self._default is not None:
            ir["default"] = self._default._ir(ctx)
        return ir

    def __repr__(self) -> str:
        whens = ", ".join(f"({c!r}, {v!r})" for c, v in self._whens)
        return f"func.case({whens}{'' if self._default is None else f', default={self._default!r}'})"


class JsonPath(Expression[Any]):
    """``Post.meta["a"]["b"]``: a ``jsonb`` value inside a JSON column. It compares as JSON
    (``== "x"`` is the JSON string ``"x"``); :meth:`as_text` reads it as text."""

    __slots__ = ("_item", "_path", "_text")

    def __init__(self, item: Expression[Any], path: tuple[str | int, ...], text: bool = False) -> None:
        for key in path:
            if isinstance(key, bool) or not isinstance(key, (str, int)):
                raise TypeError(f"JSON path steps are str keys or int indexes, got {key!r}")
        if isinstance(item, JsonPath):
            inner: JsonPath = item
            if inner._text:
                raise TypeError("as_text() ends a JSON path")
            item, path = inner._item, inner._path + path
        self._item: Expression[Any] = item
        self._path: tuple[str | int, ...] = path
        self._text: bool = text

    def __getitem__(self, index: int | str) -> JsonPath:  # type: ignore[override]
        """The value under a key or at a 0-based array index, one step deeper."""
        return JsonPath(self, (index,))

    def as_text(self) -> Expression[str | None]:
        """The value as text (the last step is ``->>``): a JSON string without quotes, so
        ``like``, ``contains`` and string functions work on it."""
        return JsonPath(self._item, self._path, text=True)

    def _ir(self, ctx: IRContext) -> IR:
        ir: IR = {"t": "json_path", "item": self._item._ir(ctx), "path": list(self._path)}
        if self._text:
            ir["text"] = True
        return ir

    def __repr__(self) -> str:
        steps = "".join(f"[{k!r}]" for k in self._path)
        return f"{self._item!r}{steps}{'.as_text()' if self._text else ''}"


class ScalarSubquery(Expression[T]):
    """``qs.select(x).as_scalar()``: a one-column subquery used as a value."""

    __slots__ = ("_select",)

    def __init__(self, select: Select[T]) -> None:
        self._select = select

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "subquery", "select": self._select._subquery_ir(ctx, "as_scalar()")}

    def __repr__(self) -> str:
        return f"({self._select!r}).as_scalar()"


N = TypeVar("N", int, float, Decimal)


class TsVector:
    """The type of a ``tsvector`` expression (``func.to_tsvector``): only for typing."""


class TsQuery:
    """The type of a ``tsquery`` expression (``func.to_tsquery``, ...): only for typing."""


class _Config(Expression[Any]):
    """A text search configuration (``'english'::regconfig``), written into the SQL so the
    expression matches an expression index."""

    __slots__ = ("name",)

    def __init__(self, name: str) -> None:
        if not isinstance(name, str):
            raise TypeError(f"a text search configuration is a name, got {name!r}")
        self.name = name

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "text", "value": self.name}

    def __repr__(self) -> str:
        return repr(self.name)


def _search(a: Any, b: Any) -> tuple[Any, ...]:
    return (a,) if b is None else (_Config(a), b)


class _Functions:
    """``func.count(...)``, ``func.sum(...)``, ...: SQL functions as expressions.

    An aggregate over a relation path is computed per row of the query, in a correlated
    subquery: ``func.count(User.posts)`` is each user's number of posts, and
    ``func.sum(User.posts.views)`` their posts' total views. Over the model's own
    columns, aggregates summarize the rows of each ``group_by()`` group (or all rows).

    Every aggregate takes ``filter=cond``: ``FILTER (WHERE cond)``, so it reads only the
    rows where ``cond`` holds: ``func.count(filter=Post.published)``. Only ``array_agg``
    takes ``order_by``: the other aggregates do not depend on the order of their rows.
    """

    __slots__ = ()

    def count(
        self,
        what: Expression[Any] | RelationPath[Any] | None = None,
        *,
        distinct: bool = False,
        filter: ConditionLike | None = None,
    ) -> Func[int]:
        """``COUNT(*)`` without argument, ``COUNT(expr)`` (non-NULL values), or the rows
        of a relation: ``func.count(User.posts)``."""
        if isinstance(what, RelationPath):
            return Func("count", rel=what, filter=filter)
        return Func("count", () if what is None else (what,), distinct=distinct, filter=filter)

    @overload
    def sum(
        self, expr: Expression[N], *, distinct: bool = False, filter: ConditionLike | None = None
    ) -> Func[N | None]: ...
    @overload
    def sum(
        self, expr: Expression[N | None], *, distinct: bool = False, filter: ConditionLike | None = None
    ) -> Func[N | None]: ...
    def sum(self, expr: Expression[Any], *, distinct: bool = False, filter: ConditionLike | None = None) -> Func[Any]:
        """``SUM``; integer sums come back as ``int`` (cast to bigint)."""
        return Func("sum", (expr,), distinct=distinct, filter=filter)

    @overload
    def avg(  # type: ignore[overload-overlap]  # pyright: ignore[reportOverlappingOverload]
        self,
        expr: Expression[Decimal] | Expression[Decimal | None],
        *,
        distinct: bool = False,
        filter: ConditionLike | None = None,
    ) -> Func[Decimal | None]: ...
    @overload
    def avg(
        self,
        expr: Expression[int] | Expression[int | None] | Expression[float] | Expression[float | None],
        *,
        distinct: bool = False,
        filter: ConditionLike | None = None,
    ) -> Func[float | None]: ...
    def avg(self, expr: Expression[Any], *, distinct: bool = False, filter: ConditionLike | None = None) -> Func[Any]:
        """``AVG``: a ``float``, or a ``Decimal`` for decimal columns (exact)."""
        return Func("avg", (expr,), distinct=distinct, filter=filter)

    def array_agg(
        self,
        expr: Expression[T],
        *,
        order_by: Expression[Any] | Ordering | Iterable[Expression[Any] | Ordering] | None = None,
        distinct: bool = False,
        filter: ConditionLike | None = None,
    ) -> Func[list[T] | None]:
        """``ARRAY_AGG(expr [ORDER BY ...])``: the values as a list, ``NULL`` values included.
        ``None`` (not ``[]``) over no rows. ``order_by`` fixes the order of the elements
        (``order_by=Post.views.desc()``); with ``distinct`` it must use the same expression.
        Not for array columns; PostgreSQL only."""
        return Func("array_agg", (expr,), distinct=distinct, filter=filter, order_by=order_by)

    def min(self, expr: Expression[T], *, filter: ConditionLike | None = None) -> Func[T | None]:
        return Func("min", (expr,), filter=filter)

    def max(self, expr: Expression[T], *, filter: ConditionLike | None = None) -> Func[T | None]:
        return Func("max", (expr,), filter=filter)

    def lower(self, expr: Expression[str] | Expression[str | None]) -> Func[str]:
        return Func("lower", (expr,))

    def upper(self, expr: Expression[str] | Expression[str | None]) -> Func[str]:
        return Func("upper", (expr,))

    def length(self, expr: Expression[str] | Expression[str | None]) -> Func[int]:
        return Func("length", (expr,))

    def concat(self, *parts: str | Expression[Any]) -> Func[str]:
        """``CONCAT(...)``: the parts as text, a ``NULL`` part as an empty string. A
        literal part is a ``str``; write a number as text (``"1"``) or as a column.
        ``a.concat(b)`` (``a || b``) is ``NULL`` when either side is ``NULL``."""
        if not parts:
            raise TypeError("concat() needs at least one part")
        return Func("concat", parts)

    def trim(self, expr: Expression[str] | Expression[str | None]) -> Func[str]:
        """Without leading and trailing spaces."""
        return Func("trim", (expr,))

    def ltrim(self, expr: Expression[str] | Expression[str | None]) -> Func[str]:
        """Without leading spaces."""
        return Func("ltrim", (expr,))

    def rtrim(self, expr: Expression[str] | Expression[str | None]) -> Func[str]:
        """Without trailing spaces."""
        return Func("rtrim", (expr,))

    def replace(
        self,
        expr: Expression[str] | Expression[str | None],
        old: str | Expression[str] | Expression[str | None],
        new: str | Expression[str] | Expression[str | None],
    ) -> Func[str]:
        """Every ``old`` in ``expr`` replaced by ``new``."""
        return Func("replace", (expr, old, new))

    def substr(
        self, expr: Expression[str] | Expression[str | None], start: int, length: int | None = None
    ) -> Func[str]:
        """The characters from the 1-based ``start`` (at least 1), ``length`` of them
        (at least 0; default: all)."""
        return Func("substr", (expr, _Int(start)) if length is None else (expr, _Int(start), _Int(length)))

    def strpos(
        self, expr: Expression[str] | Expression[str | None], part: str | Expression[str] | Expression[str | None]
    ) -> Func[int]:
        """The 1-based position of the first ``part`` in ``expr``, 0 if absent
        (``STRPOS``; ``INSTR`` on SQLite)."""
        return Func("strpos", (expr, part))

    def cardinality(self, expr: Expression[list[Any]] | Expression[list[Any] | None]) -> Func[int]:
        """The number of elements of an array."""
        return Func("cardinality", (expr,))

    @overload
    def unnest(self, expr: Expression[list[E]]) -> Func[E]: ...
    @overload
    def unnest(self, expr: Expression[list[E] | None]) -> Func[E]: ...
    def unnest(self, expr: Expression[Any]) -> Func[Any]:
        """One row for each element of an array. Only a ``select()`` column; PostgreSQL only."""
        return Func("unnest", (expr,))

    def abs(self, expr: Expression[T]) -> Func[T]:
        return Func("abs", (expr,))

    def coalesce(self, expr: Expression[T | None], default: T | Expression[T]) -> Func[T]:
        return Func("coalesce", (expr, default))

    def now(self) -> Func[datetime]:
        return Func("now")

    # Full-text search: PostgreSQL only. An optional first argument names the text search
    # configuration (``"english"``); without it the server's default applies.

    @overload
    def to_tsvector(self, document: str | Expression[str] | Expression[str | None], /) -> Func[TsVector]: ...
    @overload
    def to_tsvector(
        self, config: str, document: str | Expression[str] | Expression[str | None], /
    ) -> Func[TsVector]: ...
    def to_tsvector(self, a: Any, b: Any = None, /) -> Func[TsVector]:
        """``to_tsvector([config,] document)``: the document's normalized words. Index it
        with ``@@index([sql("to_tsvector('english', title)")], type: Gin)``."""
        return Func("to_tsvector", _search(a, b))

    @overload
    def to_tsquery(self, query: str | Expression[str], /) -> Func[TsQuery]: ...
    @overload
    def to_tsquery(self, config: str, query: str | Expression[str], /) -> Func[TsQuery]: ...
    def to_tsquery(self, a: Any, b: Any = None, /) -> Func[TsQuery]:
        """``to_tsquery([config,] query)``: a query in tsquery syntax (``"cat & !dog"``)."""
        return Func("to_tsquery", _search(a, b))

    @overload
    def plainto_tsquery(self, query: str | Expression[str], /) -> Func[TsQuery]: ...
    @overload
    def plainto_tsquery(self, config: str, query: str | Expression[str], /) -> Func[TsQuery]: ...
    def plainto_tsquery(self, a: Any, b: Any = None, /) -> Func[TsQuery]:
        """``plainto_tsquery([config,] text)``: every word of plain text."""
        return Func("plainto_tsquery", _search(a, b))

    @overload
    def websearch_to_tsquery(self, query: str | Expression[str], /) -> Func[TsQuery]: ...
    @overload
    def websearch_to_tsquery(self, config: str, query: str | Expression[str], /) -> Func[TsQuery]: ...
    def websearch_to_tsquery(self, a: Any, b: Any = None, /) -> Func[TsQuery]:
        """``websearch_to_tsquery([config,] text)``: search-engine syntax (``"cat -dog"``,
        ``"or"``, quoted phrases)."""
        return Func("websearch_to_tsquery", _search(a, b))

    def ts_rank(self, vector: Expression[TsVector], query: Expression[TsQuery]) -> Func[float]:
        """``ts_rank(vector, query)``: how well the document matches, for ``order_by``."""
        return Func("ts_rank", (vector, query))

    # Branches of expressions only first: mypy can't solve `T` from `Expression[T] | T`
    # when no plain value pins it.
    @overload
    def case(self, *whens: tuple[ConditionLike, Expression[T]], default: Expression[T] | T) -> Case[T]: ...
    @overload
    def case(self, *whens: tuple[ConditionLike, Expression[T]]) -> Case[T | None]: ...
    @overload
    def case(
        self, *whens: tuple[ConditionLike, Expression[T]] | tuple[ConditionLike, T], default: Expression[T] | T
    ) -> Case[T]: ...
    @overload
    def case(self, *whens: tuple[ConditionLike, Expression[T]] | tuple[ConditionLike, T]) -> Case[T | None]: ...
    def case(self, *whens: tuple[ConditionLike, Any], default: Any = None) -> Case[Any]:
        """``CASE WHEN cond THEN value ... ELSE default END``: the value of the first true
        condition, else ``default`` (``None`` when not given)::

            func.case((Post.views > 100, "hot"), (Post.views > 10, "warm"), default="cold")
            func.sum(func.case((Post.published, 1), default=0))
        """
        return Case(whens, default)

    # Window functions: only valid with .over(...).

    def row_number(self) -> Func[int]:
        """1, 2, 3, ... in the window's order."""
        return Func("row_number")

    def rank(self) -> Func[int]:
        """Rank with gaps: ties share a rank, the next rank skips (1, 1, 3)."""
        return Func("rank")

    def dense_rank(self) -> Func[int]:
        """Rank without gaps (1, 1, 2)."""
        return Func("dense_rank")

    def percent_rank(self) -> Func[float]:
        return Func("percent_rank")

    def cume_dist(self) -> Func[float]:
        return Func("cume_dist")

    def ntile(self, buckets: int) -> Func[int]:
        """The bucket (1..buckets) of the row when the window is split evenly."""
        return Func("ntile", (_Int(buckets),))

    def lag(self, expr: Expression[T], offset: int = 1, default: T | None = None) -> Func[T | None]:
        """``expr`` on the row ``offset`` rows before this one, ``default`` if none."""
        args: tuple[Any, ...] = (expr, _Int(offset)) if default is None else (expr, _Int(offset), default)
        return Func("lag", args)

    def lead(self, expr: Expression[T], offset: int = 1, default: T | None = None) -> Func[T | None]:
        """``expr`` on the row ``offset`` rows after this one, ``default`` if none."""
        args: tuple[Any, ...] = (expr, _Int(offset)) if default is None else (expr, _Int(offset), default)
        return Func("lead", args)

    def first_value(self, expr: Expression[T]) -> Func[T]:
        return Func("first_value", (expr,))

    def last_value(self, expr: Expression[T]) -> Func[T]:
        return Func("last_value", (expr,))

    def nth_value(self, expr: Expression[T], n: int) -> Func[T | None]:
        return Func("nth_value", (expr, _Int(n)))


func = _Functions()


def outer(column: ColumnRef[T]) -> Outer[T]:
    """A column of the enclosing query, inside a subquery (Django's ``OuterRef``)::

        await User.objects.filter(exists(Post.objects.filter(Post.author_id == outer(User.id))))

    It refers to the nearest enclosing query over the column's model. A path of to-one
    relations reads a related row: ``outer(Post.author.name)``.
    """
    return Outer(column)


def exists(query: QuerySet[Any] | Select[Unpack[tuple[Any, ...]]]) -> Condition:
    """``EXISTS (<query>)``, a condition: true when the query has a row. Correlate it
    with :func:`outer`; negate it with ``~``."""
    return Exists(query)


def excluded(column: ColumnRef[T]) -> Excluded[T]:
    """The row an upsert tried to insert: ``EXCLUDED.<column>`` in ``DO UPDATE``."""
    return Excluded(column)


class RelationPath(Generic[M]):
    """``User.posts``: a relation reached from ``root``. Attribute access continues the
    path: columns of the related model give :class:`ColumnRef`, relations give a
    longer :class:`RelationPath`. Used in filters and ``load()``; ``.objects`` gives the relation query set.
    """

    __slots__ = ("_root", "_path", "_target", "_rule", "_alias")
    _rule: NullRule | None

    def __init__(
        self,
        root: type[Model],
        path: tuple[str, ...],
        target: type[M],
        rule: NullRule | None = None,
        alias: tuple[Alias | None, ...] = (),
    ) -> None:
        self._root = root
        self._path = path
        self._target = target
        self._rule = rule
        self._alias = alias

    def alias(self) -> Self:
        """Another related row: every condition on one to-many path is about one related row for
        the whole query, and ``alias()`` names a different one. Keep the result in a variable to
        use one alias in several conditions::

            p = User.posts.alias()
            User.objects.filter(User.posts.views > 10, p.views < 3)   # a popular post and another, unpopular post
            User.objects.filter(p.title == "a", p.views < 3)          # one post: both hold for it

        It tags the last hop only: ``User.posts.alias().comments`` is another post,
        ``User.posts.comments.alias()`` is another comment of the same post. It works in
        filter conditions, not in aggregates, ``order_by()``, ``load()`` or ``outer()``."""
        kept = (*self._alias, *(None,) * (len(self._path) - 1 - len(self._alias)))[: len(self._path) - 1]
        return RelationPath(self._root, self._path, self._target, self._rule, (*kept, Alias()))  # type: ignore[return-value]

    def or_null(self) -> Self:
        """The null rule of this path: ``or_null()`` lets the missing related row, and a NULL in
        any nullable column reached through the path, pass. A method on one comparison wins:
        ``Post.author.or_null().email != 'x'`` keeps the posts without an author."""
        return RelationPath(self._root, self._path, self._target, "or_null", self._alias)  # type: ignore[return-value]

    def and_not_null(self) -> Self:
        """The null rule of this path: ``and_not_null()`` drops the missing related row, and a NULL
        in any nullable column reached through the path. A method on one comparison wins."""
        return RelationPath(self._root, self._path, self._target, "and_not_null", self._alias)  # type: ignore[return-value]

    # Hidden from type checkers: generated stubs declare each model's path class with
    # its real attributes, so unknown names are reported instead of typed as Any.
    if not TYPE_CHECKING:

        def __getattr__(self, name: str) -> Any:
            if name.startswith("_"):
                raise AttributeError(name)
            meta = self._target._meta
            if name in meta.nested:
                return meta.nested[name].__get__(None, self._target)._at(self._root, self._path, self._rule, self._alias)
            if name in meta.fields:
                return ColumnRef(self._root, self._path, meta.fields[name], self._rule, self._alias)
            if name in meta.relations:
                rel = meta.relations[name]
                return RelationPath(self._root, (*self._path, name), rel.target, self._rule, self._alias)
            raise AttributeError(f"{self._target.__name__} has no field or relation {name!r}")

    def __dir__(self) -> Iterable[str]:
        meta = self._target._meta
        return [*meta.public_names, *meta.relations, "objects"]

    @property
    def objects(self) -> QuerySet[M]:
        """The relation query set for :meth:`QuerySet.load`: ``self``'s target model's
        query set, bound to this relation (``User.posts.objects.filter(...)[:3]``)."""
        return self._target.objects._clone(_via=self)

    def __repr__(self) -> str:
        return ".".join((self._root.__name__, *self._path))

    # ``Post.author == alice`` compares the foreign key column ``Post.author_id``, so
    # the related table is never joined.

    def __eq__(self, other: M | None) -> Condition:  # type: ignore[override]
        return self._compare(other, neg=False)

    def __ne__(self, other: M | None) -> Condition:  # type: ignore[override]
        return self._compare(other, neg=True)

    __hash__ = object.__hash__

    def _compare(self, other: Any, *, neg: bool) -> Condition:
        from .fields import BelongsTo

        owner: type[Model] = self._root
        for name in self._path[:-1]:
            owner = owner._meta.relations[name].target
        rel = owner._meta.relations[self._path[-1]]
        if not isinstance(rel, BelongsTo):
            raise TypeError(
                f"{self!r} is a {type(rel).__name__} relation; only a BelongsTo relation "
                "compares with an instance (compare a key column instead)"
            )
        columns: list[ColumnRef[Any]] = [
            ColumnRef(self._root, self._path[:-1], owner._meta.fields[n], self._rule, self._alias[: len(self._path) - 1])
            for n in rel.via_fields
        ]
        if other is None:
            nulls: list[Condition] = [IsNull(c, neg=neg) for c in columns]
            return nulls[0] if len(nulls) == 1 else (or_ if neg else and_)(*nulls)
        if not isinstance(other, self._target):
            raise TypeError(f"{self!r} compares with a {self._target.__name__}, not {other!r}")
        keys = [other._field_value(n) for n in rel.to_fields]
        if any(k is None for k in keys):
            raise ValueError(f"{self!r} can't compare with {other!r}: its {', '.join(rel.to_fields)} is None")
        if len(columns) == 1:
            if not neg:
                return PathComparison("eq", columns[0], _wrap(keys[0]))
            different: Condition = Comparison("ne", columns[0], _wrap(keys[0]))
        else:
            match: list[Condition] = [Comparison("eq", c, _wrap(k)) for c, k in zip(columns, keys, strict=True)]
            if not neg:
                return PathMatch(match)
            different = Negated(Together(match))
        # `path != x` is `~(path == x)`: a NULL key stays, unless the path has its own null rule.
        nullable = [c for c in columns if c._nullable]
        if self._rule is not None or not nullable:
            return different
        return NullKept(different, [IsNull(c, neg=False) for c in nullable])

    __bool__ = _bool_misuse


class Ordering:
    __slots__ = ("expr", "desc", "nulls")

    def __init__(self, expr: Expression[Any], desc: bool, nulls: NullsPlace | None = None) -> None:
        if nulls not in (None, "first", "last"):
            raise ValueError(f"nulls is 'first' or 'last', not {nulls!r}")
        self.expr = expr
        self.desc = desc
        self.nulls = nulls

    def reversed(self) -> Ordering:
        """The opposite order, NULLs included."""
        nulls: NullsPlace | None = None if self.nulls is None else "last" if self.nulls == "first" else "first"
        return Ordering(self.expr, not self.desc, nulls)

    def _ir(self, ctx: IRContext) -> IR:
        ir: IR = {"expr": self.expr._ir(ctx), "desc": self.desc}
        if self.nulls is not None:
            ir["nulls"] = self.nulls
        return ir

    def __repr__(self) -> str:
        nulls = "" if self.nulls is None else f"nulls={self.nulls!r}"
        return f"{self.expr!r}.{'desc' if self.desc else 'asc'}({nulls})"


# -- conditions ---------------------------------------------------------------------------


class Condition(Node):
    """A boolean SQL condition. Combine with ``&``, ``|`` and ``~``."""

    __slots__ = ()

    def __and__(self, other: Condition | Expression[bool]) -> Condition:
        return and_(self, other)

    def __rand__(self, other: Expression[bool]) -> Condition:
        return and_(other, self)

    def __or__(self, other: Condition | Expression[bool]) -> Condition:
        return or_(self, other)

    def __ror__(self, other: Expression[bool]) -> Condition:
        return or_(other, self)

    def __invert__(self) -> Condition:
        return not_(self)

    def label(self, name: str) -> Labeled[bool]:
        """The condition as a named boolean column in ``select()``."""
        return Labeled(self, name)

    def or_null(self) -> Condition:
        """``(condition OR column IS NULL)``: a NULL row passes. Apply it to one comparison
        (``==``, ``!=``, ``<``, ``>``, ``in_``, ``like``, ``between``) of a nullable column,
        before ``~``: ``~(x > 20).or_null()`` is ``NOT (x > 20 OR x IS NULL)``."""
        return or_(self, *_null_tests(self._null_values("or_null"), keeps_null=True))

    def and_not_null(self) -> Condition:
        """``(condition AND column IS NOT NULL)``: a NULL row fails. Under ``~`` it keeps NULL
        rows: ``~(x > 20).and_not_null()`` is ``NOT (x > 20 AND x IS NOT NULL)``."""
        return and_(self, *_null_tests(self._null_values("and_not_null"), keeps_null=False))

    def _null_values(self, method: str) -> list[Node]:
        raise TypeError(
            f"{method}() applies to one comparison (==, !=, <, >, in_, like, between) of a column, "
            f"not to {self!r}; call it on the comparison itself, before you combine with & | ~"
        )


def _to_one(column: ColumnRef[Any]) -> bool:
    """Whether the column sits behind to-one hops only, counted from the last to-many hop (the
    engine's scope): there a missing related row reads as a NULL."""
    from .fields import BelongsTo, HasOne

    owner: type[Model] = column._root
    to_one = False
    for name in column._path:
        relation = owner._meta.relations[name]
        to_one = isinstance(relation, (BelongsTo, HasOne))
        owner = relation.target
    return to_one


_NEVER_NULL = frozenset({"count", "now", "row_number", "rank", "dense_rank", "percent_rank", "cume_dist", "ntile"})
_ALWAYS_NULLABLE = frozenset({"sum", "avg", "min", "max", "array_agg", "unnest", "lag", "lead", "first_value", "last_value", "nth_value"})
_NULL_PRESERVING = frozenset(
    {
        "lower", "upper", "length", "concat", "trim", "ltrim", "rtrim", "replace", "substr", "strpos", "cardinality",
        "abs", "to_tsvector", "to_tsquery", "plainto_tsquery", "websearch_to_tsquery", "ts_rank",
    }
)  # fmt: skip
_AGGREGATES = frozenset({"count", "sum", "avg", "min", "max", "array_agg"})


def can_be_null(node: Node) -> bool:
    """Whether the ORM cannot prove ``node`` non-null: the one rule behind the strict null rule
    and the null methods. A column is nullable when its field is. A function is non-null when it is
    ``count()``, ``now()`` or a ranking function, a ``coalesce`` with a non-null argument, or one
    of a closed list of null-preserving functions (``lower``, ``concat``, ``abs``, ...) of
    non-null arguments. Every other function is nullable, ``element`` (an array index, NULL out of
    range) and the aggregates included, and so are a scalar subquery, a JSON path, a ``case()``
    with no default, ``/`` and ``%`` (SQLite divides by zero to NULL) and anything the ORM does
    not know."""
    if isinstance(node, ColumnRef):
        return node._nullable
    if isinstance(node, Excluded):
        return can_be_null(node._column)
    if isinstance(node, Labeled):
        return can_be_null(node._inner)
    if isinstance(node, Window):
        return can_be_null(node._func)
    if isinstance(node, Outer):
        return can_be_null(node._column)
    if isinstance(node, Literal):
        return node.value is None
    if isinstance(node, (Param, _Bound, _Int, _Config)) or getattr(node, "_constant", False):
        return False
    if isinstance(node, Func):
        if node._name in _NEVER_NULL:
            return False
        if node._name in _ALWAYS_NULLABLE:
            return True
        if node._name == "coalesce":
            return all(can_be_null(a) for a in node._args)
        return node._name not in _NULL_PRESERVING or any(can_be_null(a) for a in node._args)
    if isinstance(node, Case):
        return node._default is None or any(can_be_null(v) for _, v in node._whens) or can_be_null(node._default)
    if isinstance(node, Arith):
        return node.op in ("div", "mod") or can_be_null(node.left) or can_be_null(node.right)
    return True


# The values the strict rule checks. Arithmetic and the other values stay non-strict:
# the methods apply to them and are never required.
_STRICT_VALUES = (ColumnRef, Func, Window, ScalarSubquery, Case, JsonPath, Outer, Excluded)


def _leaf_columns(node: Node) -> list[ColumnRef[Any]]:
    """The columns a value reads in the query's own scope: through functions, arithmetic,
    ``case()``, windows, labels and the conditions among them. An aggregate, a subquery and
    ``outer()`` read their own scope."""
    if isinstance(node, ColumnRef):
        return [node]
    children: Sequence[Node] = ()
    if isinstance(node, Func):
        children = () if node._name in _AGGREGATES else node._args
    elif isinstance(node, Window):
        children = (*node._func._args, *node._partition)
    elif isinstance(node, Case):
        children = [n for c, v in node._whens for n in (c, v)] + ([] if node._default is None else [node._default])
    elif isinstance(node, Arith | Comparison):
        children = (node.left, node.right)
    elif isinstance(node, Labeled):
        children = (node._inner,)
    elif isinstance(node, JsonPath):
        children = (node._item,)
    elif isinstance(node, In):
        children = (node.item, *node.values)
    elif isinstance(node, InSelect | Like | IsNull | Not):
        children = (node.item,)
    elif isinstance(node, BoolOp):
        children = node.items
    return [c for n in children for c in _leaf_columns(n)]


def _is_to_one_value(node: Node) -> bool:
    """Whether the value reads a column behind a to-one path: a missing related row reads as a
    NULL, so the value is nullable and ``!=`` / ``not_in`` of it flip as a to-one column does."""
    return any(_to_one(c) for c in _leaf_columns(node))


def _null_tests(values: Sequence[Node], *, keeps_null: bool) -> list[Condition]:
    """The NULL tests a null method adds for each value. Behind a to-one path the missing
    related row counts as a NULL, so `or_null()` adds `NOT EXISTS (... col IS NOT NULL)`."""
    out: list[Condition] = []
    for c in values:
        to_one = _is_to_one_value(c)
        if keeps_null:
            if to_one:
                out.append(Not(IsNull(c, neg=True)))
            elif can_be_null(c):
                out.append(IsNull(c, neg=False))
        elif can_be_null(c) or to_one:
            out.append(IsNull(c, neg=True))
    return out


def _nullable_values(method: str, nodes: Sequence[Node]) -> list[Node]:
    """The values among ``nodes`` that hold a NULL, once each; raises when none does.
    Behind a to-one path the missing related row is one."""
    out: dict[str, Node] = {}
    for node in nodes:
        if can_be_null(node) or _is_to_one_value(node):
            out.setdefault(_ckey(node), node)
    if not out:
        raise QueryError(
            f"{method}() on {', '.join(map(repr, nodes))}: the value is NOT NULL, so no row has a NULL to keep or drop"
        )
    return list(out.values())


def _either_side(method: str, owner: Condition, nodes: list[Node]) -> list[Node]:
    """The nullable values on any side of a comparison; a constant side adds none."""
    if not nodes:
        raise TypeError(f"{method}() applies to a comparison of a value, not to {owner!r}")
    return _nullable_values(method, nodes)


def _tested(leaf: Condition) -> list[Node]:
    """The operands a comparison tests, whatever their kind; empty for any other condition."""
    if isinstance(leaf, Comparison):
        return [leaf.left, leaf.right]
    if isinstance(leaf, (In, InSelect, Like)):
        return [leaf.item]
    if isinstance(leaf, Together):
        return [n for item in leaf.items for n in _tested(item)]
    if isinstance(leaf, Negated):
        return _tested(leaf.item)
    return []


def _strict_values(leaf: Condition, keep: Callable[[Node], bool]) -> list[Node]:
    """The values a comparison tests that satisfy ``keep``."""
    return [n for n in _tested(leaf) if keep(n)]


def _nullable_column(node: Node) -> bool:
    """A nullable column. A NOT NULL column behind a to-one path is not a nullable value: a
    negation keeps the rows with no related row."""
    return isinstance(node, ColumnRef) and node._nullable


def _checked_value(node: Node) -> bool:
    """A nullable value the strict rule checks: a column or a computed value (function,
    aggregate, window function, scalar subquery, ``case()``, JSON path, ``outer()``)."""
    return isinstance(node, _STRICT_VALUES) and can_be_null(node)


def _is_negative(leaf: Condition) -> bool:
    if isinstance(leaf, Comparison):
        return leaf.op == "ne"
    return isinstance(leaf, Negated) or (isinstance(leaf, (In, InSelect)) and leaf.neg)


def _ckey(e: object) -> str:
    return e._key if isinstance(e, ColumnRef) else repr(e)


def _null_siblings(items: Sequence[Condition], tests_not_null: bool) -> set[str]:
    """The columns that `items` test for NULL in the way the two null methods build it:
    ``col IS NOT NULL`` in an `&`, ``col IS NULL`` (or ``NOT col IS NOT NULL``) in a `|`."""
    out: set[str] = set()
    for i in items:
        if isinstance(i, IsNull) and i.neg == tests_not_null:
            out.add(_ckey(i.item))
        elif isinstance(i, Not) and isinstance(i.item, IsNull) and i.item.neg != tests_not_null:
            out.add(_ckey(i.item.item))
    return out


def resolve_null_rule(condition: Condition, *, negated: bool = False, strict: bool = True) -> Condition:
    """Apply the null rule to ``condition``; return the condition to run.

    A comparison on a nullable column that has neither ``or_null()`` nor ``and_not_null()`` and
    either sits under ``~`` or is a ``!=`` / ``not_in`` raises. A comparison counts as handled
    when a sibling ``IS NULL`` in an enclosing ``|``, or a sibling ``IS NOT NULL`` in an
    enclosing ``&``, is on its column: that is the shape the two methods build, and it stays
    correct under ``~``.

    A column that comes from a path with a null rule (``Post.author.or_null()``) gets the
    method of the path, unless its comparison is handled. A ``path == x`` of a nullable key
    under ``~`` keeps the rows with a NULL key. ``strict=False`` applies the path rules and never
    raises (``func.case()`` and ``select()`` values)."""

    def rule_of(leaf: Condition, neg: bool, handled: frozenset[str]) -> Condition:
        # The rule of a ruled column inside a computed value applies to the whole value.
        ruled: dict[tuple[str, str], tuple[Node, NullRule]] = {}
        for value in _tested(leaf):
            if _ckey(value) in handled:
                continue
            for c in _leaf_columns(value):
                if c._rule:
                    ruled.setdefault((_ckey(value), c._rule), (value, c._rule))
        ruled_leaf = leaf
        for value, rule in ruled.values():
            keeps_null = rule == "or_null"
            tests = _null_tests([value], keeps_null=keeps_null)
            if tests:
                ruled_leaf = (or_ if keeps_null else and_)(ruled_leaf, *tests)
        if ruled_leaf is not leaf:
            return ruled_leaf
        if neg and isinstance(leaf, (PathComparison, PathMatch)):
            keys = [c for c in _strict_values(leaf, _nullable_column) if _ckey(c) not in handled]
            if keys:
                return and_(leaf, *(IsNull(c, neg=True) for c in keys))
        return leaf

    def walk(node: Condition, neg: bool, handled: frozenset[str]) -> Condition:
        if isinstance(node, Not) and not isinstance(node, Negated):
            item = walk(node.item, True, handled)
            return node if item is node.item else Not(item)
        if isinstance(node, BoolOp) and not isinstance(node, PathMatch):
            inner = handled | _null_siblings(node.items, node.op == "and")
            items = [walk(i, neg, inner) for i in node.items]
            if all(a is b for a, b in zip(items, node.items, strict=True)):
                return node
            return Together(items) if isinstance(node, Together) else BoolOp(node.op, items)
        ruled = rule_of(node, neg, handled)
        if ruled is not node:
            return ruled
        for col in _strict_values(node, _checked_value) if strict else ():
            if _ckey(col) in handled:
                continue
            if neg:
                raise QueryError(
                    f"{node!r} sits under ~ / not_() / exclude() and {col!r} is nullable: add or_null() or "
                    "and_not_null() to the comparison. or_null() drops NULL rows under negation, "
                    "and_not_null() keeps them"
                )
            if _is_negative(node):
                raise QueryError(
                    f"{node!r} on the nullable {col!r} drops NULL rows silently: add or_null() or "
                    "and_not_null() to the comparison. or_null() keeps NULL rows, and_not_null() drops them"
                )
        return node

    return walk(condition, negated, frozenset())


class Comparison(Condition):
    __slots__ = ("op", "left", "right")

    def __init__(self, op: str, left: Node, right: Node) -> None:
        self.op = op
        self.left = left
        self.right = right

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "cmp", "op": self.op, "l": self.left._ir(ctx), "r": self.right._ir(ctx)}

    def _null_values(self, method: str) -> list[Node]:
        return _either_side(method, self, _tested(self))

    def __repr__(self) -> str:
        sym = {"eq": "==", "ne": "!=", "lt": "<", "le": "<=", "gt": ">", "ge": ">="}.get(self.op, self.op)
        return f"({self.left!r} {sym} {self.right!r})"


class PathComparison(Comparison):
    """``path == x`` of a single key: it keeps the rows with a NULL key under ``~``."""

    __slots__ = ()


class Arith(Expression[Any]):
    __slots__ = ("op", "left", "right")

    def __init__(self, op: str, left: Node, right: Node) -> None:
        self.op = op
        self.left = left
        self.right = right

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "arith", "op": self.op, "l": self.left._ir(ctx), "r": self.right._ir(ctx)}

    def __repr__(self) -> str:
        sym = {"add": "+", "sub": "-", "mul": "*", "div": "/", "concat": "||", "json_merge": "||"}[self.op]
        return f"({self.left!r} {sym} {self.right!r})"


class InSelect(Condition):
    __slots__ = ("item", "select", "neg")

    def __init__(self, item: Expression[Any], select: Any, neg: bool) -> None:
        self.item = item
        self.select = select
        self.neg = neg

    def _ir(self, ctx: IRContext) -> IR:
        return {
            "t": "in_select",
            "item": self.item._ir(ctx),
            "select": self.select._subquery_ir(ctx, "in_()"),
            "neg": self.neg,
        }

    def _null_values(self, method: str) -> list[Node]:
        return _nullable_values(method, [self.item])

    def __repr__(self) -> str:
        return f"{self.item!r} {'NOT IN' if self.neg else 'IN'} ({self.select!r})"


class Exists(Condition):
    __slots__ = ("query",)

    def __init__(self, query: Any) -> None:
        from .query import QuerySet
        from .select import Select

        if not isinstance(query, (QuerySet, Select)):
            raise TypeError(f"exists() takes a query set or a select(), got {query!r}")
        self.query = query

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "exists", "select": self.query._subquery_ir(ctx, "exists()")}

    def __repr__(self) -> str:
        return f"exists({self.query!r})"


class In(Condition):
    __slots__ = ("item", "values", "neg")

    def __init__(self, item: Node, values: list[Node], neg: bool) -> None:
        self.item = item
        self.values = values
        self.neg = neg

    def _ir(self, ctx: IRContext) -> IR:
        return {
            "t": "in",
            "item": self.item._ir(ctx),
            "values": [v._ir(ctx) for v in self.values],
            "neg": self.neg,
        }

    def _null_values(self, method: str) -> list[Node]:
        return _nullable_values(method, [self.item])

    def __repr__(self) -> str:
        return f"{self.item!r} {'NOT IN' if self.neg else 'IN'} ({', '.join(map(repr, self.values))})"


class IsNull(Condition):
    __slots__ = ("item", "neg")

    def __init__(self, item: Node, neg: bool) -> None:
        self.item = item
        self.neg = neg

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "is_null", "item": self.item._ir(ctx), "neg": self.neg}

    def _null_values(self, method: str) -> list[Node]:
        raise TypeError(f"{method}() does not apply to {self!r}: it is already a NULL test")

    def __repr__(self) -> str:
        return f"{self.item!r} IS {'NOT ' if self.neg else ''}NULL"


class Like(Condition):
    __slots__ = ("item", "pattern", "ci", "neg")

    def __init__(self, item: Node, pattern: Any, ci: bool, neg: bool = False) -> None:
        self.item = item
        self.pattern = pattern
        self.ci = ci
        self.neg = neg

    def _ir(self, ctx: IRContext) -> IR:
        return {
            "t": "like",
            "item": self.item._ir(ctx),
            "pattern": self.pattern._ir(ctx) if isinstance(self.pattern, Node) else ctx.param(self.pattern),
            "ci": self.ci,
            "neg": self.neg,
        }

    def _null_values(self, method: str) -> list[Node]:
        return _nullable_values(method, [self.item])

    def __repr__(self) -> str:
        return f"{self.item!r} {'NOT ' if self.neg else ''}{'ILIKE' if self.ci else 'LIKE'} {self.pattern!r}"


class Const(Condition):
    """A constant condition. ``in_([])`` and ``not_in([])`` keep the ``column`` they test, so
    ``or_null()`` and ``and_not_null()`` apply to them as to any other ``in_``."""

    __slots__ = ("value", "column")

    def __init__(self, value: bool, column: Node | None = None) -> None:
        self.value = value
        self.column = column

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "const", "value": self.value}

    def _null_values(self, method: str) -> list[Node]:
        if self.column is None:
            return super()._null_values(method)
        return _nullable_values(method, [self.column])

    def __repr__(self) -> str:
        return "TRUE" if self.value else "FALSE"


class BoolOp(Condition):
    __slots__ = ("op", "items")

    def __init__(self, op: str, items: list[Condition]) -> None:
        self.op = op
        self.items = items

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": self.op, "items": [i._ir(ctx) for i in self.items]}

    def __repr__(self) -> str:
        sym = " & " if self.op == "and" else " | "
        return "(" + sym.join(map(repr, self.items)) + ")"


class Together(BoolOp):
    """An AND that is one comparison: ``between`` and a composite key match. ``or_null()`` adds
    one ``IS NULL`` per nullable column it compares."""

    __slots__ = ()

    def __init__(self, items: list[Condition]) -> None:
        super().__init__("and", items)

    def _null_values(self, method: str) -> list[Node]:
        if not all(isinstance(item, Comparison) for item in self.items):
            return super()._null_values(method)
        return _either_side(method, self, _tested(self))


class PathMatch(Together):
    """``path == x`` of a composite key: it keeps the rows with a NULL key under ``~``."""

    __slots__ = ()


class NullKept(BoolOp):
    """``path != x`` of a nullable key: ``(key <> x OR key IS NULL)``, the form of ``~(path == x)``.
    ``or_null()`` is already there; ``and_not_null()`` drops the NULL keys again."""

    __slots__ = ("base",)

    def __init__(self, base: Condition, nulls: list[Condition]) -> None:
        super().__init__("or", [base, *nulls])
        self.base = base

    def or_null(self) -> Condition:
        return self

    def and_not_null(self) -> Condition:
        return self.base.and_not_null()


class Not(Condition):
    __slots__ = ("item",)

    def __init__(self, item: Condition) -> None:
        self.item = item

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "not", "item": self.item._ir(ctx)}

    def __repr__(self) -> str:
        return f"~{self.item!r}"


class Negated(Not):
    """The ``!=`` of a composite key: one comparison, so ``or_null()`` applies to it."""

    __slots__ = ()

    def _null_values(self, method: str) -> list[Node]:
        return self.item._null_values(method)

    def __repr__(self) -> str:
        pairs = [(i.left, i.right) for i in self.item.items if isinstance(i, Comparison)]  # type: ignore[attr-defined]
        return f"({', '.join(repr(a) for a, _ in pairs)}) != ({', '.join(repr(b) for _, b in pairs)})"


ConditionLike = Union[Condition, Expression[bool]]


def as_condition(value: ConditionLike) -> Condition:
    if isinstance(value, Condition):
        return value
    if isinstance(value, Expression):
        return Comparison("eq", value, Literal(True))
    raise TypeError(f"expected a condition such as `User.email == 'a@b.c'`, got {value!r}")


def _combine(op: str, items: tuple[ConditionLike, ...]) -> Condition:
    flat: list[Condition] = []
    for item in map(as_condition, items):
        if isinstance(item, BoolOp) and item.op == op:
            flat.extend(item.items)
        else:
            flat.append(item)
    if len(flat) == 1:
        return flat[0]
    return BoolOp(op, flat)


def and_(*conditions: ConditionLike) -> Condition:
    """All of ``conditions``. Same as joining them with ``&``."""
    return _combine("and", conditions) if conditions else Const(True)


def or_(*conditions: ConditionLike) -> Condition:
    """Any of ``conditions``. Same as joining them with ``|``."""
    return _combine("or", conditions) if conditions else Const(False)


def not_(condition: ConditionLike) -> Condition:
    return Not(resolve_null_rule(as_condition(condition), negated=True))


def differs(column: ColumnRef[Any], value: Any) -> Condition:
    """``column`` differs from ``value``, or from every item of a list; a NULL row of a nullable
    column counts as different, except from ``None``."""
    condition = column.not_in(value) if isinstance(value, list) else column != value
    if column._nullable and not isinstance(condition, IsNull):
        return condition.or_null()
    return condition
