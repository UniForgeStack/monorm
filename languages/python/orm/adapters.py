"""Async field adapters: one write pipeline for fields whose value needs I/O before the SQL.

A write runs ``validate`` (sync, no I/O) for every adapted value, then ``await prepare`` for each value
that needs it, then the SQL. ``decode`` runs on read. An extension registers an adapter with
:func:`register_field_adapter` and relies on the pipeline; this language adapter declares the
``async-field-adapter.v1`` capability.

The ORM never deletes a prepared object. If the SQL fails, the error holds the prepared operation, and
``operation.execute()`` retries the SQL with no new ``prepare`` call.
"""

from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable, Generator, Iterable, Mapping
from dataclasses import dataclass
from types import MappingProxyType
from typing import TYPE_CHECKING, Any, Protocol, TypeVar
from weakref import WeakKeyDictionary

from .expr import Expression
from .hooks import PreparedInsert, PreparedUpdate, decode_field, prepare_insert, prepare_update
from .protection import allow_writes

if TYPE_CHECKING:
    from .model import Model
    from .query import QuerySet

__all__ = [
    "AdapterWriteCancelled",
    "AdapterWriteError",
    "FieldAdapter",
    "FieldAdapterError",
    "Pending",
    "PreparedWrite",
    "register_field_adapter",
]

_T = TypeVar("_T")


class FieldAdapterError(ValueError):
    """An adapted value or statement that the pipeline rejects before any ``prepare`` call."""


@dataclass(frozen=True)
class Pending:
    """What ``validate`` returns for a value that needs ``prepare``.

    ``value`` goes to ``prepare``. ``placeholder`` is a stored value of the right type that checks the
    statement before any ``prepare`` call; it is never written."""

    value: Any
    placeholder: Any


class FieldAdapter(Protocol):
    """The operations of one adapted field."""

    def validate(self, value: Any) -> Any:
        """Return the stored value, or :class:`Pending` when ``prepare`` must run. Sync, no I/O."""

    async def prepare(self, value: Any) -> Any:
        """Turn the ``value`` of a :class:`Pending` into the stored value."""

    def decode(self, stored: Any) -> Any:
        """Turn a loaded stored value into the value the model shows. Sync, no I/O."""


_ADAPTERS: WeakKeyDictionary[type, dict[str, FieldAdapter]] = WeakKeyDictionary()
_Pending = dict[str, tuple[FieldAdapter, Any]]


def register_field_adapter(
    model: type[Model], field: str, adapter: FieldAdapter, methods: Mapping[str, Callable[..., Any]] | None = None
) -> None:
    """Adapt ``model.field``: its writes run the pipeline and its loaded value reads through ``decode``.

    ``methods`` become instance methods; a name that a model member already uses raises ``FieldAdapterError``."""
    registered = _ADAPTERS.setdefault(model, {})
    if field in registered:
        raise FieldAdapterError(f"{model.__name__}.{field} already has a field adapter")
    for name in methods or {}:
        if any(name in vars(base) for base in model.__mro__):
            raise FieldAdapterError(f"{model.__name__}.{name}: adapter method collides with a model member")
    decode_field(model, field, adapter.decode)
    registered[field] = adapter
    for name, method in (methods or {}).items():
        setattr(model, name, method)


def split(model: type[Model], values: Mapping[str, Any]) -> tuple[dict[str, Any], _Pending] | None:
    """Validate the adapted values. Return the values that check the statement, and the pending
    ones by field; ``None`` when ``values`` has no adapted field."""
    adapters = _ADAPTERS.get(model)
    if not adapters or not adapters.keys() & values.keys():
        return None
    checked = dict(values)
    pending: _Pending = {}
    for name, value in values.items():
        adapter = adapters.get(name)
        if adapter is None:
            continue
        if isinstance(value, Expression):
            raise FieldAdapterError(f"{model.__name__}.{name}: an adapted field takes no expression")
        result = adapter.validate(value)
        if isinstance(result, Pending):
            pending[name] = (adapter, result.value)
            result = result.placeholder
        checked[name] = result
    return checked, pending


def split_rows(model: type[Model], rows: Iterable[Mapping[str, Any]], statement: str) -> list[Mapping[str, Any]]:
    """Validate the adapted values of every row of a batch write. A value that needs ``prepare`` is not
    supported in a batch yet: it would store the raw input."""
    out: list[Mapping[str, Any]] = []
    for row in rows:
        split_row = split(model, row)
        if split_row is None:
            out.append(row)
            continue
        checked, pending = split_row
        if pending:
            raise FieldAdapterError(
                f"{model.__name__}.{', '.join(pending)}: {statement} takes no value that needs prepare yet"
            )
        out.append(checked)
    return out


class AdapterWriteError(Exception):
    """A ``prepare`` call or the SQL failed. ``stored`` holds the values that ``prepare`` produced.
    After every ``prepare`` call finished, ``operation.execute()`` retries the SQL with no new ``prepare`` call."""

    def __init__(self, operation: PreparedWrite):
        super().__init__("adapted write failed; inspect error.stored and error.operation")
        self.operation = operation

    @property
    def stored(self) -> Mapping[str, Any]:
        return self.operation.stored


class AdapterWriteCancelled(asyncio.CancelledError):
    def __init__(self, operation: PreparedWrite):
        super().__init__("adapted write cancelled; inspect error.stored and error.operation")
        self.operation = operation

    @property
    def stored(self) -> Mapping[str, Any]:
        return self.operation.stored


class PreparedWrite:
    """The ``prepare`` calls of one statement, run once. A failed or cancelled ``prepare`` call is final,
    because a stream cannot be replayed."""

    def __init__(self, pending: _Pending):
        self._pending = dict(pending)
        self._stored: dict[str, Any] = {}
        self._state = "new"
        self._executing = False
        self._statement: Callable[[Mapping[str, Any]], Awaitable[Any]] | None = None

    @property
    def stored(self) -> Mapping[str, Any]:
        """The stored values that ``prepare`` produced so far, by field."""
        return MappingProxyType(self._stored)

    async def prepare(self) -> Mapping[str, Any]:
        if self._state == "ready":
            return self.stored
        if self._state != "new":
            raise FieldAdapterError("prepare is running or failed; its input cannot be replayed")
        self._state = "preparing"
        try:
            for name, (adapter, value) in self._pending.items():
                self._stored[name] = await adapter.prepare(value)
            self._state = "ready"
            return self.stored
        except BaseException as error:
            self._state = "failed"
            if isinstance(error, asyncio.CancelledError):
                raise AdapterWriteCancelled(self) from error
            raise AdapterWriteError(self) from error

    async def execute(self, statement: Callable[[Mapping[str, Any]], Awaitable[_T]] | None = None) -> _T:
        """Run ``prepare`` once, then ``statement(stored)``. Without ``statement``, run the last one again."""
        if statement is not None:
            self._statement = statement
        if self._statement is None:
            raise FieldAdapterError("no statement to run")
        if self._executing:
            raise FieldAdapterError("adapted write already executing")
        self._executing = True
        try:
            stored = await self.prepare()
            result: _T = await self._statement(stored)
            return result
        except (AdapterWriteError, AdapterWriteCancelled):
            raise
        except asyncio.CancelledError as error:
            raise AdapterWriteCancelled(self) from error
        except BaseException as error:
            raise AdapterWriteError(self) from error
        finally:
            self._executing = False


class PendingInsert:
    """``await`` runs the ``prepare`` calls, then inserts. ``operation`` keeps the stored values for a retry."""

    def __init__(self, qs: QuerySet[Any], checked: dict[str, Any], pending: _Pending):
        self.qs, self.checked = qs, checked
        # Check the statement now; write protection waits for the await, like any lazy write.
        with allow_writes(qs.model):
            prepare_insert(qs, checked)
        self.operation = PreparedWrite(pending)

    def on_conflict(self, *columns: Any, **options: Any) -> Any:
        raise FieldAdapterError("a value that needs prepare is unsupported in conflict writes")

    async def run(self) -> Any:
        prepared: PreparedInsert[Any] = prepare_insert(self.qs, self.checked)
        return await self.operation.execute(lambda stored: prepared.execute({**self.checked, **stored}))

    def __await__(self) -> Generator[Any, None, Any]:
        return self.run().__await__()


class PendingUpdate:
    """The unique-row update of a value that needs ``prepare``; ``returning()`` gives the rows."""

    def __init__(self, qs: QuerySet[Any], checked: dict[str, Any], pending: _Pending):
        self.qs, self.checked = qs, checked
        with allow_writes(qs.model):
            if not prepare_update(qs, checked).unique:
                raise FieldAdapterError("an update with a value that needs prepare must target one unique row")
        self.operation = PreparedWrite(pending)
        self.return_rows = False

    def returning(self) -> PendingUpdate:
        self.return_rows = True
        return self

    async def run(self) -> Any:
        prepared: PreparedUpdate[Any] = prepare_update(self.qs, self.checked)
        return await self.operation.execute(
            lambda stored: prepared.execute({**self.checked, **stored}, returning=self.return_rows)
        )

    def __await__(self) -> Generator[Any, None, Any]:
        return self.run().__await__()
