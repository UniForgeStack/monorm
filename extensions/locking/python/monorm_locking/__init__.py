"""Locking language members for models with ``@@locking.version``."""

from __future__ import annotations

import weakref
from typing import Any, Callable

from monorm import BoundQuerySet, ColumnRef, Condition, QuerySet, Returning, VersionConflict, and_, or_
from monorm.ext import (
    ModelT,
    behavior_field,
    behavior_records,
    bound_atomic,
    bound_fresh,
    bound_guarded,
    bound_model,
    bound_objects,
    key_condition,
)

_SOURCE = "monorm.locking"


# One lookup for each model class, not one for each call.
_VERSIONS: weakref.WeakKeyDictionary[type, tuple[str, ColumnRef[Any]]] = weakref.WeakKeyDictionary()


def _version(model: Any) -> tuple[str, ColumnRef[Any]]:
    """The name and the column of the version field. Only a model with a locking record has the mixin."""
    if model not in _VERSIONS:
        records = [r for r in behavior_records(model, "instance_guards") if r["source"] == _SOURCE]
        if not records:
            raise TypeError(f"{model.__name__} has no @@locking.version attribute")
        _VERSIONS[model] = (records[0]["field"], behavior_field(model, records[0]))
    return _VERSIONS[model]


class LockingQuerySet(QuerySet[ModelT]):
    """No public members: the version check is in the private write seam of the bound set."""


class LockingInstance:
    """No members: an instance write has its guards from ``instance_guards``."""


class LockingBoundQuerySet(BoundQuerySet[ModelT]):
    """A bound write matches the loaded ``(key, version)`` pairs and raises
    :class:`monorm.VersionConflict` when an object's row has another version. The conflict rolls
    the write back, so no row changes: a write of several objects runs in a transaction.
    An instance write already matches the loaded version, so it passes through."""

    __slots__ = ()

    def _held(self, name: str) -> list[ModelT]:
        """One object for each key; two objects of a row with two versions can never both match."""
        seen: dict[Any, ModelT] = {}
        for o in bound_objects(self):
            first = seen.setdefault(o.pk, o)
            if getattr(first, name) != getattr(o, name):
                raise VersionConflict(f"{type(o).__name__} {o.pk!r} is held with two versions")
        return list(seen.values())

    def _pairs(self, held: list[ModelT], name: str, column: ColumnRef[Any]) -> Condition:
        model = bound_model(self)
        return or_(*(and_(key_condition(model, o.pk), column == getattr(o, name)) for o in held))

    async def _write(self, build: Callable[[QuerySet[ModelT]], Returning[ModelT]]) -> list[ModelT]:
        if bound_guarded(self):
            return await super()._write(build)
        name, column = _version(bound_model(self))
        held = self._held(name)
        pairs = self._pairs(held, name, column)

        def matching(qs: QuerySet[ModelT]) -> Returning[ModelT]:
            return build(qs.filter(pairs))

        if len(held) == 1:
            changed = await super()._write(matching)
            if not changed:
                await self._raise_if_stale(held, name, column)
            return changed
        async with bound_atomic(self):
            changed = await super()._write(matching)
            matched = {o.pk for o in changed}
            await self._raise_if_stale([o for o in held if o.pk not in matched], name, column)
            return changed

    async def _raise_if_stale(self, unmatched: list[ModelT], name: str, column: ColumnRef[Any]) -> None:
        """The write left these objects out: raise when their rows have another version. A row that a
        filter or an object that no longer exists left out is not a conflict."""
        if not unmatched:
            return
        for o, row in await bound_fresh(self, unmatched, column):
            if getattr(row, name) != getattr(o, name):
                raise VersionConflict(f"{type(row).__name__} {row.pk!r} was changed since it was loaded")
