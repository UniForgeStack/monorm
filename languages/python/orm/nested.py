"""Nested types: value classes, column references, and the conversions between values and rows.

A ``type`` block of the schema becomes an immutable value class. A model field of that type
is stored as one JSON column or as one column per leaf (``@storage``); the model attribute is
the same either way: a reference in queries (``Shop.address.city``) and a value object on an
instance.
"""

from __future__ import annotations

import datetime as dt
import decimal
import enum
import json
import uuid
from collections.abc import Callable, Iterable, Iterator, Mapping, Sequence
from typing import TYPE_CHECKING, Any, ClassVar

from .errors import DecodeError, NotLoaded
from .expr import ColumnRef, Comparison, Condition, IsNull, Node
from .fields import BY_TYPE, Enum, Json

if TYPE_CHECKING:
    from .expr import IR, IRContext
    from .model import Model
    from .select import Select

_MISSING: Any = object()
# What a leaf of each stored type accepts from the caller (bool is never an int).
_ACCEPTS: dict[str, tuple[type, ...]] = {
    "int": (int,),
    "big_int": (int,),
    "float": (int, float),
    "bool": (bool,),
    "string": (str,),
    "text": (str,),
    "date_time": (dt.datetime,),
    "date": (dt.date,),
    "uuid": (uuid.UUID, str),
    "decimal": (decimal.Decimal, int, str),
}


class _Sub:
    """One subfield of a nested type."""

    __slots__ = ("name", "nullable", "list", "nested", "type", "enum", "default", "dynamic_default")

    def __init__(self, ir: dict[str, Any]) -> None:
        leaf = ir.get("leaf") or {}
        self.name: str = ir["name"]
        self.nullable: bool = ir["nullable"]
        self.list: bool = ir.get("list", False)
        self.nested: str | None = ir.get("type")
        self.type: str | None = leaf.get("type")
        self.enum: str | None = leaf.get("enum")
        self.default: Any = leaf.get("default", _MISSING)
        # The engine or the database fills this default (a clock, a SQL expression).
        self.dynamic_default: bool = bool(leaf.get("default_now") or leaf.get("default_sql") or leaf.get("client_default"))


class NestedType:
    """The shape of one ``type`` block, with the conversions between its representations."""

    def __init__(self, ir: dict[str, Any], types: dict[str, NestedType], enums: Mapping[str, type[enum.Enum]]) -> None:
        self.name: str = ir["name"]
        self.subs: tuple[_Sub, ...] = tuple(_Sub(f) for f in ir["fields"])
        self.by_name = {s.name: s for s in self.subs}
        self.types = types
        self.enums = enums
        self.cls: type[NestedValue] = None  # type: ignore[assignment]

    def inner(self, sub: _Sub) -> NestedType:
        assert sub.nested is not None
        return self.types[sub.nested]

    def at(self, path: Sequence[str]) -> NestedType:
        """The type reached through the nested subfields ``path``."""
        node = self
        for k in path:
            node = node.inner(node.by_name[k])
        return node

    def required(self, rel: Sequence[str]) -> bool:
        """Whether the leaf at ``rel`` must be set whenever the type is set."""
        sub = self.by_name[rel[0]]
        if sub.nested is None:
            return not sub.nullable
        return not sub.nullable and not sub.list and self.inner(sub).required(rel[1:])

    def leaf_paths(self, prefix: tuple[str, ...] = ()) -> Iterator[tuple[str, ...]]:
        for s in self.subs:
            if s.nested is None:
                yield (*prefix, s.name)
            else:
                yield from self.inner(s).leaf_paths((*prefix, s.name))

    # Values ------------------------------------------------------------------------------

    def given(self, value: Any, what: str) -> dict[str, Any]:
        """The subfields a value or a mapping names, checked against the type."""
        if isinstance(value, NestedValue):
            if value._type is not self:
                raise TypeError(f"{what}: expected {self.name}, got {value._type.name}")
            return {s.name: getattr(value, s.name) for s in self.subs}
        if not isinstance(value, Mapping):
            raise TypeError(f"{what}: expected {self.name} or a dict, got {type(value).__name__}")
        unknown = sorted(str(k) for k in value if k not in self.by_name)
        if unknown:
            raise TypeError(f"{what}: {self.name} has no subfield {', '.join(unknown)}")
        return dict(value)

    def leaf_value(self, sub: _Sub, value: Any, what: str) -> Any:
        """A caller's leaf value, checked and left as it is."""
        if value is None:
            if not sub.nullable:
                raise ValueError(f"{what}: is not nullable")
            return None
        if isinstance(value, Node):
            return value
        accepts = _ACCEPTS.get(sub.type or "")
        if sub.enum is not None:
            members = self.enums.get(sub.enum)
            if members is not None and not isinstance(value, members):
                try:
                    value = members(value)
                except ValueError:
                    raise TypeError(f"{what}: {value!r} is not a value of {sub.enum}") from None
        elif accepts is not None and (not isinstance(value, accepts) or (isinstance(value, bool) and bool not in accepts)):
            raise TypeError(f"{what}: expected {sub.type}, got {type(value).__name__}")
        return value

    def leaf_list(self, sub: _Sub, value: Any, what: str) -> Any:
        """A list leaf as one list parameter, each element checked like a plain leaf."""
        if value is None:
            return self.leaf_value(sub, value, what)
        if isinstance(value, (str, bytes)) or not isinstance(value, Sequence):
            raise TypeError(f"{what}: expected a list, got {type(value).__name__}")
        return [self.leaf_value(sub, v, f"{what}[{i}]") for i, v in enumerate(value)]

    def bound(self, sub: _Sub, value: Any, what: str, json: bool) -> Any:
        """A leaf value as a query parameter; inside a JSON value the engine binds a DateTime or Date as text."""
        v = self.leaf_value(sub, value, what)
        if not json or v is None or isinstance(v, Node):
            return v
        if isinstance(v, enum.Enum):
            return v.value
        if sub.type in ("date_time", "date"):
            return self._json_leaf(sub, v, what)
        return v

    def coerce(self, sub: _Sub, value: Any, what: str) -> Any:
        """The value a value class holds for ``sub``."""
        if value is None:
            if not sub.nullable:
                raise ValueError(f"{what}: is not nullable")
            return None
        if sub.list:
            if isinstance(value, (str, bytes)) or not isinstance(value, Sequence):
                raise TypeError(f"{what}: expected a list, got {type(value).__name__}")
            return tuple(self._one(sub, v, f"{what}[{i}]") for i, v in enumerate(value))
        return self._one(sub, value, what)

    def _one(self, sub: _Sub, value: Any, what: str) -> Any:
        if sub.nested is None:
            return self.leaf_value(sub, value, what)
        inner = self.inner(sub)
        if isinstance(value, inner.cls):
            return value
        return inner.cls(**inner.given(value, what))

    # JSON --------------------------------------------------------------------------------

    def to_json(self, value: Any, what: str, *, whole: bool = True) -> dict[str, Any]:
        """The JSON object of a value (or a dict that names every required subfield)."""
        given = self.given(value, what)
        out: dict[str, Any] = {}
        for s in self.subs:
            at = f"{what}.{s.name}"
            if s.name not in given:
                if whole and not s.nullable and s.default is _MISSING:
                    raise ValueError(f"{at}: is required")
                if whole and s.default is not _MISSING and not s.nullable:
                    out[s.name] = self._json_leaf(s, s.default, at)
                continue
            out[s.name] = self._json_sub(s, given[s.name], at)
        return out

    def _json_sub(self, s: _Sub, v: Any, at: str) -> Any:
        if v is None:
            if not s.nullable:
                raise ValueError(f"{at}: is not nullable")
            return None
        if s.list:
            if isinstance(v, (str, bytes)) or not isinstance(v, Sequence):
                raise TypeError(f"{at}: expected a list, got {type(v).__name__}")
            return [self._json_one(s, x, f"{at}[{i}]") for i, x in enumerate(v)]
        return self._json_one(s, v, at)

    def _json_one(self, s: _Sub, v: Any, at: str) -> Any:
        if s.nested is not None:
            return self.inner(s).to_json(v, at)
        return self._json_leaf(s, v, at)

    def _json_leaf(self, s: _Sub, v: Any, at: str) -> Any:
        if v is None:
            return None
        v = self.leaf_value(s, v, at)
        if isinstance(v, Node):
            raise TypeError(f"{at}: an expression can't go into a whole JSON value")
        if isinstance(v, enum.Enum):
            return v.value
        match s.type:
            case "big_int":
                return str(v)
            case "decimal":
                return str(v)
            case "date_time":
                if v.tzinfo is None:
                    raise ValueError(f"{at}: a datetime needs a timezone")
                return v.astimezone(dt.UTC).strftime("%Y-%m-%dT%H:%M:%S.%fZ")
            case "date":
                return (v.date() if isinstance(v, dt.datetime) else v).isoformat()
            case "uuid":
                return str(v)
            case "float":
                return float(v)
        return v

    def from_json(self, raw: Any, model: str, field: str, path: tuple[str, ...] = ()) -> NestedValue:
        """The value of a stored JSON object; ``DecodeError`` says where it differs from the type."""
        if not isinstance(raw, dict):
            raise DecodeError(model, field, path, f"expected an object, got {_kind(raw)}")
        values: dict[str, Any] = {}
        for s in self.subs:
            at = (*path, s.name)
            v = raw.get(s.name)
            if v is None:
                if not s.nullable:
                    raise DecodeError(model, field, at, "is null or missing but not nullable")
                values[s.name] = None
            elif s.list:
                if not isinstance(v, list):
                    raise DecodeError(model, field, at, f"expected a list, got {_kind(v)}")
                values[s.name] = tuple(self._decode_one(s, x, model, field, (*at, i)) for i, x in enumerate(v))
            else:
                values[s.name] = self._decode_one(s, v, model, field, at)
        return self.cls._make(values)

    def _decode_one(self, s: _Sub, v: Any, model: str, field: str, at: tuple[Any, ...]) -> Any:
        if s.nested is not None:
            if v is None:
                return None
            return self.inner(s).from_json(v, model, field, at)
        return self._decode_leaf(s, v, model, field, at)

    def _decode_leaf(self, s: _Sub, v: Any, model: str, field: str, at: tuple[Any, ...]) -> Any:
        def bad(expected: str) -> DecodeError:
            return DecodeError(model, field, at, f"expected {expected}, got {_kind(v)} {v!r}"[:200])

        t = s.type
        try:
            if s.enum is not None:
                members = self.enums.get(s.enum)
                if not isinstance(v, (str, int)) or isinstance(v, bool):
                    raise bad(f"a value of {s.enum}")
                if members is None:
                    return v
                try:
                    return members(v)
                except ValueError:
                    raise bad(f"a value of {s.enum}") from None
            if t in ("string", "text"):
                if not isinstance(v, str):
                    raise bad("a string")
                return v
            if t == "int":
                if isinstance(v, bool) or not isinstance(v, int):
                    raise bad("an integer")
                return v
            if t == "big_int":
                if isinstance(v, bool) or not isinstance(v, (int, str)):
                    raise bad("an integer or integer text")
                return int(v)
            if t == "float":
                if isinstance(v, bool) or not isinstance(v, (int, float)):
                    raise bad("a number")
                return float(v)
            if t == "bool":
                if not isinstance(v, bool):
                    raise bad("a boolean")
                return v
            if t == "decimal":
                if isinstance(v, decimal.Decimal):
                    return v
                if isinstance(v, bool) or not isinstance(v, (int, str, float)):
                    raise bad("decimal text")
                return decimal.Decimal(v if not isinstance(v, float) else repr(v))
            if t == "date_time":
                if isinstance(v, dt.datetime):
                    return v.replace(tzinfo=dt.UTC) if v.tzinfo is None else v.astimezone(dt.UTC)
                if not isinstance(v, str):
                    raise bad("a datetime string")
                out = dt.datetime.fromisoformat(v)
                return out.replace(tzinfo=dt.UTC) if out.tzinfo is None else out.astimezone(dt.UTC)
            if t == "date":
                if isinstance(v, dt.date) and not isinstance(v, dt.datetime):
                    return v
                if not isinstance(v, str):
                    raise bad("a date string")
                return dt.date.fromisoformat(v)
            if t == "uuid":
                if isinstance(v, uuid.UUID):
                    return v
                if not isinstance(v, str):
                    raise bad("a uuid string")
                return uuid.UUID(v)
        except (ValueError, decimal.InvalidOperation) as e:
            if isinstance(e, DecodeError):
                raise
            raise bad(f"a valid {t}") from None
        return v

    # Columns -----------------------------------------------------------------------------

    def collapse(self, cells: Mapping[tuple[str, ...], Any], prefix: tuple[str, ...] = (), *, nullable: bool = False) -> NestedValue | None:
        """The value of leaf cells keyed by path; ``None`` when the group is null."""
        if nullable and self._is_null(cells, prefix):
            return None
        values: dict[str, Any] = {}
        for s in self.subs:
            at = (*prefix, s.name)
            if s.nested is not None:
                values[s.name] = self.inner(s).collapse(cells, at, nullable=s.nullable)
            else:
                values[s.name] = cells[at]
        return self.cls._make(values)

    def _is_null(self, cells: Mapping[tuple[str, ...], Any], prefix: tuple[str, ...]) -> bool:
        under = [p for p in cells if p[: len(prefix)] == prefix]
        # The first required leaf is null exactly when the group is: a set group holds it.
        for p in under:
            if self._required_at(prefix, p):
                return cells[p] is None
        return all(cells[p] is None for p in under)

    def _required_at(self, prefix: tuple[str, ...], path: tuple[str, ...]) -> bool:
        return self.required(path[len(prefix):])


def _kind(v: Any) -> str:
    return "null" if v is None else type(v).__name__


class NestedValue:
    """Base class of the generated value classes: immutable, compared by value."""

    __slots__ = ()
    _type: ClassVar[NestedType]

    def __init__(self, **values: Any) -> None:
        t = type(self)._type
        given = t.given(values, t.name)
        out: dict[str, Any] = {}
        for s in t.subs:
            at = f"{t.name}.{s.name}"
            if s.name in given:
                out[s.name] = t.coerce(s, given[s.name], at)
            elif s.nullable:
                out[s.name] = None
            elif s.default is not _MISSING:
                out[s.name] = s.default
            else:
                raise TypeError(f"{at}: is required")
        for k, v in out.items():
            object.__setattr__(self, k, v)

    @classmethod
    def _make(cls, values: dict[str, Any]) -> NestedValue:
        self = object.__new__(cls)
        for k, v in values.items():
            object.__setattr__(self, k, v)
        return self

    if not TYPE_CHECKING:  # type checkers see the stub's read-only properties instead

        def __setattr__(self, name: str, value: Any) -> None:
            raise AttributeError(f"{type(self).__name__} values are immutable")

        def __delattr__(self, name: str) -> None:
            raise AttributeError(f"{type(self).__name__} values are immutable")

    def _items(self) -> tuple[tuple[str, Any], ...]:
        return tuple((s.name, getattr(self, s.name)) for s in type(self)._type.subs)

    def __eq__(self, other: object) -> bool:
        return type(other) is type(self) and self._items() == other._items()

    def __hash__(self) -> int:
        return hash((type(self), self._items()))

    def to_dict(self) -> dict[str, Any]:
        """The subfields by name, nested values as dicts."""

        def plain(v: Any) -> Any:
            if isinstance(v, NestedValue):
                return v.to_dict()
            if isinstance(v, tuple):
                return [plain(x) for x in v]
            return v

        return {k: plain(v) for k, v in self._items()}

    def __repr__(self) -> str:
        return f"{type(self).__name__}({', '.join(f'{k}={v!r}' for k, v in self._items())})"

    def __copy__(self) -> Any:
        return self

    def __deepcopy__(self, memo: dict[int, Any]) -> Any:
        return self

    def __reduce__(self) -> Any:
        return (_unpickle, (type(self), dict(self._items())))


def _unpickle(cls: type[NestedValue], values: dict[str, Any]) -> Any:
    return cls._make(values)


def build_types(
    irs: Sequence[dict[str, Any]], enums: Mapping[str, type[enum.Enum]], module: str | None
) -> dict[str, NestedType]:
    """The nested types of a schema, each with its value class."""
    types: dict[str, NestedType] = {}
    for ir in irs:
        t = NestedType(ir, types, enums)
        types[t.name] = t
        ns: dict[str, Any] = {"__slots__": tuple(s.name for s in t.subs), "_type": t, "__match_args__": ()}
        if module is not None:
            ns["__module__"] = module
        t.cls = type(t.name, (NestedValue,), ns)
    return types


# Model attributes -----------------------------------------------------------------------


class _ObjectNode(Node):
    """A value of a nested type in a query: ``Expr::Object``."""

    __slots__ = ("nt", "value", "what", "json")

    def __init__(self, nt: NestedType, value: Any, what: str, json: bool) -> None:
        self.nt = nt
        self.value = value
        self.what = what
        self.json = json

    def _ir(self, ctx: IRContext) -> IR:
        return object_ir(self.nt, self.value, ctx, self.what, self.json)

    def __repr__(self) -> str:
        return repr(self.value)


_NULL: IR = {"t": "null"}


def _node(ctx: IRContext, v: Any) -> IR:
    return v._ir(ctx) if isinstance(v, Node) else ctx.param(v)


def object_ir(nt: NestedType, value: Any, ctx: IRContext, what: str, json: bool) -> IR:
    """``Expr::Object`` of a value (every subfield) or of a dict (the subfields it names)."""
    given = nt.given(value, what)
    fields: list[IR] = []
    for s in nt.subs:
        if s.name not in given:
            continue
        v = given[s.name]
        at = f"{what}.{s.name}"
        if v is None:
            if not s.nullable:
                raise ValueError(f"{at}: is not nullable")
            node = _NULL
        elif s.nested is not None and not s.list:
            node = object_ir(nt.inner(s), v, ctx, at, json)
        elif s.list:
            node = ctx.param(nt._json_sub(s, v, at))
        else:
            node = _node(ctx, nt.bound(s, v, at, json))
        fields.append({"name": s.name, "value": node})
    return {"t": "object", "fields": fields}


class _Access:
    """What ``Shop.address`` is on the class: a reference; on an instance the value is in ``__dict__``."""

    name: str
    model: type[Model]
    nt: NestedType
    nullable: bool
    storage: str
    primary_key = False
    unique = False
    index = False
    type_name = "nested"

    def __get__(self, obj: object | None, owner: type[Any]) -> Any:
        if obj is None:
            return NestedRef(owner, (), self, ())
        raise NotLoaded(f"{owner.__name__}.{self.name} was not loaded")

    def leaf_field(self, sub: tuple[str, ...]) -> Any:
        """A field object standing for the leaf at ``sub``, for the places that read a column's name and type."""
        cache: dict[tuple[str, ...], Any] = self.__dict__.setdefault("_leaf_fields", {})
        if sub not in cache:
            node = self.nt.at(sub[:-1])
            s = node.by_name[sub[-1]]
            f = Enum(s.enum, stored=s.type or "string", nullable=s.nullable) if s.enum else BY_TYPE[s.type or "json"](nullable=s.nullable)
            f.name = f"{self.name}_{'_'.join(sub)}"
            f.column = f.name
            f.model = self.model
            cache[sub] = f
        return cache[sub]

    # Writes ------------------------------------------------------------------------------

    def insert_values(self, value: Any) -> dict[str, Any]:
        """The engine fields an insert sets for ``value`` (a value, a dict naming every required subfield, or None)."""
        raise NotImplementedError

    def assignments(self, value: Any, ctx: IRContext) -> list[dict[str, Any]]:
        what = f"{self.model.__name__}.{self.name}"
        if value is None:
            if not self.nullable:
                raise ValueError(f"{what}: is not nullable")
            return [{"field": self.name, "sub": [], "value": _NULL}]
        if isinstance(value, Node):
            return [{"field": self.name, "sub": [], "value": value._ir(ctx)}]
        if isinstance(value, NestedValue):
            return [{"field": self.name, "sub": [], "value": object_ir(self.nt, value, ctx, what, self.storage == "json")}]
        if not isinstance(value, Mapping):
            raise TypeError(f"{what}: expected {self.nt.name} or a dict, got {type(value).__name__}")
        if not value:
            raise ValueError(f"{what}: a patch needs at least one subfield")
        return list(self._patch(self.nt, self.nt.given(value, what), (), ctx, what))

    def _patch(self, nt: NestedType, given: dict[str, Any], prefix: tuple[str, ...], ctx: IRContext, what: str) -> Iterator[dict[str, Any]]:
        for key, v in given.items():
            s = nt.by_name[key]
            path = (*prefix, key)
            at = f"{what}.{'.'.join(path)}"
            if s.list:
                raise TypeError(f"{at}: a list subfield can't be patched; assign the whole {self.name}")
            if v is None:
                if not s.nullable:
                    raise ValueError(f"{at}: is not nullable")
                node = _NULL
            elif s.nested is not None:
                inner = nt.inner(s)
                if isinstance(v, Mapping):
                    if not v:
                        raise ValueError(f"{at}: a patch needs at least one subfield")
                    yield from self._patch(inner, inner.given(v, at), path, ctx, what)
                    continue
                node = v._ir(ctx) if isinstance(v, Node) else object_ir(inner, v, ctx, at, self.storage == "json")
            else:
                node = _node(ctx, nt.bound(s, v, at, self.storage == "json"))
            yield {"field": self.name, "sub": list(path), "value": node}

    # Rows --------------------------------------------------------------------------------

    def decode_json(self, raw: Any, sub: tuple[str, ...] = ()) -> Any:
        """The value of a JSON cell the engine assembled for this field (or the subfield group at ``sub``)."""
        if raw is None:
            return None
        if isinstance(raw, (str, bytes)):
            raw = json.loads(raw)
        node = self.nt.at(sub)
        return node.from_json(raw, self.model.__name__, self.name, sub)


class NestedGroup(_Access):
    """``@storage(columns)``: the field has no column of its own; its leaves have."""

    def __init__(self, name: str, nt: NestedType, nullable: bool, leaves: tuple[tuple[str, tuple[str, ...]], ...]) -> None:
        self.name = name
        self.nt = nt
        self.nullable = nullable
        self.storage = "columns"
        self.leaves = leaves

    def __set_name__(self, owner: type[Model], name: str) -> None:
        self.model = owner

    def insert_values(self, value: Any) -> dict[str, Any]:
        what = f"{self.model.__name__}.{self.name}"
        names = dict((p, n) for n, p in self.leaves)
        out: dict[str, Any] = {}
        if value is None:
            if not self.nullable:
                raise ValueError(f"{what}: is not nullable")
            return dict.fromkeys(names.values())
        self._flatten(self.nt, self.nt.given(value, what), (), names, out, what)
        return out

    def _flatten(self, nt: NestedType, given: dict[str, Any], prefix: tuple[str, ...], names: dict[tuple[str, ...], str], out: dict[str, Any], what: str) -> None:
        for s in nt.subs:
            path = (*prefix, s.name)
            at = f"{what}.{'.'.join(path)}"
            if s.name in given:
                v = given[s.name]
            elif s.nullable:
                v = None
            elif s.default is not _MISSING:
                v = s.default
            elif s.dynamic_default:
                continue
            else:
                raise ValueError(f"{at}: is required")
            if s.nested is None:
                out[names[path]] = nt.leaf_list(s, v, at) if s.list else nt.leaf_value(s, v, at)
            elif v is None:
                if not s.nullable:
                    raise ValueError(f"{at}: is not nullable")
                for p, n in names.items():
                    if p[: len(path)] == path:
                        out[n] = None
            else:
                inner = nt.inner(s)
                self._flatten(inner, inner.given(v, at), path, names, out, what)

    def collapse(self, *values: Any) -> Any:
        cells = {p: v for (_, p), v in zip(self.leaves, values, strict=True)}
        return self.nt.collapse(cells, nullable=self.nullable)

    @property
    def leaf_names(self) -> tuple[str, ...]:
        return tuple(n for n, _ in self.leaves)

    def decoder(self) -> tuple[str, tuple[str, ...], Callable[..., Any]]:
        return (self.name, self.leaf_names, self.collapse)


class NestedJson(Json[Any], _Access):
    """``@storage(json)``: one JSON column holds the value."""

    def __init__(self, ir: dict[str, Any], nt: NestedType, **kwargs: Any) -> None:
        Json.__init__(self, **kwargs)
        self._schema = ir
        self.nt = nt
        self.storage = "json"

    def ir(self) -> dict[str, Any]:
        return self._schema

    def __get__(self, obj: object | None, owner: type[Any]) -> Any:
        return _Access.__get__(self, obj, owner)

    def insert_values(self, value: Any) -> dict[str, Any]:
        what = f"{self.model.__name__}.{self.name}"
        if value is None:
            if not self.nullable:
                raise ValueError(f"{what}: is not nullable")
            return {self.name: None}
        return {self.name: self.nt.to_json(value, what)}

    def decoder(self) -> tuple[str, tuple[str, ...], Callable[..., Any]]:
        return (self.name, (self.name,), self.decode_json)


class NestedRef(ColumnRef[Any]):
    """``Shop.address``, ``Shop.address.geo`` or ``Shop.address.geo.lat``."""

    __slots__ = ("_group", "_sub")

    def __init__(self, root: type[Model], path: tuple[str, ...], group: _Access, sub: tuple[str, ...]) -> None:
        field = group.leaf_field(sub) if sub and _is_leaf(group.nt, sub) else group
        super().__init__(root, path, field)  # type: ignore[arg-type]
        self._group = group
        self._sub = sub

    @property
    def _whole(self) -> bool:
        """Whether this is a nested value, not a scalar."""
        return not self._sub or not _is_leaf(self._group.nt, self._sub)

    def _type_here(self) -> NestedType:
        node = self._group.nt.at(self._sub)
        return node

    def _ir(self, ctx: IRContext) -> IR:
        ColumnRef._ir(self, ctx)  # checks the root
        return {"t": "col", "path": list(self._path), "name": self._group.name, "sub": list(self._sub)}

    def _decode(self, raw: Any) -> Any:
        """The Python value of a selected cell."""
        if self._whole:
            return self._group.decode_json(raw, self._sub)
        if raw is None or self._group.storage != "json":
            return raw
        nt = self._group.nt.at(self._sub[:-1])
        leaf = nt.by_name[self._sub[-1]]
        model, name = self._group.model.__name__, self._group.name
        if leaf.list and isinstance(raw, list):
            return tuple(nt._decode_leaf(leaf, x, model, name, (*self._sub, i)) for i, x in enumerate(raw))
        return nt._decode_leaf(leaf, raw, model, name, self._sub)

    def _at(self, root: type[Model], path: tuple[str, ...]) -> NestedRef:
        """The same reference reached from ``root`` through a relation ``path``."""
        return NestedRef(root, path, self._group, self._sub)

    def _select_name(self) -> str:
        return "_".join((self._group.name, *self._sub))

    def _load_names(self) -> list[str]:
        """The stored field names a ``load()`` of this reference reads: the whole group, since an instance holds no part of one."""
        group = self._group
        if isinstance(group, NestedGroup):
            return [name for name, _ in group.leaves]
        return [group.name]

    if not TYPE_CHECKING:  # type checkers see the stub's subfield attributes instead

        def __getattr__(self, name: str) -> Any:
            if name.startswith("_") or not self._whole:
                raise AttributeError(name)
            nt = self._type_here()
            if name not in nt.by_name:
                raise AttributeError(f"{nt.name} has no subfield {name!r}; it has {', '.join(nt.by_name)}")
            return NestedRef(self._root, self._path, self._group, (*self._sub, name))

    def __getitem__(self, name: Any) -> Any:
        if not isinstance(name, str) or not self._whole or name not in self._type_here().by_name:
            raise KeyError(name)
        return NestedRef(self._root, self._path, self._group, (*self._sub, name))

    def __eq__(self, other: Any) -> Condition:  # type: ignore[override]
        if self._whole and other is not None and not isinstance(other, Node):
            return Comparison("eq", self, _ObjectNode(self._type_here(), other, repr(self), self._group.storage == "json"))
        if other is None:
            return IsNull(self, neg=False)
        return ColumnRef.__eq__(self, self._conv(other))

    def __ne__(self, other: Any) -> Condition:  # type: ignore[override]
        if self._whole and other is not None and not isinstance(other, Node):
            return Comparison("ne", self, _ObjectNode(self._type_here(), other, repr(self), self._group.storage == "json"))
        if other is None:
            return IsNull(self, neg=True)
        return ColumnRef.__ne__(self, self._conv(other))

    def _conv(self, value: Any) -> Any:
        """A value to compare with a JSON leaf, as the engine binds it (a DateTime or Date as text)."""
        if value is None or isinstance(value, Node) or self._whole or self._group.storage != "json":
            return value
        nt = self._group.nt.at(self._sub[:-1])
        return nt.bound(nt.by_name[self._sub[-1]], value, repr(self), True)

    def __lt__(self, other: Any) -> Condition:
        return ColumnRef.__lt__(self, self._conv(other))

    def __le__(self, other: Any) -> Condition:
        return ColumnRef.__le__(self, self._conv(other))

    def __gt__(self, other: Any) -> Condition:
        return ColumnRef.__gt__(self, self._conv(other))

    def __ge__(self, other: Any) -> Condition:
        return ColumnRef.__ge__(self, self._conv(other))

    def _convs(self, values: Iterable[Any] | Select[Any]) -> Iterable[Any] | Select[Any]:
        return values if isinstance(values, Node) or not isinstance(values, Iterable) else [self._conv(v) for v in values]

    def in_(self, values: Iterable[Any] | Select[Any]) -> Condition:
        return ColumnRef.in_(self, self._convs(values))

    def not_in(self, values: Iterable[Any] | Select[Any]) -> Condition:
        return ColumnRef.not_in(self, self._convs(values))

    __hash__ = object.__hash__

    def __repr__(self) -> str:
        return ".".join((self._root.__name__, *self._path, self._group.name, *self._sub))


def _is_leaf(nt: NestedType, sub: tuple[str, ...]) -> bool:
    node = nt.at(sub[:-1])
    return node.by_name[sub[-1]].nested is None


def leaf_names(model_ir: dict[str, Any]) -> dict[str, list[tuple[str, tuple[str, ...]]]]:
    """The leaf fields of each ``columns`` group of a model, in row order."""
    out: dict[str, list[tuple[str, tuple[str, ...]]]] = {}
    for f in model_ir["fields"]:
        n = f.get("nested")
        if n and n["storage"] == "columns":
            out.setdefault(n["field"], []).append((f["name"], tuple(n["path"])))
    return out
