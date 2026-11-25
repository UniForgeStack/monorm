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
import os
import uuid
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from functools import partial
from json import dumps as _dumps
from typing import TYPE_CHECKING, Any, Literal, Mapping, NotRequired, Required, Sequence, TypedDict, cast

from . import _native
from . import _admin_permissions
from ._admin_host import HOST as _HOST, auth_user_table
from . import _admin_routes
from ._admin_routes import Reply, RouteFunction, RouteRequest, Stream
from ._duration import format_duration
from ._admin_write import ActionDef, Context, InputDef, action, action_def, action_function, admin_mode_of, download, input, is_sync, is_threaded, log, message, redirect, refresh, service_function, threaded, view_host, writer_host
from ._admin_permissions import RoleSeedReport, SyncReport
from .adapters import _ADAPTERS as _adapters
from .errors import QueryError
from .expr import ColumnRef, Condition, Expression, IRContext, Ordering, RelationPath, SlotParams, _Slot, as_condition, resolve_null_rule
from .model import Model, registry as default_registry

if TYPE_CHECKING:
    from .db import Database
    from .model import Registry

__all__ = ["Admin", "AdminConfigError", "AdminSettingsError", "AdminResponse", "AdminTestClient", "AggregateColumn", "BrandConfig", "BrandLink", "ThemeConfig", "ActionDef", "ComputedColumn", "Context", "DateHierarchy", "EmbedConfig", "Fieldset", "FrameConfig", "InlineDef", "ListConfig", "NamedChoice", "NamedFilter", "PackageTabConfig", "Reply", "ResourceConfig", "RoleSeedReport", "RouteRequest", "Stream", "SwitcherBuild", "SwitcherConfig", "SwitcherService", "SyncReport", "action", "check", "download", "inline", "input", "log", "message", "redirect", "refresh", "register", "test_client", "threaded", "url"]

DEFAULT_BASE_PATH = "/admin"


class AdminConfigError(ValueError):
    """The admin config names an unknown field, a list column through a to-many relation, or a bad value.

    ``code`` says which rule failed: ``config.<rule>`` for a wrong site config, such as ``config.unknown_field``."""

    code: str

    def __init__(self, message: str, code: str = "config.invalid_value") -> None:
        super().__init__(message)
        self.code = code


class AdminSettingsError(AdminConfigError):
    """The ``[admin]`` settings, the session options or the secret are wrong. ``code`` is ``settings.<rule>``, such as ``settings.secret``."""

    def __init__(self, message: str, code: str = "settings.invalid_value") -> None:
        super().__init__(message, code)


def _admin_error(e: ValueError) -> AdminConfigError:
    """The error class of a ``ValueError`` that the native module raised: its ``code`` attribute picks the class."""
    code = getattr(e, "code", None)
    if not isinstance(code, str):
        return AdminConfigError(str(e))
    return (AdminSettingsError if code.startswith("settings.") else AdminConfigError)(str(e), code)


def _admin_call(fn: Callable[..., Any], *args: Any) -> Any:
    """Calls a native admin function and raises its refusal as an :class:`AdminConfigError`."""
    try:
        return fn(*args)
    except ValueError as e:
        raise _admin_error(e) from None


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


BadgeColour = Literal["grey", "blue", "green", "yellow", "red", "purple"]


class DateHierarchy(TypedDict, total=False):
    field: Required[ColumnRef[Any] | str]
    counts: bool
    """``False`` skips the counts: the bar shows the periods of the date range and runs no count statement."""


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
    """A sort header. It needs ``order_by``, or ``expr`` and ``nullable=False``."""
    order_by: Any
    """What the sort header orders by: a field of the model or of a to-one relation (``Note.author.name``, not null), or an expression
    (the column then declares ``nullable=False``). It makes the column sortable, also for ``function``."""


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
    badges: Mapping[ColumnRef[Any] | str, Mapping[str | int | bool, BadgeColour]]
    """A column to a map of value to colour: the list and the detail page show the value as a badge. A value outside the map shows grey."""
    date_hierarchy: ColumnRef[Any] | str | DateHierarchy
    """A date or datetime field (one to-one hop allowed, as ``"author.joined"``): a bar over the table drills down year, month and day. It joins ``filters``."""
    tabs: Sequence[RelationPath[Any]]
    """The to-many relations that the detail page shows as tabs. Without it: every one that leads to a resource."""
    order: Sequence[ColumnRef[Any] | Ordering]
    page_size: int
    """Rows per page, 1 to 200 (25 by default)."""
    sortable: Sequence[ColumnRef[Any]]
    """The list columns that a sort header may use. Without it every list column that can sort."""
    editable: Sequence[ColumnRef[Any] | str]
    """List columns that a user edits in place and saves for several rows at once. Each is a plain field of the model that the update form may write."""
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
    environment_level: Literal["info", "warning", "danger"]
    """The colour of the environment label. ``production`` gives ``danger``, ``staging`` ``warning``, any other label ``info``."""


class SwitcherService(TypedDict):
    key: str
    """Lower case letters, digits, ``-`` and ``_``; unique."""
    title: str
    """1 to 40 characters."""
    url: NotRequired[str]
    """A path that starts with ``/`` or an ``https`` URL. Needed for every service but the current one."""
    icon: NotRequired[str]
    """A lucide icon name such as ``server``."""
    color: NotRequired[str]
    """``#rgb`` or ``#rrggbb``. The colour of the current service sets the primary colour."""


class SwitcherBuild(TypedDict, total=False):
    """Plain strings that your project fills from its own environment. An empty string is no value."""

    commit: str
    built_at: str
    deployed_at: str
    tree: str


class SwitcherConfig(TypedDict, total=False):
    """The service switcher. The same table in ``monorm.toml`` (``[admin.switcher]``, ``${NAME}`` reads the environment) adds keys; a key set in both is an error."""

    current: str
    """The ``key`` of the service that this admin is."""
    services: Sequence[SwitcherService]
    build: SwitcherBuild


class ThemeConfig(TypedDict, total=False):
    """The look of the UI in three values. Each one is the same value as a ``brand`` key; set it in one place only."""

    title: str
    """The name in the header and the page title (same as ``brand["name"]``)."""
    logo: str
    """The URL of the logo image: a path or an ``https`` URL."""
    color: str
    """The brand colour, ``#rgb`` or ``#rrggbb`` (same as ``brand["colors"]["primary"]``). The UI picks a readable text colour for it."""


class SectionConfig(TypedDict, total=False):
    title: str
    fields: Sequence[str]
    collapsed: bool
    """A collapsed section starts closed. It needs a ``title``."""


class FrameConfig(TypedDict):
    name: str
    title: str
    url: str
    sandbox: NotRequired[Sequence[str]]
    scripts: NotRequired[bool]
    same_origin: NotRequired[bool]
    height: NotRequired[str]


class Fieldset(TypedDict):
    name: str
    fields: Sequence[str]
    title: NotRequired[str]


class EmbedConfig(TypedDict):
    resource: str
    tab: str
    lookup: str
    ancestors: Sequence[str]


class PackageTabConfig(TypedDict):
    module: str
    embeddable: NotRequired[bool]


class ResourceConfig(TypedDict, total=False):
    list: ListConfig
    title: str
    """The name in the navigation and the page heading (the model name, spaced, by default)."""
    group: str
    """The navigation group: one of ``groups`` of the admin."""
    icon: str
    """A lucide icon name such as ``book-open``, shown in the navigation."""
    allow: Sequence[Literal["read", "create", "update", "delete"]]
    """The operations that the resource allows for every user, superusers too."""
    perms: Sequence[Literal["read", "manage", "delete"]]
    """The ``page`` strategy only: add ``"delete"`` to split the delete permission from ``manage``."""
    label: str
    """The label of the permission names of this resource, instead of the site label."""
    field_permissions: Sequence[str]
    """Names such as ``"see_credential"`` that become ``<label>.<name>``."""
    see: Mapping[str, str]
    """Field permissions that show a field: ``{"cost": "see_cost"}`` shows the column ``cost`` only to a user who holds ``<label>.see_cost``.
    Without it the field leaves the list, detail, tabs, search, filters, sort, export, labels, inlines and the response. A filter, sort or write on it is an error."""
    change: Mapping[str, str]
    """Field permissions that change a field: ``{"price": "change_price"}`` makes ``price`` read-only for a user who does not hold ``<label>.change_price``."""
    actions: Sequence[Any]
    """Functions that ``@admin.action(...)`` marked (a button where staff need it), or names such as ``"refund"`` that only declare
    the permission ``<label>.<action>_<resource>``."""
    bulk_delete: bool
    set_password: bool
    """``False`` removes the bulk delete of the list. It is on by default; it needs the delete permission, and a ``delete`` service gets the query set."""
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
    detail_tabs: Sequence[str]
    frames: Sequence[FrameConfig]
    fieldsets: Sequence[Fieldset]
    """Package detail tabs, each ``"<package>.<tab>"``: the detail page shows them after the relation tabs. See :meth:`Admin.package`."""
    form_component: str | os.PathLike[str]
    """The path of a JavaScript module that replaces the whole form: ``export default (React) => function Form(props) {...}``.
    The admin reads the file when the site starts and serves it to signed-in staff."""
    relations: Mapping[str, RelationConfig]
    """The relation fields that the form edits, by relation name. A to-one relation becomes a picker with keyset autocomplete;
    a many-to-many relation (through a join model) becomes chips. The target of each needs a text column that names a row."""
    files: Mapping[str, str]
    """File fields by field name, each with the name of its storage in the ``storage`` registry of the admin
    (``{"cover": "media"}``). A file field is a nullable ``Json`` field that holds the reference of the file."""
    create: Callable[..., Any]
    """The service of a create: ``async def create(ctx, values)`` returns the new record. It owns its transactions."""
    update: Callable[..., Any]
    """The service of an update: ``async def update(ctx, qs, changes)``, where ``qs`` is the one-row query set of the record
    (with the ``queryset`` filter and the ``can`` rule). Check ``ctx.loaded`` for a version conflict (``VersionConflict`` is a 409)."""
    delete: Callable[..., Any]
    """The service of a delete: ``async def delete(ctx, qs)``."""
    inputs: Sequence[InputDef]
    """Controls of the form that are no field (``admin.input("reason", type="textarea")``). Only a service reads them (``ctx.inputs``)."""
    allow_protected_writes: bool
    """Open application write rules for this resource. The audit row records the opt-in."""
    inlines: Sequence[Any]
    """To-many relations that the detail page shows and edits in place: ``admin.inline(OrderLine)`` for each one (a bare model works too).
    The inline takes the registered page of the child model; ``admin.inline(..., page={...})`` overrides page keys."""


class InlineDef:
    """A to-many relation that the detail page of the parent edits in place; see :func:`inline`."""

    def __init__(self, model: type[Model], via: str | None, save: str, permission: str | None, style: str, max: int, label: str | None, collapsed: bool, delete: bool, page: Mapping[str, Any] | None) -> None:
        self.model = model
        self.via = via
        self.save = save
        self.permission = permission
        self.style = style
        self.max = max
        self.label = label
        self.collapsed = collapsed
        self.delete = delete
        self.page: Mapping[str, Any] = dict(page or {})


def inline(
    model: type[Model],
    *,
    via: str | None = None,
    save: Literal["with_parent", "row"] = "with_parent",
    permission: str | None = None,
    style: Literal["table", "stacked"] = "table",
    max: int = 20,
    label: str | None = None,
    collapsed: bool = False,
    delete: bool = True,
    page: ResourceConfig | None = None,
) -> InlineDef:
    """The inline of a to-many relation: the registered page of ``model`` (or ``page``, for a model with no page) as rows of the parent.

    ``via`` names the relation of the parent when more than one leads to ``model``. ``save="with_parent"`` sends the rows with the parent
    in one change set and one transaction; ``"row"`` saves each row alone through the page of the child. ``permission`` is a name
    (``"edit_lines"`` becomes ``<label>.edit_lines``) that replaces the write permission of the parent. ``max`` is the row cap (1 to 200). ``page`` overrides page keys of the child:
    ``queryset``, ``can``, ``list``, ``form``, ``readonly``, ``widgets``, ``relations``, ``inputs`` and ``check_messages``.
    """
    _admin_call(_native.AdminConfigBuilder.check_inline, model.__name__, save, style)
    return InlineDef(model, via, save, permission, style, max, label, collapsed, delete, page)


class RelationConfig(TypedDict, total=False):
    label: str
    """The text column of the target that names a row in the picker (the first text column by default)."""
    create: bool
    """Offer a modal in the form that creates a record of the target and fills the field."""
    choices: Any
    """A prepared query set of the target (``Author.objects.filter(Author.active == True).prepare()``) that limits the picker.
    The server checks it again on write. It combines with the ``queryset`` of the target."""


Strategy = Literal["page", "model", "access"]


def _ir(model: type[Model], node: Any, what: str, *, wire: bool = False) -> Any:
    """The engine IR of an expression or condition, with literals inlined. ``wire`` sends it as ``{"$ir": ...}``, with a ``slot`` node for each ``param(...)``; Rust checks the slots."""
    params: list[Any] = SlotParams() if wire else []
    if isinstance(node, Condition):
        node = resolve_null_rule(node)
    elif not isinstance(node, Expression):
        node = as_condition(node)
    try:
        ir = node._ir(IRContext(model, params))
    except QueryError as e:
        raise AdminConfigError(f"{model.__name__}: {what}: {e}") from None
    where = f"{model.__name__}: {what}"
    inlined = _inline(ir, params, where, wire)
    return {"$ir": inlined} if wire else inlined


def _inline(ir: Any, params: list[Any], what: str, wire: bool = False) -> Any:
    if isinstance(ir, list):
        return [_inline(item, params, what, wire) for item in ir]
    if not isinstance(ir, dict):
        return ir
    if ir.get("t") == "param":
        value = params[ir["i"]]
        if wire and isinstance(value, _Slot):
            return {"t": "slot", "name": value.name, "transform": value.transform is not None}
        if value is None:
            return {"t": "null"}
        if isinstance(value, bool):
            return {"t": "const", "value": value}
        if isinstance(value, int):
            return {"t": "int", "value": value}
        if isinstance(value, str):
            return {"t": "text", "value": value}
        if wire:
            return {"$host": type(value).__name__}
        raise AdminConfigError(f"{what}: a value of type {type(value).__name__} is not a literal of the admin; use an integer, a string or a boolean")
    return {k: _inline(v, params, what, wire) for k, v in ir.items()}


def _ref(ref: Any) -> Any:
    """A column or relation of a model on the wire: a name stays, a column or relation is ``$ref``, any other value is ``$host``. Rust checks the model and the name."""
    if isinstance(ref, str):
        return ref
    if isinstance(ref, RelationPath):
        return {"$ref": {"model": ref._root.__name__, "path": ".".join(ref._path)}}
    if isinstance(ref, ColumnRef):
        return {"$ref": {"model": ref._root.__name__, "path": ".".join((*ref._path, ref._field.name))}}
    return {"$host": type(ref).__name__}


def _order_item(model: type[Model], item: Any) -> Any:
    """An order item on the wire: the column, with ``desc`` and ``nulls`` beside it. Rust refuses an expression and a placement."""
    expr, desc, nulls = (item.expr, item.desc, item.nulls) if isinstance(item, Ordering) else (item, False, None)
    by = _ir(model, expr, "order", wire=True) if isinstance(expr, Expression) and not isinstance(expr, ColumnRef) else _ref(expr)
    if not isinstance(by, dict) or not (desc or nulls):
        return by
    return {**by, **({"desc": True} if desc else {}), **({"nulls": nulls} if nulls else {})}


# The config positions that hold a host object, and what each holds. The call sites send each one through `_host`.
_HOST_FIELDS: dict[str, str] = {
    "list.columns": "column",
    "list.filters": "column",
    "list.tabs": "column",
    "list.editable": "column",
    "list.sortable": "column",
    "list.search": "column",
    "list.date_hierarchy": "column",
    "list.date_hierarchy.field": "column",
    "list.order": "order",
    "list.aggregates.relation": "relation",
    "list.aggregates.field": "column",
    "list.computed.order_by": "order_by",
    "list.computed.expr": "ir",
    "list.named_filters.choices.condition": "ir",
    "list.search_with": "function",
    "list.computed.function": "function",
    "can": "ir",
    "actions.can": "ir",
    "actions.applies_to": "ir",
    "queryset": "queryset",
    "relations.choices": "queryset",
}


def _host(model: type[Model], path: str, value: Any, what: str = "", fns: _Fns | None = None, maker: Callable[[], Any] | None = None) -> Any:
    """The wire form of the value at config position ``path``, by its kind in ``_HOST_FIELDS``."""
    kind = _HOST_FIELDS[path]
    if kind == "column":
        return _ref(value)
    if kind == "relation":
        return _ref(value) if isinstance(value, (str, RelationPath)) else {"$host": type(value).__name__}
    if kind == "order":
        return _order_item(model, value)
    if kind == "order_by":
        return _ref(value) if isinstance(value, ColumnRef) else _ir(model, value, what, wire=True) if isinstance(value, Expression) else {"$host": type(value).__name__}
    if kind == "ir":
        return _ir(model, value, what, wire=True)
    assert fns is not None
    return _queryset(model, value, fns) if kind == "queryset" else fns.register(cast(Callable[[], Any], maker))


class _Fns:
    """The host functions of one config: a local id for each, sent as ``{"$fn": id}``. Rust names them and returns the table."""

    def __init__(self) -> None:
        self._makers: dict[str, Callable[[], Callable[[Any, dict[str, Any]], Awaitable[Any]]]] = {}

    def register(self, maker: Callable[[], Callable[[Any, dict[str, Any]], Awaitable[Any]]]) -> dict[str, str]:
        key = f"f{len(self._makers)}"
        self._makers[key] = maker
        return {"$fn": key}

    def host(self, handles: Mapping[str, str]) -> dict[str, Any]:
        return {name: self._makers[key]() for key, name in handles.items()}


def _computed(model: type[Model], items: Sequence[Mapping[str, Any]], fns: _Fns) -> list[dict[str, Any]]:
    out = []
    for item in items:
        entry = {k: v for k, v in item.items() if k not in ("expr", "function", "order_by")}
        if item.get("order_by") is not None:
            entry["order_by"] = _host(model, "list.computed.order_by", item["order_by"], f"computed {item.get('name')!r} order_by")
        if item.get("expr") is not None:
            entry["expr"] = _host(model, "list.computed.expr", item["expr"], f"computed {item.get('name')!r}")
        if item.get("function") is not None:
            entry["function"] = _host(model, "list.computed.function", item["function"], fns=fns, maker=partial(_batch_host, model, item["function"]))
        out.append(entry)
    return out


def _aggregates(model: type[Model], items: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    out = []
    for item in items:
        entry: dict[str, Any] = {k: v for k, v in item.items() if k not in ("relation", "fn", "field")}
        entry["relation"] = _host(model, "list.aggregates.relation", item["relation"])
        entry["fn"] = item["fn"]
        if item.get("field") is not None:
            entry["field"] = _host(model, "list.aggregates.field", item["field"])
        out.append(entry)
    return out


def _named(model: type[Model], items: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    out = []
    for item in items:
        entry = {k: v for k, v in item.items() if k != "choices"}
        entry["choices"] = [
            {**{k: v for k, v in c.items() if k != "condition"}, "expr": _host(model, "list.named_filters.choices.condition", c["condition"], f"named filter {item.get('name')!r}")}
            for c in item["choices"]
        ]
        out.append(entry)
    return out


def _date_hierarchy(model: type[Model], value: Any) -> Any:
    if isinstance(value, Mapping):
        field = {"field": _host(model, "list.date_hierarchy.field", value["field"])} if "field" in value else {}
        return {**value, **field}
    return _host(model, "list.date_hierarchy", value)


def _list(model: type[Model], config: Mapping[str, Any], fns: _Fns) -> dict[str, Any]:
    # Every key goes as None first so that Rust names an unknown key; a known key is replaced below.
    out: dict[str, Any] = {k: None for k in config}
    if "columns" in config:
        out["columns"] = [_host(model, "list.columns", c) for c in config["columns"]]
    for key in ("filters", "tabs", "editable"):
        if key in config:
            out[key] = [_host(model, f"list.{key}", c) for c in config[key]]
    if "named_filters" in config:
        out["named_filters"] = _named(model, config["named_filters"])
    if "computed" in config:
        out["computed"] = _computed(model, config["computed"], fns)
    if "aggregates" in config:
        out["aggregates"] = _aggregates(model, config["aggregates"])
    for key in ("labels", "empty_fields"):
        if key in config:
            out[key] = [[_ref(k), v] for k, v in config[key].items()]
    if "empty" in config:
        out["empty"] = config["empty"]
    if "date_hierarchy" in config:
        out["date_hierarchy"] = _date_hierarchy(model, config["date_hierarchy"])
    if "badges" in config:
        out["badges"] = [[_ref(k), {_badge_value(v): colour for v, colour in map_.items()}] for k, map_ in config["badges"].items()]
    if "order" in config:
        out["order"] = [_host(model, "list.order", item) for item in config["order"]]
    if "page_size" in config:
        out["page_size"] = config["page_size"]
    for key in ("sortable", "search"):
        if key in config:
            out[key] = [_host(model, f"list.{key}", c) for c in config[key]]
    if "search_with" in config:
        out["search_with"] = _host(model, "list.search_with", config["search_with"], fns=fns, maker=partial(_search_host, model, config["search_with"]))
    return out


def _badge_value(value: Any) -> str:
    """The text of a badge key: a bool as ``true`` or ``false``, a string or an int as its text. Rust checks that the column has the value."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (str, int)):
        return str(value)
    raise AdminConfigError(f"a badge value is a string, an integer or a boolean, not {value!r}")


def _queryset(model: type[Model], config: Any, fns: _Fns) -> dict[str, Any]:
    """The queryset of a resource on the wire: ``$fn`` for a function, ``$qs`` for a prepared query set. Rust checks both."""
    from .query import Prepared

    if callable(config) and not isinstance(config, Prepared):
        return fns.register(partial(_queryset_host, model, cast(Callable[..., Awaitable[Any]], config)))
    if not isinstance(config, Prepared):
        return {"$host": type(config).__name__}
    qs = config._qs
    body: dict[str, Any] = {
        "model": qs.model.__name__,
        "load": bool(qs._loads or qs._model_fields is not None or qs._related or qs._prefetch),
        "slice": qs._limit is not None or qs._offset is not None,
        "join": qs._from is not None or bool(qs._joins),
        "lock": qs._lock is not None,
        "without_defaults": bool(qs._without_defaults),
    }
    if qs.model is not model or not qs._filters or any(body[flag] for flag in ("load", "slice", "join", "lock", "without_defaults")):
        # Rust refuses it with its code; building the filter first would raise a binding text before it.
        return {"$qs": body}
    params = SlotParams()
    ctx = IRContext(model, params)
    try:
        filters = [resolve_null_rule(f)._ir(ctx) for f in qs._filters]
    except QueryError as e:
        raise AdminConfigError(f"{model.__name__}: queryset: {e}") from None
    irs = [_inline(f, params, f"{model.__name__}: queryset", True) for f in filters]
    body["filter"] = {"$ir": irs[0] if len(irs) == 1 else {"t": "and", "items": irs}}
    body["order"] = [_order_item(model, o) for o in qs._order]
    return {"$qs": body}


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
    return {name: _host(model, "can", rule, f"can {name!r}") for name, rule in rules.items()}


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
    if isinstance(value, datetime.timedelta):
        return format_duration(value)
    return value.isoformat() if isinstance(value, (datetime.date, datetime.time)) else str(value)


def _key_value(value: Any) -> Any:
    if isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, datetime.timedelta):
        return format_duration(value)
    # `str(datetime)` has a space where the detail route wants `T`.
    return value.isoformat() if isinstance(value, (datetime.date, datetime.time)) else str(value)


def _key_json(key: Any) -> Any:
    if isinstance(key, Mapping):
        return {str(k): _key_value(v) for k, v in key.items()}
    if isinstance(key, (list, tuple)):
        return [_key_value(v) for v in key]
    return _key_value(key)


def _relations(name: str, model: type[Model], value: Any, fns: _Fns) -> Any:
    """The ``relations`` option on the wire. Rust checks the names, the option keys and the value types."""
    if not isinstance(value, Mapping):
        return value
    out: dict[str, Any] = {}
    for rel_name, options in value.items():
        relation = model._meta.relations.get(rel_name) if isinstance(rel_name, str) else None
        if not isinstance(options, Mapping):
            out[rel_name] = options
            continue
        entry = dict(options)
        if "choices" in options:
            entry["choices"] = _host(relation.target if relation else model, "relations.choices", options["choices"], fns=fns)
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


_SESSION_COOKIE = "__Secure-monorm-session"
_CSRF_COOKIE = "__Secure-monorm-csrf"
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
        prefix: str | None = None,
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
        self._prefix = site.prefix if prefix is None else prefix
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
            if file is not None or self._site.is_raw(path):
                kind, data = _multipart(file) if file is not None else (None, None)
                if json is not None:
                    kind, data = "application/json", _dumps(json).encode()
                code, reply, raw = await self._site.request_raw(self._database._engine, method, path, self._user, trace, self._database, self._host, kind, data)
                events = trace.take()
                return AdminResponse(code, raw.decode("utf-8", "replace"), len(events), tuple(reply), tuple(e[0] for e in events), raw)
            sent_json = None if json is None else _dumps(json)
            status, body = await self._site.request(self._database._engine, method, path, self._user, trace, self._database, self._host, sent_json)
            events = trace.take()
            return AdminResponse(status, body, len(events), sql=tuple(e[0] for e in events), content=body.encode())
        router = self._router
        if router is None:
            try:
                router = self._router = self._site.client(self._database._engine, self._database, self._host, self._secret, self._options)
            except ValueError as e:
                raise _admin_error(e) from None
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

    def advance_clock(self, seconds: int) -> None:
        """Moves the clock of the login router forward: a test ends a session or a renewal without a wait. Send a request first."""
        if self._router is None:
            raise RuntimeError("advance_clock needs a login-mode client that has sent a request")
        self._router.advance_clock(seconds)

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


def _json_default(value: Any) -> Any:
    """The wire form of a value that JSON has no type for: a mapping, a path, a set, or a host value that Rust cannot read."""
    if isinstance(value, Mapping):
        return dict(value)
    if isinstance(value, os.PathLike):
        return os.fspath(value)
    if isinstance(value, (set, frozenset, Sequence)):
        return list(value)
    return {"$host": type(value).__name__}


def _inline_defs(name: str, items: Any) -> list[InlineDef]:
    if isinstance(items, (str, InlineDef)) or not isinstance(items, Sequence):
        raise AdminConfigError(f"{name}: inlines is a list of admin.inline(...) wrappers or models")
    out: list[InlineDef] = []
    for item in items:
        if isinstance(item, InlineDef):
            out.append(item)
        elif isinstance(item, type) and issubclass(item, Model):
            out.append(inline(item))
        else:
            raise AdminConfigError(f"{name}: inlines is a list of admin.inline(...) wrappers or models")
    return out


def _actions_json(name: str, model: type[Model], items: Any, fn_entry: Callable[[ActionDef], dict[str, Any]]) -> list[dict[str, Any]]:
    """The action entries of a resource: a name declares a permission, a marked function is a button."""
    if isinstance(items, str):
        raise AdminConfigError(f"{name}: actions is a list")
    out: list[dict[str, Any]] = []
    for item in items:
        d = action_def(item)
        if d is None:
            if not isinstance(item, str):
                raise AdminConfigError(f"{name}: an action is a name or a function that @admin.action marked, not {item!r}")
            entry: dict[str, Any] = {"name": item, "permission": item}
        else:
            if not all(isinstance(i, InputDef) for i in d.inputs):
                raise AdminConfigError(f"{name}: action {d.name!r}: inputs is a list of admin.input(...) controls")
            entry = {"name": d.name, "placement": d.placement, **fn_entry(d)}
            if d.permission is not None:
                entry["permission"] = d.permission
            for key in ("label", "confirm"):
                if (value := getattr(d, key)) is not None:
                    entry[key] = value
            if d.inputs:
                entry["inputs"] = [i.json() for i in d.inputs]
            if d.applies_to is not None:
                entry["applies_to"] = _host(model, "actions.applies_to", d.applies_to, f"action {d.name!r} applies_to")
            if d.can is not None:
                entry["can"] = _host(model, "actions.can", d.can, f"action {d.name!r} can")
            entry["replace_change"] = d.replace_change
        out.append(entry)
    return out


class Admin:
    """The resources of one registry.

    ``permissions`` is the strategy: ``"page"`` (``<label>.read_<resource>`` and ``<label>.manage_<resource>``, the default),
    ``"model"`` (``add_``, ``change_``, ``delete_`` and ``view_``) or ``"access"`` (only ``admin.access``).
    ``label`` is the label of the names (``admin`` by default). ``roles`` maps a role name to pattern grants
    (``{"support": ["orders.*", "-shops.see_credential"]}``) that ``seed_roles`` writes to the auth tables.
    ``brand`` sets the name, logo, colors, header links and environment label of the UI. ``theme`` sets the title, the logo and the
    brand colour in one small dict (``{"title": "Acme", "logo": "/static/acme.svg", "color": "#0a7d5a"}``); it must not repeat a ``brand`` value. ``groups`` is the order of the
    navigation groups; a resource names one of them with ``group`` and may set an ``icon`` (a lucide name), and ``group_icons`` maps a group to its icon.
    ``switcher`` lists the services of your deploy (``current``, ``services`` with ``key``, ``title``, ``url``, ``icon`` and ``color``) and the ``build`` strings; ``[admin.switcher]`` of ``monorm.toml`` takes the same keys. ``widgets`` maps a field (``"Book.title"``), a nested subfield (``"Geo.lat"``)
    or a type name (``"Text"``, ``"Status"``, a nested type) to a widget for every resource; a resource ``widgets`` entry wins at one level.
    ``statement_timeout`` is the time limit in seconds of each request's database statements (15 by default, 0 for none).
    The admin ignores tenancy: a read default that reads a ``scope`` value is off in every admin read, so staff see the rows of all tenants.
    ``storage`` is the ``monorm_file_storage.Registry`` that holds the files of the ``files`` fields of the resources.
    ``base_path`` is the URL path of the whole admin (``/admin`` by default), for a gateway that passes the full path.
    The API prefix is ``<base_path>/api`` unless ``prefix`` sets another one under the base path.
    """

    def __init__(
        self,
        registry: Registry = default_registry,
        prefix: str | None = None,
        *,
        base_path: str = DEFAULT_BASE_PATH,
        permissions: Strategy = "page",
        label: str = "admin",
        roles: Mapping[str, Sequence[str]] | None = None,
        brand: BrandConfig | None = None,
        theme: ThemeConfig | None = None,
        widgets: Mapping[str, str] | None = None,
        groups: Sequence[str] = (),
        group_icons: Mapping[str, str] | None = None,
        switcher: SwitcherConfig | None = None,
        frame_origins: Sequence[str] = (),
        frame_cards: Sequence[FrameConfig] = (),
        embeds: Mapping[str, EmbedConfig] | None = None,
        statement_timeout: float = 15.0,
        storage: Any = None,
    ) -> None:
        self._storage = storage
        self._registry = registry
        self.prefix = prefix
        self.base_path = base_path
        self._widgets = widgets
        self._groups = list(groups)
        self._group_icons = dict(group_icons) if group_icons else None
        self.switcher_settings: Mapping[str, Any] | None = None  # the `[admin.switcher]` table of monorm.toml, set by the serve mode
        self._frame_origins = list(frame_origins)
        self._frame_cards = list(frame_cards)
        self._embeds = dict(embeds or {})
        self._planned: tuple[object, _native.AdminSite] | None = None
        self._fns = _Fns()
        self._strategy = permissions
        self._label = label
        self._roles = {name: list(patterns) for name, patterns in (roles or {}).items()}
        options = {k: dict(v) for k, v in (("brand", brand), ("theme", theme), ("switcher", switcher)) if v}
        self._builder: _native.AdminConfigBuilder = _admin_call(
            _native.AdminConfigBuilder,
            json.dumps(options),
            float(statement_timeout),
            json.dumps({name: patterns if isinstance(patterns, str) else list(patterns) for name, patterns in (roles or {}).items()}),
        )
        self._resources: dict[str, tuple[type[Model], ResourceConfig]] = {}
        self._routes: dict[tuple[str, str], tuple[RouteFunction, str | None]] = {}
        self._packages: dict[str, dict[str, Any]] = {}

    def package(
        self,
        name: str,
        directory: str | os.PathLike[str],
        *,
        widgets: Mapping[str, str] | None = None,
        forms: Mapping[str, str] | None = None,
        tabs: Mapping[str, str | PackageTabConfig] | None = None,
        styles: Sequence[str] = (),
        admin_major: int = 1,
    ) -> None:
        """Adds an extension package: a directory of static ``.js``, ``.mjs`` and ``.css`` files that the admin serves to the browser.

        ``widgets`` maps a widget name to a module file of the directory. A field uses it as ``"<name>.<widget>"`` in ``widgets``.
        ``forms`` does the same for a form module (a resource ``form_component`` of ``"<name>.<form>"``). ``tabs`` maps a tab name to a module: a resource shows it on its detail page with ``detail_tabs=["<name>.<tab>"]``. ``styles`` lists the
        CSS files that load with the first widget. ``admin_major`` is the widget contract version that the files were built for.
        The admin reads the files once and refuses a package for another major version. A package adds its routes and actions
        with ``route`` and ``action`` itself: it is a function ``install(admin)`` that calls this method too.
        """
        _admin_call(
            self._builder.package,
            name,
            json.dumps(
                {
                    "name": name,
                    "admin_major": admin_major,
                    "dir": os.path.abspath(os.fspath(directory)),
                    "widgets": dict(widgets or {}),
                    "forms": dict(forms or {}),
                    "tabs": dict(tabs or {}),
                    "styles": list(styles),
                }
            ),
        )
        self._planned = None

    def route(self, method: str, path: str, fn: RouteFunction, *, permission: str | None = None) -> None:
        """Adds a JSON or stream endpoint under ``<prefix>/routes/<path>``, behind the admin session.

        ``path`` may hold ``{name}`` parts. ``fn(ctx, request)`` gets a ``Context`` and a ``RouteRequest`` (``method``, ``path``,
        ``params``, ``query``, ``body``). It returns a JSON value, ``Reply(json, status=)`` or ``Stream(chunks, content_type, filename)``.
        ``permission`` is a full name (``<label>.<name>``) that a user needs besides ``admin.access``; it shows in ``permissions``.
        Rust runs the session, CSRF and the permission check first: no session is a 401 and a missing permission a 403.
        """
        _admin_call(self._builder.route, method, path, permission, callable(fn))
        self._routes[(method.upper(), path.removeprefix("/"))] = (fn, permission)
        self._planned = None

    def register(self, model: type[Model], config: ResourceConfig | None = None) -> None:
        """Makes ``model`` a resource. Once any model is registered, only registered models are resources."""
        config = config or {}
        _admin_call(self._builder.register, model.__name__, list(config))
        self._resources[model.__name__] = (model, config)
        self._planned = None

    def _fn_entry(self, fns: _Fns, fn: Any, top: bool, maker: Callable[[], Any]) -> dict[str, Any]:
        """A service or action function on the wire. A resource sends ``$fn`` with the two flags beside it; an inline page sends the flags alone. A value that is not callable is ``$host``."""
        if not callable(fn):
            return {"$host": type(fn).__name__}
        flags = {"sync": is_sync(fn), "threaded": is_threaded(fn)}
        return {**fns.register(maker), **flags} if top else flags

    def _service_host(self, model: type[Model], op: str, fn: Any) -> Any:
        return service_function(model, op, fn, self._inline_models())

    def _action_host(self, model: type[Model], config: Mapping[str, Any], d: ActionDef) -> Any:
        return action_function(model, cast("ResourceConfig", config), d, self._inline_models())

    def _resource_json(self, name: str, model: type[Model], config: Mapping[str, Any], fns: _Fns, top: bool = True) -> dict[str, Any]:
        """The config JSON of one page. ``top`` is false for the page of an inline."""
        resource: dict[str, Any] = {"model": name}
        if "list" in config:
            resource["list"] = _list(model, config["list"], fns)
        if "queryset" in config:
            resource["queryset"] = _host(model, "queryset", config["queryset"], fns=fns)
        if "can" in config:
            resource["can"] = _can(model, config["can"])
        # These keys go as the user wrote them: Rust checks their shape and names the key in the error.
        for key in ("check_messages", "form", "readonly", "see", "change", "widgets", "form_component", "detail_tabs"):
            if key in config:
                resource[key] = config[key]
        if "frames" in config:
            resource["frames"] = list(config["frames"])
        if "fieldsets" in config:
            resource["fieldsets"] = list(config["fieldsets"])
        if "relations" in config:
            resource["relations"] = _relations(name, model, config["relations"], fns)
        if "files" in config:
            files = config["files"]
            resource["files"] = list(files) if isinstance(files, Mapping) else files
        services = {op: fn for op in ("create", "update", "delete") if (fn := config.get(op)) is not None}
        if services:
            resource["service"] = {op: self._fn_entry(fns, fn, top, partial(self._service_host, model, op, fn)) for op, fn in services.items()}
        if "inputs" in config:
            defs = config["inputs"]
            if isinstance(defs, (str, InputDef)) or not all(isinstance(d, InputDef) for d in defs):
                raise AdminConfigError(f"{name}: inputs is a list of admin.input(...) controls")
            resource["inputs"] = [d.json() for d in defs]
        if config.get("allow_protected_writes"):
            resource["allow_protected_writes"] = True
        if _adapters.get(model):
            resource["host_behavior"] = True
            views = {field: mode for field, adapter in _adapters[model].items() if (mode := admin_mode_of(adapter))}
            if views:
                resource["views"] = views
        if "actions" in config:
            def action_fn(d: ActionDef) -> dict[str, Any]:
                value = self._fn_entry(fns, d.fn, top, partial(self._action_host, model, config, d))
                return {"function": value} if "$fn" in value or "$host" in value else value

            resource["actions"] = _actions_json(name, model, config["actions"], action_fn)
        if "set_password" in config:
            resource["set_password"] = config["set_password"]
        if "bulk_delete" in config:
            resource["bulk_delete"] = config["bulk_delete"]
        raw: dict[str, Any] = dict(config)
        for key in ("title", "group", "icon", "allow", "perms", "label", "field_permissions"):
            if key in raw:
                value = raw[key]
                resource[key] = value if isinstance(value, str) else list(value)
        if "inlines" in config:
            resource["inlines"] = [self._inline_json(name, d, fns) for d in _inline_defs(name, config["inlines"])]
        return resource

    def _inline_json(self, parent: str, d: InlineDef, fns: _Fns) -> dict[str, Any]:
        child = d.model.__name__
        out: dict[str, Any] = {"model": child, "save": d.save, "style": d.style, "max": d.max, "collapsed": d.collapsed, "delete": d.delete}
        for key in ("via", "permission", "label"):
            if (value := getattr(d, key)) is not None:
                out[key] = value
        page = self._resource_json(child, d.model, d.page, fns, top=False)
        for key in d.page:
            page.setdefault(key, None)  # a key that did not lower reaches Rust as null, so that Rust refuses it by name
        if len(page) > 1:
            out["page"] = page
        return out

    def _inline_models(self) -> dict[str, type[Model]]:
        return {d.model.__name__: d.model for parent, (_, config) in self._resources.items() for d in _inline_defs(parent, config.get("inlines", ()))}

    def _config_json(self, registry: Registry | None = None) -> str:
        self._fns = fns = _Fns()
        resources = [self._resource_json(name, model, config, fns) for name, (model, config) in self._resources.items()]
        out: dict[str, Any] = {"base_path": self.base_path, "permissions": self._strategy, "label": self._label, "resources": resources}
        if any(r.get("set_password") for r in resources):
            out["user_table"] = auth_user_table(self._registry if registry is None else registry)
        if self.prefix is not None:
            out["prefix"] = self.prefix
        if self._widgets is not None:
            out["widgets"] = self._widgets
        if self._groups:
            out["groups"] = self._groups
        if self._group_icons:
            out["group_icons"] = self._group_icons
        if self.switcher_settings:
            out["switcher_file"] = self.switcher_settings
        if self._frame_origins:
            out["frame_origins"] = self._frame_origins
        if self._frame_cards:
            out["frame_cards"] = self._frame_cards
        if self._embeds:
            out["embeds"] = self._embeds
        if self._storage is not None:
            out["storage"] = True
        return json.dumps(out, default=_json_default)

    def _host(self, handles: Mapping[str, str] | None = None) -> dict[str, Any]:
        """The host functions: login, the functions of the config under the names that Rust gave them, and the writes, services, actions, files and routes."""
        if handles is None:
            handles = self._site(self._registry).handles
        host: dict[str, Any] = dict(_HOST)
        host.update(self._fns.host(handles))
        models = self._inline_models()
        host["admin.write"] = writer_host(self._resources, models)
        host["admin.view"] = view_host({**models, **{m.__name__: m for m, _ in self._resources.values()}})
        files = {(model.__name__, field): storage for model, config in self._resources.values() for field, storage in _files(model.__name__, config.get("files", {})).items()}
        if files:
            host.update(_file_host(self._storage, files))
        if self._routes:
            host.update(_admin_routes.host({key: fn for key, (fn, _) in self._routes.items()}))
        return host

    def _site(self, registry: Registry) -> _native.AdminSite:
        schema = registry._native if registry._native is not None else registry.prepare()
        try:
            return self._builder.build(schema, self._config_json(registry))
        except ValueError as e:
            raise _admin_error(e) from None

    def check(self) -> list[str]:
        """Raises :class:`AdminConfigError` when the config does not fit the schema. Needs no database.

        Returns the warnings: for example a search field that no index serves. ``monorm admin`` logs the same warnings at start."""
        return list(self._site(self._registry).warnings)

    def url(self, model: type[Model], key: Any) -> str:
        """The path of the page of one record: ``<base_path>/<Model>/<key>``.

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
                raise _admin_error(e) from None
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
            raise _admin_error(e) from None

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
            raise _admin_error(e) from None

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
        ``settings`` then takes ``idle_timeout`` and ``session_timeout`` (whole seconds), ``origins``, ``trusted_proxies``,
        ``base_path`` and ``oidc`` (``issuer``, ``client_id``, ``client_secret``, ``roles_claim``, ``scopes``, ``hd``, ``auto_start``).
        A wrong secret or setting raises :class:`AdminSettingsError` (a ``ValueError``) at the first request."""
        from .db import resolve

        db = resolve(database)
        site = self._site(db._registry)
        return AdminTestClient(site, db, user, secret=secret, settings=settings, ip=ip, host=self._host(site.handles))


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
