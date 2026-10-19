"""Exceptions raised by the ORM."""

from __future__ import annotations

from ._native import (
    DatabaseError,
    IntegrityError,
    LockNotAvailable,
    QueryError,
    SchemaError,
    TransactionAborted,
    WriteProtected,
)

__all__ = [
    "ORMError",
    "DatabaseError",
    "IntegrityError",
    "LockNotAvailable",
    "QueryError",
    "SchemaError",
    "DoesNotExist",
    "MultipleObjectsReturned",
    "NotLoaded",
    "NotConnected",
    "TransactionAborted",
    "TransactionRequired",
    "WriteProtected",
    "VersionConflict",
    "DecodeError",
]


class ORMError(Exception):
    """Base class for errors raised by the Python layer."""


class DoesNotExist(ORMError, LookupError):
    """``get()`` matched no row. Each model has a subclass: ``User.DoesNotExist``."""


class MultipleObjectsReturned(ORMError, LookupError):
    """``get()`` matched more than one row."""


class NotLoaded(ORMError, AttributeError):
    """A relation was accessed on an instance without being loaded first.

    Async code can't load it implicitly on attribute access; use ``load()``,
    or ``await obj.relation`` for to-many relations.
    """


class NotConnected(ORMError, RuntimeError):
    """No database: call ``await orm.connect(url)`` first."""


class TransactionRequired(ORMError, RuntimeError):
    """A lock was asked for outside ``db.transaction()``, where it would be released
    as soon as the statement ends."""


class VersionConflict(ORMError):
    """An instance ``update()`` or ``delete()`` found a newer ``@locking.version`` of
    its row: another writer changed the row after it was loaded."""


class DecodeError(ORMError, ValueError):
    """A stored nested value does not match its type: ``model``, ``field`` and ``path``
    (the subfield names from the field down) say where."""

    def __init__(self, model: str, field: str, path: tuple[str, ...], message: str) -> None:
        self.model = model
        self.field = field
        self.path = path
        self.message = message
        super().__init__(f"{model}.{'.'.join((field, *path))}: {message}")
