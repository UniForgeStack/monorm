"""Soft delete language members for models with ``@@soft_delete.deleted_at``."""

from __future__ import annotations

import weakref
from collections.abc import Iterable
from typing import Any, Self, cast

from monorm import BoundQuerySet, ColumnRef, Delete, QuerySet, Update, unscoped
from monorm.db import Database, resolve
from monorm.ext import (
    ModelLike,
    ModelT,
    behavior_field,
    behavior_records,
    behavior_value,
    bound_query,
    bound_write,
    hard_delete_statement,
)

__all__ = ["sweep_owned"]

_DEFAULT = "deleted"
_SOURCE = "monorm.soft-delete"
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
        row, so this deletes only rows that are already soft-deleted. With ``hard_delete_setting``
        in the datasource the statement sets that setting, and a live row is deleted too."""
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
        the trigger soft-deletes a live row, unless the datasource has ``hard_delete_setting``;
        a soft-deleted row is deleted."""
        bound: SoftDeleteBoundQuerySet[Any] = cast(Any, self.qs().without_defaults(_DEFAULT))
        await bound.hard_delete()

    async def undelete(self: ModelLike) -> None:
        """Clear the soft-delete field of this row and refresh it."""
        await cast(Any, self).update(**{_deleted_at(self)._field.name: None})


def _owned_records(db: Database, models: Iterable[Any] | None) -> list[dict[str, Any]]:
    """The ``owned`` records of the target models, all of them or the ones that ``models`` names (classes or model names)."""
    records = [r for r in behavior_value("extension_records", db.registry) or () if r["source"] == _SOURCE and r["kind"] == "owned"]
    all_ = [r["data"] | {"model": r["model"]} for r in records]
    if models is None:
        return all_
    names = [m if isinstance(m, str) else m._meta.name for m in models]
    known = {r["model"] for r in records}
    unknown = [n for n in names if n not in known]
    if unknown:
        raise ValueError(f"sweep_owned() has no model {', '.join(unknown)}: no owning relation points at it")
    return [r for r in all_ if r["model"] in names]


async def _check_role(db: Database, records: list[dict[str, Any]]) -> None:
    """The repair stamps rows that policies may hide, so it needs a superuser or a role of ``bypass_roles``."""
    sql = next((r["role_sql"] for r in records if r.get("role_sql")), None)
    if sql is not None and not (await db.fetch(sql))[0]["ok"]:
        raise PermissionError("sweep_owned() needs a superuser or a role of bypass_roles")


async def sweep_owned(
    repair: bool = False, models: Iterable[Any] | None = None, *, db: Database | None = None
) -> dict[str, list[dict[str, Any]]]:
    """Find the live rows that every owner has left: all their owners are soft-deleted, and at least one owner was.

    Returns ``{model name: [primary key of each such row, ...]}``; a model with no such row is absent,
    so ``{}`` is a clean database. With ``repair=True``, stamps each row with the current time, as the
    sweep trigger does, and repeats until a pass finds none. A chain of owners needs one pass for each
    level. It raises ``RuntimeError`` when it needs more passes than the number of owning relations plus one;
    the repair runs in one transaction, so the error rolls back all passes.
    The repair returns the rows that it stamped. ``models`` limits the check to some target models (classes or names).

    It runs without the scope filters. On PostgreSQL it refuses a role that is neither a superuser nor
    in ``bypass_roles`` (``PermissionError``), before it reads or writes anything. It takes no lock:
    run it in a quiet window, or run it again.
    """
    db = resolve(db)
    records = _owned_records(db, models)
    keys = sorted({r["key"] for r in behavior_value("scoped_fields", db.registry) or ()})
    found: dict[str, list[dict[str, Any]]] = {}

    async def run() -> None:
        await _check_role(db, records)
        if not repair:
            for r in records:
                if rows := await db.fetch(r["report_sql"]):
                    found[r["model"]] = rows
            return
        async with db.transaction():
            for _ in range(max((r["passes"] for r in records), default=1)):
                changed = False
                for r in records:
                    if rows := await db.fetch(r["repair_sql"]):
                        found.setdefault(r["model"], []).extend(rows)
                        changed = True
                if not changed:
                    return
            raise RuntimeError("sweep_owned(repair=True) did not settle: the owner rows keep changing, or the owning relations form a loop of more than one pass")

    if keys:
        with unscoped(*keys):
            await run()
    else:
        await run()
    return found
