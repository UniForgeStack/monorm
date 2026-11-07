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
from typing import TYPE_CHECKING, Any, Mapping, Sequence, TypedDict

from . import _native
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
    queries: int
    """Statements the engine ran for this request."""

    def json(self) -> Any:
        return json.loads(self.text)


class AdminTestClient:
    """Sends requests to the admin router in memory: no server, no port.

    ``user`` is a plain value that the client trusts. Its shape is a JSON object: ``{"is_superuser": True}``
    reads every resource, no user is a 401 and any other user is a 403.
    """

    def __init__(self, site: _native.AdminSite, database: Database, user: Mapping[str, Any] | None) -> None:
        self._site = site
        self._database = database
        self._user = None if user is None else json.dumps(user)

    async def request(self, method: str, path: str) -> AdminResponse:
        trace = _native.Trace()
        status, body = await self._site.request(self._database._engine, method, path, self._user, trace)
        return AdminResponse(status, body, len(trace.take()))

    async def get(self, path: str) -> AdminResponse:
        return await self.request("GET", path)


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

    def test_client(self, database: Database | None = None, *, user: Mapping[str, Any] | None = None) -> AdminTestClient:
        from .db import resolve

        db = resolve(database)
        return AdminTestClient(self._site(db._registry), db, user)


_default = Admin()


def register(model: type[Model], config: ResourceConfig | None = None) -> None:
    _default.register(model, config)


def check() -> None:
    _default.check()


def test_client(database: Database | None = None, *, user: Mapping[str, Any] | None = None) -> AdminTestClient:
    return _default.test_client(database, user=user)


test_client.__test__ = False  # type: ignore[attr-defined]
