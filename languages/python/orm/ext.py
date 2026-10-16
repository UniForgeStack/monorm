"""The host surface for extension language code, next to the public ``orm`` API."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Protocol

from .expr import ColumnRef
from .query import M as ModelT
from .write import Delete

if TYPE_CHECKING:
    from .query import QuerySet

LANGUAGE_CONTRACT = 1

__all__ = ["LANGUAGE_CONTRACT", "ModelLike", "ModelT", "behavior_field", "behavior_records", "hard_delete_statement"]


class ModelLike(Protocol):
    """The ``self`` type of an instance mixin. It has no ``update``: a generated model types it by field."""

    @property
    def pk(self) -> Any: ...


def behavior_records(model: Any, kind: str) -> list[dict[str, Any]]:
    """The prepared behavior records of ``kind`` for ``model`` (a model class or an instance)."""
    meta = model._meta
    records: list[dict[str, Any]] = meta.registry._behavior.get(kind, ())
    return [r for r in records if r["model"] == meta.name]


def behavior_field(model: Any, record: dict[str, Any]) -> ColumnRef[Any]:
    """The column that a behavior record names in its ``field`` key."""
    cls: Any = model if isinstance(model, type) else type(model)
    return ColumnRef(cls, (), cls._meta.fields[record["field"]])


def hard_delete_statement(qs: QuerySet[Any]) -> Delete[Any]:
    """``DELETE`` the matching rows of ``qs`` with the schema defaults dropped and the soft-delete rewrite off."""
    return Delete.build(qs.without_defaults(), hard=True)
