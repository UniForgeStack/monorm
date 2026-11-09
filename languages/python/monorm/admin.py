"""The admin service: read-only resources over the models, called in memory by ``test_client``.

The admin is in progress: this module registers resources and runs list requests without a
server. ::

    from monorm import admin

    admin.register(Post, {"list": {"columns": [Post.title, Post.author.name], "order": [Post.created_at.desc()]}})
    admin.check()  # raises AdminConfigError for an unknown field or a list column through a to-many relation
    client = admin.test_client(db, user={"is_superuser": True})
    page = (await client.get("/admin/api/resources/Post")).json()

With no ``register`` call every concrete model is a read-only resource.
"""

from __future__ import annotations

import datetime
import json
import math
import os
import uuid
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from json import dumps as _dumps
from typing import TYPE_CHECKING, Any, Literal, Mapping, Sequence, TypedDict, cast

from . import _native
from . import _admin_permissions
from ._admin_host import HOST as _HOST
from ._admin_permissions import RoleSeedReport, SyncReport
from .errors import QueryError
from .expr import ColumnRef, Condition, Expression, IRContext, Ordering, RelationPath, SlotParams, _Slot, as_condition, resolve_null_rule
from .model import Model, registry as default_registry

if TYPE_CHECKING:
    from .db import Database
    from .model import Registry

__all__ = ["Admin", "AdminConfigError", "AdminResponse", "AdminTestClient", "AggregateColumn", "BrandConfig", "BrandLink", "ComputedColumn", "ListConfig", "NamedChoice", "NamedFilter", "ResourceConfig", "RoleSeedReport", "SyncReport", "check", "register", "test_client", "url"]

DEFAULT_PREFIX = "/admin/api"


class AdminConfigError(ValueError):
    """The admin config names an unknown field, a list column through a to-many relation, or a bad value."""


class NamedChoice(TypedDict, total=False):
    name: str
    label: str
    condition: Any
    """A condition such as ``Note.rank > 1``. It has no ``monorm.param``: the choice is a fixed condition."""


class NamedFilter(TypedDict, total=False):
    name: str
    """The name in the URL (``n.<name>=<choice>``). The choice ``all`` is reserved: it applies no condition."""
    label: str
    choices: Sequence[NamedChoice]
    default: str
    """The choice that applies when the URL names none. Without it: all rows."""


class ComputedColumn(TypedDict, total=False):
    name: str
    """The column name: list it in ``columns`` as a string."""
    label: str
    type: str
    """``int``, ``bigint``, ``float``, ``bool``, ``string``, ``text``, ``datetime``, ``date``, ``uuid``, ``json`` or ``decimal``."""
    nullable: bool
    expr: Any
    """An expression that the list statement computes, such as ``monorm.func.lower(Note.title)``. It has no ``monorm.param``."""
    function: Callable[[list[Any]], Awaitable[Sequence[Any]]]
    """Instead of ``expr``: ``async def f(keys)`` gets the keys of one page (one call per page) and returns one value per key."""
    sortable: bool
    """A sort header. It needs ``expr`` and ``nullable=False``."""


class AggregateColumn(TypedDict, total=False):
    name: str
    label: str
    relation: Any
    """A to-many relation of the model, such as ``Author.comments``."""
    fn: Literal["count", "sum", "min", "max", "avg"]
    field: Any
    """A field of the related model, such as ``Author.comments.rank``: every function but ``count`` needs it."""


class ListConfig(TypedDict, total=False):
    columns: Sequence[ColumnRef[Any] | str]
    """Fields of the model; a string names a computed or aggregate column."""
    filters: Sequence[ColumnRef[Any] | RelationPath[Any]]
    """The filter bar: a field, a field of a to-one relation, or a to-one relation (its key is the value)."""
    named_filters: Sequence[NamedFilter]
    computed: Sequence[ComputedColumn]
    aggregates: Sequence[AggregateColumn]
    labels: Mapping[ColumnRef[Any] | str, str]
    """The title of a column."""
    empty: str
    """What an empty value shows in the list."""
    empty_fields: Mapping[ColumnRef[Any] | str, str]
    """What an empty value shows in one column."""
    tabs: Sequence[RelationPath[Any]]
    """The to-many relations that the detail page shows as tabs. Without it: every one that leads to a resource."""
    order: Sequence[ColumnRef[Any] | Ordering]
    page_size: int
    """Rows per page, 1 to 200 (25 by default)."""
    sortable: Sequence[ColumnRef[Any]]
    """The list columns that a sort header may use. Without it every list column that can sort."""
    search: Sequence[ColumnRef[Any]]
    """Fields of the model that the search box matches. The strategy comes from the indexes of each field."""
    search_with: Any
    """Replaces ``search``. A prepared query set with ``monorm.param("q")`` (its rows are the matches), or
    ``async def f(q: str, limit: int) -> Sequence[key]``. One host call per request."""


class BrandLink(TypedDict):
    label: str
    url: str


class BrandConfig(TypedDict, total=False):
    name: str
    """The name in the header and the page title (``Admin`` by default)."""
    logo: str
    """The URL of the logo image."""
    colors: Mapping[Literal["primary", "accent"], str]
    """``#rgb`` or ``#rrggbb``."""
    links: Sequence[BrandLink]
    """Links in the header."""
    environment: str
    """A short label such as ``staging``, shown in the header."""


class SectionConfig(TypedDict, total=False):
    title: str
    fields: Sequence[str]
    collapsed: bool
    """A collapsed section starts closed. It needs a ``title``."""


class ResourceConfig(TypedDict, total=False):
    list: ListConfig
    title: str
    """The name in the navigation and the page heading (the model name, spaced, by default)."""
    group: str
    """The navigation group: one of ``groups`` of the admin."""
    allow: Sequence[Literal["read", "create", "update", "delete"]]
    """The operations that the resource allows for every user, superusers too."""
    perms: Sequence[Literal["read", "manage", "delete"]]
    """The ``page`` strategy only: add ``"delete"`` to split the delete permission from ``manage``."""
    label: str
    """The label of the permission names of this resource, instead of the site label."""
    field_permissions: Sequence[str]
    """Names such as ``"see_credential"`` that become ``<label>.<name>``."""
    actions: Sequence[str]
    """Names such as ``"refund"`` that become ``<label>.<action>_<resource>``."""
    queryset: Any
    """The rows that staff see: a prepared query set (``Model.objects.filter(Model.owner == param("user")).prepare()``) whose
    ``user`` param the admin fills, or an async function ``(user)`` that returns a condition (one host call
    for each request). Its filter limits the list, count, search, detail and tabs; its ``order`` is the list order when the list sets none."""
    can: Mapping[Literal["change", "delete"], Any]
    """Conditions that a row must meet for the user to change or delete it. The list and the detail select them as flags."""
    check_messages: Mapping[str, str]
    """The form message of each named ``@@check``, by the name of the check. A failed check without an entry shows a generic message."""
    form: Sequence[SectionConfig] | Mapping[Literal["create", "edit"], Sequence[SectionConfig]]
    """The sections of the form: one list for both forms, or ``{"create": [...], "edit": [...]}``. A field that no section names
    trails in an untitled section. A name that is not a field of the form is an error at the start of the site."""
    readonly: Sequence[str]
    """Fields that the form shows but never writes, in the create and the edit form. A write to one of them is a 422."""
    widgets: Mapping[str, str]
    """The widget of a field (``"blurb": "textarea"``), of a nested path (``"Geo.lat"``) or of a type (``"Text"``, an enum name).
    A field beats a path, a path beats a type; the site ``widgets`` come last."""
    form_component: str | os.PathLike[str]
    """The path of a JavaScript module that replaces the whole form: ``export default (React) => function Form(props) {...}``.
    The admin reads the file when the site starts and serves it to signed-in staff."""
    relations: Mapping[str, RelationConfig]
    """The relation fields that the form edits, by relation name. A to-one relation becomes a picker with keyset autocomplete;
    a many-to-many relation (through a join model) becomes chips. The target of each needs a text column that names a row."""
    files: Mapping[str, str]
    """File fields by field name, each with the name of its storage in the ``storage`` registry of the admin
    (``{"cover": "media"}``). A file field is a nullable ``Json`` field that holds the reference of the file."""


class RelationConfig(TypedDict, total=False):
    label: str
    """The text column of the target that names a row in the picker (the first text column by default)."""
    create: bool
    """Offer a modal in the form that creates a record of the target and fills the field."""
    choices: Any
    """A prepared query set of the target (``Author.objects.filter(Author.active == True).prepare()``) that limits the picker.
    The server checks it again on write. It combines with the ``queryset`` of the target."""


def _widgets(name: str | None, value: Any) -> dict[str, str]:
    who = f"{name}: widgets" if name else "widgets"
    if not isinstance(value, Mapping) or not all(isinstance(k, str) and isinstance(v, str) for k, v in value.items()):
        raise AdminConfigError(f"{who} maps a field, a path or a type to a widget name")
    return dict(value)


def _section(name: str, s: Any) -> dict[str, Any]:
    if not isinstance(s, Mapping) or not isinstance(s.get("fields"), (list, tuple)) or not all(isinstance(f, str) for f in s["fields"]):
        raise AdminConfigError(f"{name}: a form section is {{'title': ..., 'fields': [...]}}")
    if unknown := sorted(set(s) - {"title", "fields", "collapsed"}):
        raise AdminConfigError(f"{name}: unknown section option {unknown[0]!r}")
    out: dict[str, Any] = {"fields": list(s["fields"])}
    if "title" in s:
        out["title"] = s["title"]
    if "collapsed" in s:
        out["collapsed"] = bool(s["collapsed"])
    return out


def _form(name: str, form: Any) -> Any:
    if isinstance(form, Mapping):
        for which, sections in form.items():
            if not isinstance(sections, (list, tuple)):
                raise AdminConfigError(f"{name}: form {which} is a list of sections")
        return {which: [_section(name, s) for s in sections] for which, sections in form.items()}
    if isinstance(form, (list, tuple)):
        return [_section(name, s) for s in form]
    raise AdminConfigError(f"{name}: form is a list of sections, or a mapping with create and edit")


_LIST_KEYS = {"columns", "order", "page_size", "sortable", "search", "search_with", "filters", "named_filters", "computed", "aggregates", "labels", "empty", "empty_fields", "tabs"}
_RESOURCE_KEYS = {"list", "title", "group", "allow", "perms", "label", "field_permissions", "actions", "queryset", "can", "check_messages", "form", "readonly", "widgets", "form_component", "relations", "files"}
_BRAND_KEYS = {"name", "logo", "colors", "links", "environment"}
Strategy = Literal["page", "model", "access"]


def _label(model: type[Model], ref: ColumnRef[Any]) -> str:
    if ref._root is not model:
        raise AdminConfigError(f"{ref!r} belongs to {ref._root.__name__}, not to {model.__name__}")
    return ".".join((*ref._path, ref._field.name))


def _name(model: type[Model], ref: Any) -> str:
    """A column name: a string (a computed or aggregate column), a column or a relation of ``model``."""
    if isinstance(ref, str):
        return ref
    if isinstance(ref, RelationPath):
        if ref._root is not model:
            raise AdminConfigError(f"{ref!r} belongs to {ref._root.__name__}, not to {model.__name__}")
        return ".".join(ref._path)
    if not isinstance(ref, ColumnRef):
        raise AdminConfigError(f"{model.__name__}: {ref!r} is not a column or a relation")
    return _label(model, ref)


def _ir(model: type[Model], node: Any, what: str) -> Any:
    """The engine IR of an expression or condition. The admin keeps no parameters: a literal goes into the IR itself."""
    params: list[Any] = []
    if isinstance(node, Condition):
        node = resolve_null_rule(node)
    elif not isinstance(node, Expression):
        node = as_condition(node)
    try:
        ir = node._ir(IRContext(model, params))
    except QueryError as e:
        raise AdminConfigError(f"{model.__name__}: {what}: {e}") from None
    return _inline(ir, params, f"{model.__name__}: {what}")


def _inline(ir: Any, params: list[Any], what: str) -> Any:
    if isinstance(ir, list):
        return [_inline(item, params, what) for item in ir]
    if not isinstance(ir, dict):
        return ir
    if ir.get("t") == "param":
        value = params[ir["i"]]
        if value is None:
            return {"t": "null"}
        if isinstance(value, bool):
            return {"t": "const", "value": value}
        if isinstance(value, int):
            return {"t": "int", "value": value}
        if isinstance(value, str):
            return {"t": "text", "value": value}
        raise AdminConfigError(f"{what}: a value of type {type(value).__name__} is not a literal of the admin; use an integer, a string or a boolean")
    return {k: _inline(v, params, what) for k, v in ir.items()}


def _batch_name(model: type[Model], column: str) -> str:
    return f"column.{model.__name__}.{column}"


def _computed(model: type[Model], items: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    out = []
    for item in items:
        entry = {k: v for k, v in item.items() if k not in ("expr", "function")}
        if (item.get("expr") is None) == (item.get("function") is None):
            raise AdminConfigError(f"{model.__name__}: computed {item.get('name')!r} sets expr or function, one of them")
        if item.get("expr") is not None:
            entry["expr"] = _ir(model, item["expr"], f"computed {item.get('name')!r}")
        else:
            entry["function"] = _batch_name(model, str(item["name"]))
        out.append(entry)
    return out


def _aggregates(model: type[Model], items: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    out = []
    for item in items:
        entry: dict[str, Any] = {k: v for k, v in item.items() if k not in ("relation", "fn", "field")}
        relation = item["relation"]
        if not isinstance(relation, RelationPath):
            raise AdminConfigError(f"{model.__name__}: aggregate {item.get('name')!r}: relation takes a relation such as {model.__name__}.<relation>")
        entry["relation"] = _name(model, relation)
        entry["fn"] = item["fn"]
        if item.get("field") is not None:
            field = item["field"]
            if not isinstance(field, ColumnRef) or field._root is not model or field._path != relation._path:
                raise AdminConfigError(f"{model.__name__}: aggregate {item.get('name')!r}: field takes a field of the relation {entry['relation']!r}")
            entry["field"] = field._field.name
        out.append(entry)
    return out


def _named(model: type[Model], items: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    out = []
    for item in items:
        entry = {k: v for k, v in item.items() if k != "choices"}
        entry["choices"] = [
            {**{k: v for k, v in c.items() if k != "condition"}, "expr": _ir(model, c["condition"], f"named filter {item.get('name')!r}")}
            for c in item["choices"]
        ]
        out.append(entry)
    return out


def _list(model: type[Model], config: Mapping[str, Any]) -> dict[str, Any]:
    if unknown := sorted(set(config) - _LIST_KEYS):
        raise AdminConfigError(f"{model.__name__}: unknown list option {unknown[0]!r}")
    out: dict[str, Any] = {}
    if "columns" in config:
        out["columns"] = [_name(model, c) for c in config["columns"]]
    for key in ("filters", "tabs"):
        if key in config:
            out[key] = [_name(model, c) for c in config[key]]
    if "named_filters" in config:
        out["named_filters"] = _named(model, config["named_filters"])
    if "computed" in config:
        out["computed"] = _computed(model, config["computed"])
    if "aggregates" in config:
        out["aggregates"] = _aggregates(model, config["aggregates"])
    for key in ("labels", "empty_fields"):
        if key in config:
            out[key] = {_name(model, k): v for k, v in config[key].items()}
    if "empty" in config:
        out["empty"] = config["empty"]
    if "order" in config:
        order = []
        for item in config["order"]:
            if isinstance(item, Ordering):
                if not isinstance(item.expr, ColumnRef):
                    raise AdminConfigError(f"{model.__name__}: order takes columns, not {item.expr!r}")
                if item.nulls is not None:
                    raise AdminConfigError(f"{model.__name__}: order does not take a NULLs placement")
                order.append(("-" if item.desc else "") + _label(model, item.expr))
            else:
                order.append(_label(model, item))
        out["order"] = order
    if "page_size" in config:
        out["page_size"] = config["page_size"]
    for key in ("sortable", "search"):
        if key in config:
            out[key] = [_label(model, c) for c in config[key]]
    if "search_with" in config:
        out["search_with"] = _search_name(model)
    return out


def _search_name(model: type[Model]) -> str:
    return f"search.{model.__name__}"


def _queryset_name(model: type[Model]) -> str:
    return f"queryset.{model.__name__}"


_SLOTS = ("user",)


def _slot_ir(model: type[Model], ir: Any, params: list[Any], slots: list[str]) -> Any:
    """The IR of a queryset filter: a ``param("user")`` becomes slot ``i`` and a literal goes inline."""
    if isinstance(ir, list):
        return [_slot_ir(model, item, params, slots) for item in ir]
    if not isinstance(ir, dict):
        return ir
    if ir.get("t") == "param":
        value = params[ir["i"]]
        if not isinstance(value, _Slot):
            return _inline(ir, params, f"{model.__name__}: queryset")
        if value.name not in _SLOTS or value.transform is not None:
            raise AdminConfigError(f"{model.__name__}: queryset takes the params {', '.join(repr(n) for n in _SLOTS)}, not {value!r}")
        if value.name not in slots:
            slots.append(value.name)
        return {"t": "param", "i": slots.index(value.name)}
    return {k: _slot_ir(model, v, params, slots) for k, v in ir.items()}


def _queryset(model: type[Model], config: Any) -> tuple[dict[str, Any], list[str] | None]:
    """The queryset of a resource as config JSON, and the order it sets (column names) or None."""
    from .query import Prepared

    if callable(config) and not isinstance(config, Prepared):
        return {"function": _queryset_name(model)}, None
    if not isinstance(config, Prepared):
        raise AdminConfigError(f"{model.__name__}: queryset takes a prepared query set (Model.objects.filter(...).prepare()) or an async function")
    qs = config._qs
    if qs.model is not model:
        raise AdminConfigError(f"{model.__name__}: queryset is a query set of {qs.model.__name__}")
    for what, used in (
        ("load()", qs._loads or qs._model_fields is not None or qs._related or qs._prefetch),
        ("a slice", qs._limit is not None or qs._offset is not None),
        ("from_() or join()", qs._from is not None or qs._joins),
        ("lock()", qs._lock is not None),
        ("without_defaults()", qs._without_defaults),
    ):
        if used:
            raise AdminConfigError(f"{model.__name__}: queryset takes a filter and an order; it does not take {what}")
    if not qs._filters:
        raise AdminConfigError(f"{model.__name__}: queryset needs a filter")
    params = SlotParams()
    ctx = IRContext(model, params)
    try:
        filters = [resolve_null_rule(f)._ir(ctx) for f in qs._filters]
    except QueryError as e:
        raise AdminConfigError(f"{model.__name__}: queryset: {e}") from None
    slots: list[str] = []
    irs = [_slot_ir(model, f, params, slots) for f in filters]
    filter_ir = irs[0] if len(irs) == 1 else {"t": "and", "items": irs}
    order = [("-" if o.desc else "") + _label(model, o.expr) for o in qs._order if isinstance(o.expr, ColumnRef)]
    if len(order) != len(qs._order):
        raise AdminConfigError(f"{model.__name__}: queryset order takes columns")
    return {"filter": filter_ir, "slots": slots}, order or None


def _queryset_host(model: type[Model], function: Callable[..., Awaitable[Any]]) -> Callable[[Any, dict[str, Any]], Awaitable[Any]]:
    """The host function of a callable queryset: the filter for this user, with its values inline."""

    async def run(database: Any, args: dict[str, Any]) -> Any:
        found = await function(args["user"])
        conditions = [found] if isinstance(found, (Condition, Expression)) else list(found or ())
        if not conditions:
            raise AdminConfigError(f"{model.__name__}: the queryset function returns a condition")
        irs = [_ir(model, c, "queryset") for c in conditions]
        return {"filter": irs[0] if len(irs) == 1 else {"t": "and", "items": irs}}

    return run


def _is_prepared(value: Any) -> bool:
    from .query import Prepared

    return isinstance(value, Prepared)


def _can(model: type[Model], rules: Mapping[Any, Any]) -> dict[str, Any]:
    return {name: _ir(model, rule, f"can {name!r}") for name, rule in rules.items()}


def _key_cells(model: type[Model], row: Any) -> list[Any]:
    return [_key_value(getattr(row, name)) for name in model._meta.key_names]


def _search_host(model: type[Model], search: Any) -> Callable[[Any, dict[str, Any]], Awaitable[Any]]:
    """The host function of a ``search_with``: the keys of the matches, at most ``limit``."""
    from .query import Prepared

    async def run(database: Any, args: dict[str, Any]) -> Any:
        limit = int(args["limit"])
        if isinstance(search, Prepared):
            rows = await search(q=args["q"])
            return [_key_cells(model, row) for row in rows[:limit]]
        found = await search(args["q"], limit)
        return [_key_json(k) for k in found][:limit]

    return run


def _batch_host(model: type[Model], function: Callable[[list[Any]], Awaitable[Sequence[Any]]]) -> Callable[[Any, dict[str, Any]], Awaitable[Any]]:
    """The host function of a computed column: one call with the keys of the page, one value per key."""
    single = len(model._meta.key_names) == 1

    async def run(database: Any, args: dict[str, Any]) -> Any:
        keys = [cells[0] if single else cells for cells in args["keys"]]
        return [_cell_json(v) for v in await function(keys)]

    return run


def _cell_json(value: Any) -> Any:
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, (list, tuple)):
        return [_cell_json(v) for v in value]
    if isinstance(value, Mapping):
        return {str(k): _cell_json(v) for k, v in value.items()}
    return value.isoformat() if isinstance(value, (datetime.date, datetime.time)) else str(value)


def _key_value(value: Any) -> Any:
    if isinstance(value, (str, int, float, bool)):
        return value
    # `str(datetime)` has a space where the detail route wants `T`.
    return value.isoformat() if isinstance(value, (datetime.date, datetime.time)) else str(value)


def _key_json(key: Any) -> Any:
    if isinstance(key, Mapping):
        return {str(k): _key_value(v) for k, v in key.items()}
    if isinstance(key, (list, tuple)):
        return [_key_value(v) for v in key]
    return _key_value(key)


_RELATION_KEYS = {"label", "create", "choices"}


def _relations(name: str, model: type[Model], value: Any) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise AdminConfigError(f"{name}: relations maps a relation name to its options")
    out: dict[str, Any] = {}
    for rel_name, options in value.items():
        relation = model._meta.relations.get(rel_name) if isinstance(rel_name, str) else None
        if relation is None:
            raise AdminConfigError(f"{name}: relations has {rel_name!r}, which is not a relation of the model")
        if not isinstance(options, Mapping):
            raise AdminConfigError(f"{name}: relations[{rel_name!r}] is a mapping of options")
        if unknown := sorted(set(options) - _RELATION_KEYS):
            raise AdminConfigError(f"{name}: relations[{rel_name!r}] has an unknown option {unknown[0]!r}")
        entry: dict[str, Any] = {}
        if "label" in options:
            if not isinstance(options["label"], str):
                raise AdminConfigError(f"{name}: relations[{rel_name!r}].label is a column name")
            entry["label"] = options["label"]
        if "create" in options:
            if not isinstance(options["create"], bool):
                raise AdminConfigError(f"{name}: relations[{rel_name!r}].create is True or False")
            entry["create"] = options["create"]
        if "choices" in options:
            from .query import Prepared

            if not isinstance(options["choices"], Prepared):
                raise AdminConfigError(f"{name}: relations[{rel_name!r}].choices takes a prepared query set of the target")
            entry["choices"], _ = _queryset(relation.target, options["choices"])
        out[rel_name] = entry
    return out


def _files(name: str, value: Any) -> dict[str, str]:
    if not isinstance(value, Mapping) or not all(isinstance(k, str) and isinstance(v, str) and v for k, v in value.items()):
        raise AdminConfigError(f"{name}: files maps a field name to a storage name")
    return dict(value)


def _file_host(storage: Any, files: Mapping[tuple[str, str], str]) -> dict[str, Callable[[Any, dict[str, Any]], Awaitable[Any]]]:
    """The host functions ``file.store`` and ``file.fetch``: the core hands over a temporary file, the registry moves the bytes."""
    from monorm_file_storage import Reference

    async def store(database: Any, args: dict[str, Any]) -> Any:
        name = files.get((args["model"], args["field"]))
        if name is None:
            raise AdminConfigError(f"{args['model']}.{args['field']} is not a file field")
        with open(args["path"], "rb") as source:
            reference = await storage.upload(name, source, filename=args["filename"], content_type=args["content_type"], max_size=args["max_size"])
        return reference.to_dict()

    async def fetch(database: Any, args: dict[str, Any]) -> Any:
        reference = Reference.from_dict(args["reference"])
        with open(args["path"], "wb") as out:
            async for chunk in storage.open(reference):
                out.write(chunk)
        return None

    return {"file.store": store, "file.fetch": fetch}


def _multipart(file: tuple[str, bytes | str, str]) -> tuple[str, bytes]:
    name, data, kind = file
    boundary = f"----monorm{uuid.uuid4().hex}"
    body = f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{name}"\r\nContent-Type: {kind}\r\n\r\n'.encode()
    body += data.encode() if isinstance(data, str) else data
    body += f"\r\n--{boundary}--\r\n".encode()
    return f"multipart/form-data; boundary={boundary}", body


@dataclass(frozen=True)
class AdminResponse:
    """The answer to one in-memory request."""

    status: int
    text: str
    queries: int = 0
    """Statements the engine ran for this request (counted on the in-memory ``user=`` path only)."""
    headers: tuple[tuple[str, str], ...] = ()
    """The response headers, every ``set-cookie`` kept (login mode only)."""
    sql: tuple[str, ...] = ()
    """The SQL of those statements, with placeholders (the ``user=`` path only)."""
    content: bytes = b""
    """The body as bytes: a file download is not text. Login mode keeps only the UTF-8 text."""

    def json(self) -> Any:
        return json.loads(self.text)


_SESSION_COOKIE = "__Host-monorm-session"
_CSRF_COOKIE = "__Host-monorm-csrf"
_AUTO: Any = object()


class AdminTestClient:
    """Sends requests to the admin router in memory: no server, no port.

    Two modes:

    - ``user=`` is a plain value that the client trusts, with no login. Its shape is a JSON object:
      ``{"is_superuser": True}`` reads every resource (``allow=`` still limits it), no user is a 401, and any other user
      gets what its grants allow: a user with no grant is a 403, and a ``{"key": ...}`` user has its grants loaded.
    - ``secret=`` runs the served router with login: the guard, the signed cookie, CSRF, the Origin check,
      the rate limit and the security headers. The client keeps a cookie jar. ``login`` signs in; a write
      sends the CSRF token of the jar and the Origin of ``http://localhost`` unless ``csrf=`` or ``origin=``
      says otherwise (``None`` leaves the header out). ``ip`` is the address that the rate limit sees.
    """

    def __init__(
        self,
        site: _native.AdminSite,
        database: Database,
        user: Mapping[str, Any] | None,
        *,
        secret: str | None = None,
        settings: Mapping[str, Any] | None = None,
        prefix: str = DEFAULT_PREFIX,
        ip: str = "127.0.0.1",
        host: Mapping[str, Any] | None = None,
    ) -> None:
        if secret is not None and user is not None:
            raise ValueError("test_client takes `user` (no login) or `secret` (login), not both")
        if secret is None and settings is not None:
            raise ValueError("test_client takes `settings` only with `secret`")
        self._site = site
        self._host = dict(host) if host is not None else dict(_HOST)
        self._database = database
        self._user = None if user is None else json.dumps(user)
        self._secret = secret
        self._options = json.dumps(dict(settings or {}))
        self._prefix = prefix
        self._router: _native.AdminRouter | None = None
        self.ip = ip
        self.cookies: dict[str, str] = {}
        """The cookie jar: copy it to another client to share a session."""

    async def request(
        self,
        method: str,
        path: str,
        *,
        json: Any = None,
        headers: Mapping[str, str] | None = None,
        csrf: str | None = _AUTO,
        origin: str | None = _AUTO,
        file: tuple[str, bytes | str, str] | None = None,
    ) -> AdminResponse:
        """``file=(filename, data, content_type)`` sends a multipart upload. Login mode sends text files only (UTF-8)."""
        if file is not None and json is not None:
            raise TypeError("a request takes `json` or `file`, not both")
        if self._secret is None:
            if headers or csrf is not _AUTO or origin is not _AUTO:
                raise TypeError("the `user=` client takes only a method, a path, `json` and `file`")
            trace = _native.Trace()
            if file is not None or "/files/" in path:
                kind, data = _multipart(file) if file is not None else (None, None)
                code, reply, raw = await self._site.request_raw(self._database._engine, method, path, self._user, trace, self._database, self._host, kind, data)
                events = trace.take()
                return AdminResponse(code, raw.decode("utf-8", "replace"), len(events), tuple(reply), tuple(e[0] for e in events), raw)
            sent_json = None if json is None else _dumps(json)
            status, body = await self._site.request(self._database._engine, method, path, self._user, trace, self._database, self._host, sent_json)
            events = trace.take()
            return AdminResponse(status, body, len(events), sql=tuple(e[0] for e in events), content=body.encode())
        router = self._router
        if router is None:
            router = self._router = self._site.client(self._database._engine, self._database, self._host, self._secret, self._options)
        sent: dict[str, str] = {"host": "localhost", **{k.lower(): v for k, v in (headers or {}).items()}}
        if self.cookies:
            sent["cookie"] = "; ".join(f"{k}={v}" for k, v in self.cookies.items())
        if method not in ("GET", "HEAD", "OPTIONS"):
            token = self.cookies.get(_CSRF_COOKIE, "") if csrf is _AUTO else csrf
            if token is not None:
                sent["x-csrf-token"] = token
            origin_value = "http://localhost" if origin is _AUTO else origin
            if origin_value is not None:
                sent["origin"] = origin_value
        body_text = None if json is None else _dumps(json)
        if body_text is not None:
            sent.setdefault("content-type", "application/json")
        if file is not None:
            kind, data = _multipart(file)
            try:
                body_text = data.decode("utf-8")
            except UnicodeDecodeError:
                raise ValueError("the login-mode client sends text files only (UTF-8); use `user=` for binary data") from None
            sent.setdefault("content-type", kind)
        status, reply_headers, text = await router.request(method, path, list(sent.items()), body_text, self.ip)
        for name, value in reply_headers:
            if name.lower() == "set-cookie":
                pair = value.split(";", 1)[0]
                key, _, val = pair.partition("=")
                if "Max-Age=0" in value or not val:
                    self.cookies.pop(key, None)
                else:
                    self.cookies[key] = val
        return AdminResponse(status, text, 0, tuple(reply_headers), content=text.encode())

    async def get(self, path: str, **kwargs: Any) -> AdminResponse:
        return await self.request("GET", path, **kwargs)

    async def post(self, path: str, **kwargs: Any) -> AdminResponse:
        return await self.request("POST", path, **kwargs)

    async def patch(self, path: str, **kwargs: Any) -> AdminResponse:
        return await self.request("PATCH", path, **kwargs)

    async def delete(self, path: str, **kwargs: Any) -> AdminResponse:
        return await self.request("DELETE", path, **kwargs)

    async def login(self, username: str, password: str) -> AdminResponse:
        """Signs in: reads the CSRF token, then posts the credentials. The jar keeps the session cookie."""
        await self.get(f"{self._prefix}/login")
        return await self.post(f"{self._prefix}/login", json={"username": username, "password": password})

    async def logout(self) -> AdminResponse:
        return await self.post(f"{self._prefix}/logout", json={})


class Admin:
    """The resources of one registry.

    ``permissions`` is the strategy: ``"page"`` (``<label>.read_<resource>`` and ``<label>.manage_<resource>``, the default),
    ``"model"`` (``add_``, ``change_``, ``delete_`` and ``view_``) or ``"access"`` (only ``admin.access``).
    ``label`` is the label of the names (``admin`` by default). ``roles`` maps a role name to pattern grants
    (``{"support": ["orders.*", "-shops.see_credential"]}``) that ``seed_roles`` writes to the auth tables.
    ``brand`` sets the name, logo, colors, header links and environment label of the UI. ``groups`` is the order of the
    navigation groups; a resource names one of them with ``group``. ``widgets`` maps a field (``"Book.title"``), a nested subfield (``"Geo.lat"``)
    or a type name (``"Text"``, ``"Status"``, a nested type) to a widget for every resource; a resource ``widgets`` entry wins at one level.
    ``statement_timeout`` is the time limit in seconds of each request's database statements (15 by default, 0 for none).
    The admin ignores tenancy: a read default that reads a ``scope`` value is off in every admin read, so staff see the rows of all tenants.
    ``storage`` is the ``monorm_file_storage.Registry`` that holds the files of the ``files`` fields of the resources.
    """

    def __init__(
        self,
        registry: Registry = default_registry,
        prefix: str = DEFAULT_PREFIX,
        *,
        permissions: Strategy = "page",
        label: str = "admin",
        roles: Mapping[str, Sequence[str]] | None = None,
        brand: BrandConfig | None = None,
        widgets: Mapping[str, str] | None = None,
        groups: Sequence[str] = (),
        statement_timeout: float = 15.0,
        storage: Any = None,
    ) -> None:
        self._storage = storage
        if not math.isfinite(statement_timeout) or statement_timeout < 0:
            raise AdminConfigError("statement_timeout is a number of seconds, 0 or more (0 sets no limit)")
        self._statement_timeout = statement_timeout
        self._registry = registry
        self.prefix = prefix
        if unknown := sorted(set(brand or {}) - _BRAND_KEYS):
            raise AdminConfigError(f"unknown brand option {unknown[0]!r}")
        self._brand = dict(brand) if brand else None
        self._widgets = _widgets(None, widgets) if widgets else None
        self._groups = list(groups)
        self._planned: tuple[object, _native.AdminSite] | None = None
        self._strategy = permissions
        self._label = label
        self._roles = {name: list(patterns) for name, patterns in (roles or {}).items()}
        for name, patterns in self._roles.items():
            if isinstance(patterns, str) or not all(isinstance(p, str) for p in patterns):
                raise AdminConfigError(f"roles[{name!r}] must be a list of patterns")
        self._resources: dict[str, tuple[type[Model], ResourceConfig]] = {}

    def register(self, model: type[Model], config: ResourceConfig | None = None) -> None:
        """Makes ``model`` a resource. Once any model is registered, only registered models are resources."""
        if model.__name__ in self._resources:
            raise AdminConfigError(f"{model.__name__} is already registered")
        config = config or {}
        if unknown := sorted(set(config) - _RESOURCE_KEYS):
            raise AdminConfigError(f"{model.__name__}: unknown option {unknown[0]!r}")
        self._resources[model.__name__] = (model, config)
        self._planned = None

    def _config_json(self) -> str:
        resources = []
        for name, (model, config) in self._resources.items():
            resource: dict[str, Any] = {"model": name}
            if "list" in config:
                resource["list"] = _list(model, config["list"])
            if "queryset" in config:
                resource["queryset"], order = _queryset(model, config["queryset"])
                if order is not None:
                    listing = resource.setdefault("list", {})
                    if "order" in listing:
                        raise AdminConfigError(f"{name}: the order is in the queryset or in the list, not in both")
                    listing["order"] = order
            if "can" in config:
                resource["can"] = _can(model, config["can"])
            if "check_messages" in config:
                messages = config["check_messages"]
                if not isinstance(messages, Mapping) or not all(isinstance(k, str) and isinstance(v, str) for k, v in messages.items()):
                    raise AdminConfigError(f"{name}: check_messages maps a check name to a message")
                resource["check_messages"] = dict(messages)
            if "form" in config:
                resource["form"] = _form(name, config["form"])
            if "readonly" in config:
                names = config["readonly"]
                if isinstance(names, str) or not all(isinstance(n, str) for n in names):
                    raise AdminConfigError(f"{name}: readonly is a list of field names")
                resource["readonly"] = list(names)
            if "widgets" in config:
                resource["widgets"] = _widgets(name, config["widgets"])
            if "form_component" in config:
                path = config["form_component"]
                if not isinstance(path, (str, os.PathLike)):
                    raise AdminConfigError(f"{name}: form_component is the path of a JavaScript file")
                resource["form_component"] = os.fspath(path)
            if "relations" in config:
                resource["relations"] = _relations(name, model, config["relations"])
            if "files" in config:
                resource["files"] = list(_files(name, config["files"]))
                if self._storage is None:
                    raise AdminConfigError(f"{name}: files needs the storage option of Admin (a monorm_file_storage Registry)")
            raw: dict[str, Any] = dict(config)
            for key in ("title", "group", "allow", "perms", "label", "field_permissions", "actions"):
                if key in raw:
                    value = raw[key]
                    resource[key] = value if isinstance(value, str) else list(value)
            resources.append(resource)
        out: dict[str, Any] = {"prefix": self.prefix, "permissions": self._strategy, "label": self._label, "resources": resources}
        if self._brand is not None:
            out["brand"] = self._brand
        if self._widgets:
            out["widgets"] = self._widgets
        if self._groups:
            out["groups"] = self._groups
        if self._statement_timeout != 15.0:
            out["statement_timeout_ms"] = round(self._statement_timeout * 1000)
        return json.dumps(out)

    def _host(self) -> dict[str, Any]:
        """The host functions: login, the ``search_with`` function of each resource and the function of each computed column."""
        host: dict[str, Any] = dict(_HOST)
        for model, config in self._resources.values():
            listing = config.get("list", {})
            scope = config.get("queryset")
            if scope is not None and callable(scope) and not _is_prepared(scope):
                host[_queryset_name(model)] = _queryset_host(model, cast(Callable[..., Awaitable[Any]], scope))
            search = listing.get("search_with")
            if search is not None:
                host[_search_name(model)] = _search_host(model, search)
            for column in listing.get("computed", ()):
                function = column.get("function")
                if function is not None:
                    host[_batch_name(model, column.get("name", ""))] = _batch_host(model, function)
        files = {(model.__name__, field): storage for model, config in self._resources.values() for field, storage in _files(model.__name__, config.get("files", {})).items()}
        if files:
            host.update(_file_host(self._storage, files))
        return host

    def _site(self, registry: Registry) -> _native.AdminSite:
        schema = registry._native if registry._native is not None else registry.prepare()
        try:
            return _native.admin_site(schema, self._config_json())
        except ValueError as e:
            raise AdminConfigError(str(e)) from None

    def check(self) -> list[str]:
        """Raises :class:`AdminConfigError` when the config does not fit the schema. Needs no database.

        Returns the warnings: for example a search field that no index serves. ``monorm admin`` logs the same warnings at start."""
        return list(self._site(self._registry).warnings)

    def url(self, model: type[Model], key: Any) -> str:
        """The path of the page of one record: ``/admin/<Model>/<key>``.

        ``key`` is the value of a single key field, a sequence of the key values in key order, or a mapping of key fields.
        Raises :class:`AdminConfigError` when ``model`` is no resource and ``ValueError`` when the key does not fit.
        """
        schema = self._registry._native if self._registry._native is not None else self._registry.prepare()
        if self._planned is None or self._planned[0] is not schema:
            self._planned = (schema, self._site(self._registry))
        try:
            return self._planned[1].url(model.__name__, json.dumps(_key_json(key)))
        except ValueError as e:
            if "is not an admin resource" in str(e):
                raise AdminConfigError(str(e)) from None
            raise

    @property
    def permissions(self) -> list[str]:
        """The permission names of the chosen strategy, ``admin.access`` first. They need a prepared registry, not a database."""
        return list(self._site(self._registry).permissions)

    async def sync_permissions(self, database: Database | None = None) -> SyncReport:
        """Creates the names of :attr:`permissions` that the auth tables lack and reports the stale ones. It deletes nothing."""
        from .db import resolve

        db = resolve(database)
        site = self._site(db._registry)
        try:
            return await _admin_permissions.sync(site.permissions, site.labels, db)
        except ValueError as e:
            raise AdminConfigError(str(e)) from None

    async def seed_roles(self, database: Database | None = None) -> RoleSeedReport:
        """Writes the pattern grants of ``roles`` to the auth tables. A second run changes nothing; no role or grant is removed.

        Run :meth:`sync_permissions` first: a pattern grants only names that exist in the tables.
        """
        from .db import resolve

        db = resolve(database)
        site = self._site(db._registry)
        try:
            return await _admin_permissions.seed(self._roles, site.permissions, db)
        except ValueError as e:
            raise AdminConfigError(str(e)) from None

    def test_client(
        self,
        database: Database | None = None,
        *,
        user: Mapping[str, Any] | None = None,
        secret: str | None = None,
        settings: Mapping[str, Any] | None = None,
        ip: str = "127.0.0.1",
    ) -> AdminTestClient:
        """``user=`` is the in-memory path with no login. ``secret=`` (at least 32 bytes) runs the router with login;
        ``settings`` then takes ``idle_timeout`` and ``session_timeout`` (whole seconds), ``origins`` and ``trusted_proxies``.
        A wrong secret raises ``ValueError`` at the first request."""
        from .db import resolve

        db = resolve(database)
        return AdminTestClient(self._site(db._registry), db, user, secret=secret, settings=settings, prefix=self.prefix, ip=ip, host=self._host())


_default = Admin()


def register(model: type[Model], config: ResourceConfig | None = None) -> None:
    _default.register(model, config)


def check() -> list[str]:
    return _default.check()


def url(model: type[Model], key: Any) -> str:
    return _default.url(model, key)


def test_client(
    database: Database | None = None,
    *,
    user: Mapping[str, Any] | None = None,
    secret: str | None = None,
    settings: Mapping[str, Any] | None = None,
    ip: str = "127.0.0.1",
) -> AdminTestClient:
    return _default.test_client(database, user=user, secret=secret, settings=settings, ip=ip)


test_client.__test__ = False  # type: ignore[attr-defined]
