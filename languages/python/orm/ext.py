"""The host surface for extension language code, next to the public ``orm`` API."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Callable, Protocol, TypeVar, cast

from .expr import ColumnRef
from .query import M as ModelT
from .write import Delete, Returning

if TYPE_CHECKING:
    from .query import BoundQuerySet, QuerySet

B = TypeVar("B", bound="BoundQuerySet[Any]")

LANGUAGE_CONTRACT = 1

__all__ = [
    "LANGUAGE_CONTRACT",
    "ModelLike",
    "ModelT",
    "behavior_field",
    "behavior_records",
    "bound_query",
    "bound_write",
    "hard_delete_statement",
    "row_query",
]


class ModelLike(Protocol):
    """The ``self`` type of an instance mixin. It has no ``update``: a generated model types it by field."""

    @property
    def pk(self) -> Any: ...

    def qs(self) -> BoundQuerySet[Any]: ...


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
    """``DELETE`` the matching rows of ``qs`` with the planner's delete rewrite off."""
    return Delete.build(qs, hard=True)


def row_query(instance: ModelLike) -> QuerySet[Any]:
    """The query set of the one row of ``instance``: its primary key, no defaults, its database."""
    query: QuerySet[Any] = cast(Any, instance).qs().without_defaults()._query()
    return query


def bound_query(bound: B, change: Callable[[QuerySet[Any]], QuerySet[Any]]) -> B:
    """``bound`` with ``change`` applied to its inner query set, as ``filter()`` does."""
    clone: B = bound._clone(change(bound._qs))
    return clone


async def bound_write(
    bound: BoundQuerySet[ModelT], build: Callable[[QuerySet[ModelT]], Returning[ModelT]]
) -> list[ModelT]:
    """Run the ``RETURNING`` write that ``build`` makes of the bound query; fill the objects
    whose rows it returns and give them in input order."""
    return await bound._write(build)
