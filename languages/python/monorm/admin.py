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

import json
from dataclasses import dataclass
from json import dumps as _dumps
from typing import TYPE_CHECKING, Any, Literal, Mapping, Sequence, TypedDict

from . import _native
from ._admin_host import HOST as _HOST
from .expr import ColumnRef, Ordering
from .model import Model, registry as default_registry

if TYPE_CHECKING:
    from .db import Database
    from .model import Registry

__all__ = ["Admin", "AdminConfigError", "AdminResponse", "AdminTestClient", "ListConfig", "ResourceConfig", "check", "register", "test_client"]

DEFAULT_PREFIX = "/admin/api"


class AdminConfigError(ValueError):
    """The admin config names an unknown field, a list column through a to-many relation, or a bad value."""


class ListConfig(TypedDict, total=False):
    columns: Sequence[ColumnRef[Any]]
    order: Sequence[ColumnRef[Any] | Ordering]
    page_size: int


class ResourceConfig(TypedDict, total=False):
    list: ListConfig


_LIST_KEYS = {"columns", "order", "page_size"}


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
    return out


@dataclass(frozen=True)
class AdminResponse:
    """The answer to one in-memory request."""

    status: int
    text: str
    queries: int = 0
    """Statements the engine ran for this request (counted on the in-memory ``user=`` path only)."""
    headers: tuple[tuple[str, str], ...] = ()
    """The response headers, every ``set-cookie`` kept (login mode only)."""

    def json(self) -> Any:
        return json.loads(self.text)


_SESSION_COOKIE = "__Host-monorm-session"
_CSRF_COOKIE = "__Host-monorm-csrf"
_AUTO: Any = object()


class AdminTestClient:
    """Sends requests to the admin router in memory: no server, no port.

    Two modes:

    - ``user=`` is a plain value that the client trusts, with no login. Its shape is a JSON object:
      ``{"is_superuser": True}`` reads every resource, no user is a 401 and any other user is a 403.
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
    ) -> None:
        if secret is not None and user is not None:
            raise ValueError("test_client takes `user` (no login) or `secret` (login), not both")
        if secret is None and settings is not None:
            raise ValueError("test_client takes `settings` only with `secret`")
        self._site = site
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
            status, body = await self._site.request(self._database._engine, method, path, self._user, trace)
            return AdminResponse(status, body, len(trace.take()))
        router = self._router
        if router is None:
            router = self._router = self._site.client(self._database._engine, self._database, _HOST, self._secret, self._options)
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
    """The resources of one registry."""

    def __init__(self, registry: Registry = default_registry, prefix: str = DEFAULT_PREFIX) -> None:
        self._registry = registry
        self.prefix = prefix
        self._resources: dict[str, tuple[type[Model], ResourceConfig]] = {}

    def register(self, model: type[Model], config: ResourceConfig | None = None) -> None:
        """Makes ``model`` a resource. Once any model is registered, only registered models are resources."""
        if model.__name__ in self._resources:
            raise AdminConfigError(f"{model.__name__} is already registered")
        config = config or {}
        if unknown := sorted(set(config) - {"list"}):
            raise AdminConfigError(f"{model.__name__}: unknown option {unknown[0]!r}")
        self._resources[model.__name__] = (model, config)

    def _config_json(self) -> str:
        resources = []
        for name, (model, config) in self._resources.items():
            resource: dict[str, Any] = {"model": name}
            if "list" in config:
                resource["list"] = _list(model, config["list"])
            resources.append(resource)
        return json.dumps({"prefix": self.prefix, "resources": resources})

    def _site(self, registry: Registry) -> _native.AdminSite:
        schema = registry._native if registry._native is not None else registry.prepare()
        try:
            return _native.admin_site(schema, self._config_json())
        except ValueError as e:
            raise AdminConfigError(str(e)) from None

    def check(self) -> None:
        """Raises :class:`AdminConfigError` when the config does not fit the schema. Needs no database."""
        self._site(self._registry)

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
        return AdminTestClient(self._site(db._registry), db, user, secret=secret, settings=settings, prefix=self.prefix, ip=ip)


_default = Admin()


def register(model: type[Model], config: ResourceConfig | None = None) -> None:
    _default.register(model, config)


def check() -> None:
    _default.check()


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
