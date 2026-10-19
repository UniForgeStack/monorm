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
from typing import TYPE_CHECKING, Any, Literal, Protocol, TypeVar
from weakref import WeakKeyDictionary

from .expr import Expression
from .hooks import PreparedInsert, PreparedUpdate, decode_field, prepare_insert, prepare_update
from .protection import allow_writes
from .write import InsertOne, Returning, Update, prepare_rows

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
V_co = TypeVar("V_co", covariant=True)
S = TypeVar("S")


class FieldAdapterError(ValueError):
    """An adapted value or statement that the pipeline rejects before any ``prepare`` call."""


@dataclass(frozen=True)
class Pending:
    """What ``validate`` returns for a value that needs ``prepare``.

    ``value`` goes to ``prepare``. ``placeholder`` is a stored value of the right type that checks the
    statement before any ``prepare`` call; it is never written."""

    value: Any
    placeholder: Any


class FieldAdapter(Protocol[V_co, S]):
    """The operations of one adapted field. ``S`` is the stored value, ``V_co`` the value the model shows."""

    def validate(self, value: Any) -> S | Pending:
        """Return the stored value, or :class:`Pending` when ``prepare`` must run. Sync, no I/O."""
        ...

    async def prepare(self, value: Any) -> S:
        """Turn the ``value`` of a :class:`Pending` into the stored value."""
        ...

    def decode(self, stored: S) -> V_co:
        """Turn a loaded stored value into the value the model shows. Sync, no I/O."""
        ...


_ADAPTERS: WeakKeyDictionary[type, dict[str, FieldAdapter[Any, Any]]] = WeakKeyDictionary()
_Pending = dict[str, tuple[FieldAdapter[Any, Any], Any]]


def register_field_adapter(
    model: type[Model], field: str, adapter: FieldAdapter[Any, Any], methods: Mapping[str, Callable[..., Any]] | None = None
) -> None:
    """Adapt ``model.field``: its writes run the pipeline and its loaded value reads through ``decode``.

    ``methods`` become instance methods; a name that a model member already uses raises ``FieldAdapterError``."""
    from .model import _RESERVED

    registered = _ADAPTERS.setdefault(model, {})
    if field in registered:
        raise FieldAdapterError(f"{model.__name__}.{field} already has a field adapter")
    for name in methods or {}:
        if name in _RESERVED or any(name in vars(base) for base in model.__mro__):
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


class _WriteFailure(BaseException):
    """The operation and the stored values that every failed adapted write carries."""

    def __init__(self, operation: PreparedWrite, message: str):
        super().__init__(message)
        self.operation = operation

    @property
    def stored(self) -> Mapping[str, Any]:
        return self.operation.stored


class AdapterWriteError(_WriteFailure, Exception):
    """A ``prepare`` call or the SQL failed. ``stored`` holds the values that ``prepare`` produced.
    After every ``prepare`` call finished, ``operation.execute()`` retries the SQL with no new ``prepare`` call."""

    def __init__(self, operation: PreparedWrite):
        super().__init__(operation, "adapted write failed; inspect error.stored and error.operation")


class AdapterWriteCancelled(_WriteFailure, asyncio.CancelledError):
    def __init__(self, operation: PreparedWrite):
        super().__init__(operation, "adapted write cancelled; inspect error.stored and error.operation")


class PreparedWrite:
    """The ``prepare`` calls of one statement, run once. A failed or cancelled ``prepare`` call is final,
    because a stream cannot be replayed."""

    def __init__(self, pending: _Pending):
        self._pending = dict(pending)
        self._stored: dict[str, Any] = {}
        self._state: Literal["new", "preparing", "ready", "failed"] = "new"
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
            if isinstance(error, Exception):
                raise AdapterWriteError(self) from error
            raise

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
        except Exception as error:
            raise AdapterWriteError(self) from error
        finally:
            self._executing = False


class _AdaptedWrite:
    """Shared state and ``await`` of a write whose values need ``prepare``. The statement is an
    ordinary insert or update over the placeholder values, so ``inspect()`` shows the SQL shape and
    runs no ``prepare`` call."""

    _used: bool
    qs: QuerySet[Any]
    checked: dict[str, Any]
    operation: PreparedWrite

    async def run(self) -> Any:
        raise NotImplementedError

    def __await__(self) -> Generator[Any, None, Any]:
        self._used = True
        return self.run().__await__()


class PendingInsert(_AdaptedWrite, InsertOne[Any]):
    """``await`` runs the ``prepare`` calls, then inserts. ``operation`` keeps the stored values for a retry."""

    def __init__(self, qs: QuerySet[Any], checked: dict[str, Any], pending: _Pending):
        self.qs, self.checked = qs, checked
        # Check the statement now; write protection waits for the await, like any lazy write.
        with allow_writes(qs.model):
            prepare_insert(qs, checked)
        self.operation = PreparedWrite(pending)
        super().__init__(qs, *prepare_rows(qs.model, [checked]))

    def on_conflict(self, *columns: Any, **options: Any) -> Any:
        self._used = True
        raise FieldAdapterError("a value that needs prepare is unsupported in conflict writes")

    async def run(self) -> Any:
        prepared: PreparedInsert[Any] = prepare_insert(self.qs, self.checked)
        return await self.operation.execute(lambda stored: prepared.execute({**self.checked, **stored}))


class PendingUpdate(_AdaptedWrite, Update[Any]):
    """The unique-row update of a value that needs ``prepare``; ``returning()`` gives the rows."""

    def __init__(self, qs: QuerySet[Any], checked: dict[str, Any], pending: _Pending):
        self.qs, self.checked = qs, checked
        with allow_writes(qs.model):
            if not prepare_update(qs, checked).unique:
                raise FieldAdapterError("an update with a value that needs prepare must target one unique row")
        self.operation = PreparedWrite(pending)
        params: list[Any] = []
        super().__init__(qs, qs._mutation_ir("update", params, checked), params)

    def returning(self) -> Returning[Any]:
        self._used = True
        return PendingReturning(self)

    async def run(self, returning: bool = False) -> Any:
        prepared: PreparedUpdate[Any] = prepare_update(self.qs, self.checked)
        return await self.operation.execute(
            lambda stored: prepared.execute({**self.checked, **stored}, returning=returning)
        )


class PendingReturning(Returning[Any]):
    """``PendingUpdate.returning()``: ``await`` gives the rows of the update."""

    __slots__ = ("_update",)

    def __init__(self, update: PendingUpdate):
        super().__init__(update.qs, update._ir, update._params)
        self._update = update

    def __await__(self) -> Generator[Any, None, Any]:
        self._used = True
        return self._update.run(returning=True).__await__()
