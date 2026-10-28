"""The host surface for extension language code, next to the public ``monorm`` API."""

from __future__ import annotations

from contextlib import asynccontextmanager
from typing import TYPE_CHECKING, Any, AsyncIterator, Callable, Protocol, Sequence, TypeVar

from .expr import ColumnRef, Condition
from .query import M as ModelT
from .write import Delete, Returning

if TYPE_CHECKING:
    from .db import Database
    from .model import Registry
    from .query import BoundQuerySet, QuerySet

B = TypeVar("B", bound="BoundQuerySet[Any]")

LANGUAGE_CONTRACT = 1

__all__ = [
    "LANGUAGE_CONTRACT",
    "ModelLike",
    "ModelT",
    "behavior_field",
    "behavior_records",
    "behavior_value",
    "bound_atomic",
    "bound_fresh",
    "bound_guarded",
    "bound_model",
    "bound_objects",
    "bound_query",
    "bound_write",
    "database_of",
    "hard_delete_statement",
    "key_condition",
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


def behavior_value(kind: str, registry: Registry | None = None) -> Any:
    """The prepared behavior record of ``kind`` that is not a list of per-model records, ``None`` if absent."""
    from .model import registry as default

    return (registry if registry is not None else default)._behavior.get(kind)


def behavior_field(model: Any, record: dict[str, Any]) -> ColumnRef[Any]:
    """The column that a behavior record names in its ``field`` key."""
    cls: Any = model if isinstance(model, type) else type(model)
    return ColumnRef(cls, (), cls._meta.fields[record["field"]])


def hard_delete_statement(qs: QuerySet[Any]) -> Delete[Any]:
    """``DELETE`` the matching rows of ``qs`` with the planner's delete rewrite off."""
    return Delete.build(qs, hard=True)


def database_of(instance: Any) -> Database | None:
    """The database that loaded ``instance``; ``None`` when no database did."""
    db: Database | None = instance.__dict__.get("_db")
    return db


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


def bound_objects(bound: BoundQuerySet[ModelT]) -> tuple[ModelT, ...]:
    """The objects of ``bound`` in input order."""
    return bound._objs


def bound_model(bound: BoundQuerySet[ModelT]) -> type[ModelT]:
    """The model that ``bound`` queries."""
    return bound._qs._model


def bound_guarded(bound: BoundQuerySet[Any]) -> bool:
    """Whether ``bound`` is the set of an instance write: it matches the loaded guard values of
    its row already, so a bound-write extension adds no such check."""
    return bound._guarded


def key_condition(model: type[ModelT], key: Any) -> Condition:
    """The condition of the row with the primary key ``key``."""
    from .model import key_match

    return key_match(model, (), model._meta.key, key)


async def bound_fresh(
    bound: BoundQuerySet[ModelT], objs: Sequence[ModelT], *columns: ColumnRef[Any]
) -> list[tuple[ModelT, ModelT]]:
    """Each of ``objs`` whose row the filters of ``bound`` keep, with a new instance of that row that
    has ``columns`` loaded and nothing else. The ``objs`` stay as they are."""
    query = bound._qs.using(bound._database()).filter(bound._condition(list(dict.fromkeys(o.pk for o in objs))))
    rows = {row.pk: row for row in await query._clone(_loads=()).load(*columns)}
    return [(o, rows[o.pk]) for o in objs if o.pk in rows]


def _snapshot(o: Any) -> dict[str, Any]:
    """The state of an object; a merge changes the ``_orm_internal`` dict in place, so copy it."""
    data = dict(o.__dict__)
    if "_orm_internal" in data:
        data["_orm_internal"] = dict(data["_orm_internal"])
    return data


@asynccontextmanager
async def bound_atomic(bound: BoundQuerySet[Any]) -> AsyncIterator[None]:
    """Run the block in a transaction (a savepoint in one). When the block raises, its writes roll
    back, and the objects of ``bound`` get the state that they had before the block."""
    from .db import resolve

    before = [(o, _snapshot(o)) for o in bound._objs]
    try:
        async with resolve(bound._database()).transaction():
            yield
    except BaseException:
        for o, data in before:
            o.__dict__.clear()
            o.__dict__.update(data)
        raise
