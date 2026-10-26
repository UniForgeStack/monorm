"""Load installed soft-delete and locking runtime packages through the public ORM API."""
import importlib
from types import ModuleType
from typing import Any
import orm

ID = "orm.soft-delete"


def extension_modules() -> dict[str, ModuleType]:
    return {ident: importlib.import_module(module) for ident, module in (
        (ID, "orm_soft_delete"), ("orm.locking", "orm_locking"))}


def soft_delete_module() -> ModuleType:
    return extension_modules()[ID]


def loads_soft_delete(source: str, *, registry: orm.Registry | None = None) -> dict[str, Any]:
    return orm.loads(source, registry=registry)
