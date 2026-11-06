"""Exceptions raised by the ORM."""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from typing import Self

from ._native import (
    DatabaseError,
    IntegrityError,
    LockNotAvailable,
    QueryError,
    SchemaError,
    SchemaWarning,
    TransactionAborted,
    TransactionTimeoutError,
    WriteProtected,
)

__all__ = [
    "ORMError",
    "DatabaseError",
    "IntegrityError",
    "LockNotAvailable",
    "QueryError",
    "SchemaError",
    "SchemaWarning",
    "DoesNotExist",
    "MultipleObjectsReturned",
    "NotLoaded",
    "NotConnected",
    "TransactionAborted",
    "TransactionTimeoutError",
    "TransactionRequired",
    "WriteProtected",
    "VersionConflict",
    "ValidationError",
    "DecodeError",
    "UnfilteredWriteError",
]


class ORMError(Exception):
    """Base class for errors raised by the Python layer."""


class UnfilteredWriteError(ORMError):
    """``update()`` or ``delete()`` on a query set with no ``filter()``, ``exclude()`` or related set.
    Call ``.all()`` to write every row on purpose, or open the database with
    ``connect(url, allow_unfiltered_writes=True)``."""

    def __init__(self, op: str, model: str) -> None:
        super().__init__(
            f"{model}.objects.{op}() has no filter and would change every row; "
            f"call .all() first ({model}.objects.all().{op}(...)) to write every row, "
            "or open the database with connect(url, allow_unfiltered_writes=True)"
        )
        self.op = op
        self.model = model


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
    """No database: call ``await monorm.connect(url)`` first."""


class TransactionRequired(ORMError, RuntimeError):
    """A lock was asked for outside ``db.transaction()``, where it would be released
    as soon as the statement ends."""


class VersionConflict(ORMError):
    """An instance ``update()`` or ``delete()`` found a newer ``@@locking.version`` of
    its row: another writer changed the row after it was loaded."""


class ValidationError(ORMError, ValueError):
    """App code rejects a value: messages per field path, and non-field messages that belong to no field.

    A path joins field names and list indexes with dots: ``address.city``, ``lines.2.qty``.
    A dot in a field name is not escaped, so a name with a dot reads as a nested path.
    ``fields`` maps a path to its messages and keeps the order of first use; ``non_field_messages`` holds the
    messages that belong to no field. ``ValidationError("text")`` is one message with no field.
    """

    fields: dict[str, list[str]]
    non_field_messages: list[str]

    def __init__(
        self,
        fields: Mapping[str, str | Sequence[str]] | str | None = None,
        *,
        non_field_messages: Sequence[str] = (),
    ) -> None:
        self.fields = {}
        self.non_field_messages = []
        if isinstance(fields, str):
            self.non_field_messages.append(fields)
        elif fields is not None:
            for path, texts in fields.items():
                for text in [texts] if isinstance(texts, str) else texts:
                    self.add(path, text)
        self.non_field_messages.extend(non_field_messages)
        super().__init__(self.fields, self.non_field_messages)

    def add(self, path: str | Sequence[str | int], message: str) -> Self:
        """Add ``message`` to the field at ``path`` (a dotted string, or its parts)."""
        key = path if isinstance(path, str) else ".".join(str(part) for part in path)
        self.fields.setdefault(key, []).append(message)
        return self

    def add_non_field_message(self, message: str) -> Self:
        """Add a message that belongs to no field."""
        self.non_field_messages.append(message)
        return self

    def __str__(self) -> str:
        parts = [
            *self.non_field_messages,
            *(f"{path}: {text}" for path, texts in self.fields.items() for text in texts),
        ]
        return "; ".join(parts) if parts else "validation failed"

    def __reduce__(self) -> tuple[object, ...]:
        return (_restore_validation_error, (type(self), self.fields, self.non_field_messages))


def _restore_validation_error(
    cls: type[ValidationError], fields: dict[str, list[str]], non_field_messages: list[str]
) -> ValidationError:
    return cls(fields, non_field_messages=non_field_messages)


class DecodeError(ORMError, ValueError):
    """A stored nested value does not match its type: ``model``, ``field`` and ``path``
    (the subfield names from the field down) say where."""

    def __init__(self, model: str, field: str, path: tuple[str, ...], message: str) -> None:
        self.model = model
        self.field = field
        self.path = path
        self.message = message
        super().__init__(f"{model}.{'.'.join((field, *path))}: {message}")
