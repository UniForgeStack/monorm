"""Soft delete language members for models with ``@@soft_delete.deleted_at``."""

from __future__ import annotations

import weakref
from typing import Any, Self, cast

from orm import BoundQuerySet, ColumnRef, Delete, QuerySet, Update
from orm.ext import (
    ModelLike,
    ModelT,
    behavior_field,
    behavior_records,
    bound_query,
    bound_write,
    hard_delete_statement,
)

_DEFAULT = "deleted"
_SOURCE = "orm.soft-delete"
_FIELDS: weakref.WeakKeyDictionary[type, ColumnRef[Any]] = weakref.WeakKeyDictionary()


def _deleted_at(model: Any) -> ColumnRef[Any]:
    """The soft-delete column of ``model``, found once for each model class."""
    cls: type = model if isinstance(model, type) else type(model)
    column = _FIELDS.get(cls)
    if column is None:
        records = [r for r in behavior_records(cls, "delete_rewrites") if r["source"] == _SOURCE]
        if not records:
            raise TypeError(f"{cls.__name__} has no @@soft_delete.deleted_at attribute")
        column = _FIELDS[cls] = behavior_field(cls, records[0])
    return column


class SoftDeleteQuerySet(QuerySet[ModelT]):
    def hard_delete(self) -> Delete[ModelT]:
        """``DELETE`` every matching row. In database mode, the trigger soft-deletes a live
        row, so this deletes only rows that are already soft-deleted."""
        _deleted_at(self._model)
        return hard_delete_statement(self.without_defaults(_DEFAULT))

    def undelete(self) -> Update[ModelT]:
        """Clear the soft-delete field of the matching soft-deleted rows. ``await``
        gives the number of rows restored; ``.returning()`` gives the rows. It drops the
        ``"deleted"`` default, like ``all_with_deleted()``."""
        column = _deleted_at(self._model)
        return self.without_defaults(_DEFAULT).filter(column.is_not_null()).update(**{column._field.name: None})

    def all_with_deleted(self) -> Self:
        """The rows with the soft-deleted ones. The other defaults, such as a tenant filter, stay."""
        _deleted_at(self._model)
        return self.without_defaults(_DEFAULT)

    def deleted_only(self) -> Self:
        """Only the soft-deleted rows; the other defaults stay."""
        return self.without_defaults(_DEFAULT).filter(_deleted_at(self._model).is_not_null())


class SoftDeleteBoundQuerySet(BoundQuerySet[ModelT]):
    """The bound-safe members: those of ``SoftDeleteQuerySet`` that keep the bound objects."""

    __slots__ = ()

    async def hard_delete(self) -> list[ModelT]:
        """``DELETE`` the rows of the objects, also a live one in application mode. The
        objects whose rows were deleted are filled from them, in input order."""
        _deleted_at(self._qs._model)
        return await bound_write(self, lambda qs: hard_delete_statement(qs.without_defaults(_DEFAULT)).returning())

    async def undelete(self) -> list[ModelT]:
        """Clear the soft-delete field of the soft-deleted rows of the objects; gives the
        restored objects in input order. It drops the ``"deleted"`` default."""
        column = _deleted_at(self._qs._model)
        return await bound_write(
            self,
            lambda qs: (
                qs.without_defaults(_DEFAULT)
                .filter(column.is_not_null())
                .update(**{column._field.name: None})
                .returning()
            ),
        )

    def all_with_deleted(self) -> Self:
        """The objects' rows with the soft-deleted ones. The other defaults stay."""
        _deleted_at(self._qs._model)
        return bound_query(self, lambda qs: qs.without_defaults(_DEFAULT))

    def deleted_only(self) -> Self:
        """Only the soft-deleted rows of the objects; the other defaults stay."""
        column = _deleted_at(self._qs._model)
        return bound_query(self, lambda qs: qs.without_defaults(_DEFAULT).filter(column.is_not_null()))


class SoftDeleteInstance:
    async def hard_delete(self: ModelLike) -> None:
        """``DELETE ... WHERE pk = ...`` also for a soft-delete model. In database mode,
        the trigger soft-deletes a live row; a soft-deleted row is deleted."""
        bound: SoftDeleteBoundQuerySet[Any] = cast(Any, self.qs().without_defaults())
        await bound.hard_delete()

    async def undelete(self: ModelLike) -> None:
        """Clear the soft-delete field of this row and refresh it."""
        await cast(Any, self).update(**{_deleted_at(self)._field.name: None})
