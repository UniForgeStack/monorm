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
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from json import dumps as _dumps
from typing import TYPE_CHECKING, Any, Literal, Mapping, Sequence, TypedDict

from . import _native
from . import _admin_permissions
from ._admin_host import HOST as _HOST
from ._admin_permissions import RoleSeedReport, SyncReport
from .expr import ColumnRef, Ordering
from .model import Model, registry as default_registry

if TYPE_CHECKING:
    from .db import Database
    from .model import Registry

__all__ = ["Admin", "AdminConfigError", "AdminResponse", "AdminTestClient", "BrandConfig", "BrandLink", "ListConfig", "ResourceConfig", "RoleSeedReport", "SyncReport", "check", "register", "test_client", "url"]

DEFAULT_PREFIX = "/admin/api"


class AdminConfigError(ValueError):
    """The admin config names an unknown field, a list column through a to-many relation, or a bad value."""


class ListConfig(TypedDict, total=False):
    columns: Sequence[ColumnRef[Any]]
    order: Sequence[ColumnRef[Any] | Ordering]
    page_size: int
    """Rows per page, 1 to 200 (50 by default)."""
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


_LIST_KEYS = {"columns", "order", "page_size", "sortable", "search", "search_with"}
_RESOURCE_KEYS = {"list", "title", "group", "allow", "perms", "label", "field_permissions", "actions"}
_BRAND_KEYS = {"name", "logo", "colors", "links", "environment"}
Strategy = Literal["page", "model", "access"]


def _label(model: type[Model], ref: ColumnRef[Any]) -> str:
    if ref._root is not model:
        raise AdminConfigError(f"{ref!r} belongs to {ref._root.__name__}, not to {model.__name__}")
    return ".".join((*ref._path, ref._field.name))


def _list(model: type[Model], config: Mapping[str, Any]) -> dict[str, Any]:
    if unknown := sorted(set(config) - _LIST_KEYS):
        raise AdminConfigError(f"{model.__name__}: unknown list option {unknown[0]!r}")
    out: dict[str, Any] = {}
    if "columns" in config:
        out["columns"] = [_label(model, c) for c in config["columns"]]
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
    ) -> AdminResponse:
        if self._secret is None:
            if json is not None or headers or csrf is not _AUTO or origin is not _AUTO:
                raise TypeError("the `user=` client takes only a method and a path")
            trace = _native.Trace()
            status, body = await self._site.request(self._database._engine, method, path, self._user, trace, self._database, self._host)
            events = trace.take()
            return AdminResponse(status, body, len(events), sql=tuple(e[0] for e in events))
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
        status, reply_headers, text = await router.request(method, path, list(sent.items()), body_text, self.ip)
        for name, value in reply_headers:
            if name.lower() == "set-cookie":
                pair = value.split(";", 1)[0]
                key, _, val = pair.partition("=")
                if "Max-Age=0" in value or not val:
                    self.cookies.pop(key, None)
                else:
                    self.cookies[key] = val
        return AdminResponse(status, text, 0, tuple(reply_headers))

    async def get(self, path: str, **kwargs: Any) -> AdminResponse:
        return await self.request("GET", path, **kwargs)

    async def post(self, path: str, **kwargs: Any) -> AdminResponse:
        return await self.request("POST", path, **kwargs)

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
    navigation groups; a resource names one of them with ``group``.
    ``statement_timeout`` is the time limit in seconds of each request's database statements (15 by default, 0 for none).
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
        groups: Sequence[str] = (),
        statement_timeout: float = 15.0,
    ) -> None:
        if statement_timeout < 0:
            raise AdminConfigError("statement_timeout is a number of seconds, 0 or more (0 sets no limit)")
        self._statement_timeout = statement_timeout
        self._registry = registry
        self.prefix = prefix
        if unknown := sorted(set(brand or {}) - _BRAND_KEYS):
            raise AdminConfigError(f"unknown brand option {unknown[0]!r}")
        self._brand = dict(brand) if brand else None
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
            raw: dict[str, Any] = dict(config)
            for key in ("title", "group", "allow", "perms", "label", "field_permissions", "actions"):
                if key in raw:
                    value = raw[key]
                    resource[key] = value if isinstance(value, str) else list(value)
            resources.append(resource)
        out: dict[str, Any] = {"prefix": self.prefix, "permissions": self._strategy, "label": self._label, "resources": resources}
        if self._brand is not None:
            out["brand"] = self._brand
        if self._groups:
            out["groups"] = self._groups
        if self._statement_timeout != 15.0:
            out["statement_timeout_ms"] = round(self._statement_timeout * 1000)
        return json.dumps(out)

    def _host(self) -> dict[str, Any]:
        """The host functions: login and the ``search_with`` function of each resource."""
        host: dict[str, Any] = dict(_HOST)
        for model, config in self._resources.values():
            search = config.get("list", {}).get("search_with")
            if search is not None:
                host[_search_name(model)] = _search_host(model, search)
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
