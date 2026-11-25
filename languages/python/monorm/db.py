"""Connections and transactions."""

from __future__ import annotations

import inspect
import json
import math
from collections.abc import AsyncIterator, Awaitable, Callable, Coroutine, Iterable, Iterator, Mapping, Sequence
from contextlib import AbstractAsyncContextManager, AbstractContextManager, asynccontextmanager, contextmanager
from contextvars import ContextVar
from dataclasses import dataclass, field
from typing import Any, Literal, TypeVar, overload

from . import _native, debug
from .errors import LockNotAvailable, NotConnected, QueryError, TransactionRequired
from .protection import allowed_writes
from .model import Registry, registry

__all__ = ["Database", "Isolation", "QueryEvent", "connect", "get_database", "scope"]

#: The isolation levels of ``db.transaction(isolation=...)``.
Isolation = Literal["read_committed", "repeatable_read", "serializable"]

T = TypeVar("T")

# Postgres `lock_timeout` takes at most 2^31 - 1 ms.
_MAX_TIMEOUT = 2147483.647

# Statements a replica may answer.
_READS = frozenset({"select", "count", "exists", "compound"})

_default: Database | None = None
# For each database (its root) with an open transaction in the current task: the innermost one.
_current_tx: ContextVar[dict[Database, _native.Transaction]] = ContextVar("monorm_current_tx", default={})
# The scope keys that `unscoped()` blocks of the current task have opened.
_open: ContextVar[frozenset[str]] = ContextVar("monorm_open", default=frozenset())
# The extensions whose triggers `skip_triggers()` blocks of the current task skip.
_skip: ContextVar[frozenset[str]] = ContextVar("monorm_skip", default=frozenset())
# The scope index of a key that `unscoped()` opened: no parameter holds its value.
OPEN_INDEX: int = _native.OPEN_INDEX
# For each open database: the extensions with triggers that `skip_triggers()` may skip.
_skippable: dict[Database, frozenset[str]] = {}
# The `scope.<name>` values of default filters.
_scope: ContextVar[dict[str, Any]] = ContextVar("monorm_scope", default={})
# For each open database: the scope keys its schema reads or writes. `scope()` accepts these only.
_declared: dict[Database, frozenset[str] | None] = {}  # None: a hook reads the scope, so any key is valid
# For each database with an open transaction: the on_commit callbacks of the innermost one.
_callbacks: ContextVar[dict[Database, _Callbacks]] = ContextVar("monorm_on_commit", default={})
# True while a query hook runs: the statements it sends give no event.
_in_hook: ContextVar[bool] = ContextVar("monorm_in_hook", default=False)
# Set by `monorm.testing.isolated()` in rollback mode: the outer transaction holds every statement of the test.
_isolated: ContextVar[bool] = ContextVar("monorm_isolated", default=False)
_SIBLING_SAVEPOINTS = "concurrent nested transactions share one connection"
_ISOLATED_HINT = (
    "; the test runs inside isolated() (mode=\"rollback\"), which holds one transaction: "
    "use isolated(mode=\"truncate\") for a test that runs transactions in parallel"
)


class _Callbacks(list[Callable[[], Any]]):
    """The on_commit callbacks of one transaction. ``ended`` is set when it commits or rolls
    back: a task that it started can still see it after that."""

    ended = False


class _BoundCallbacks(_Callbacks):
    """The callback list of a ``bind_request`` block: nothing can wait for a commit that happens
    outside Python, so a callback registered in the block raises."""

    def append(self, fn: Callable[[], Any]) -> None:
        raise TransactionRequired(_BOUND_CALLBACKS)

    def extend(self, fns: Iterable[Callable[[], Any]]) -> None:
        if any(True for _ in fns):
            raise TransactionRequired(_BOUND_CALLBACKS)


_BOUND_CALLBACKS = "on_commit() in a bind_request() block: the commit happens outside Python; do the work after it"


def _check_transaction_options(isolation: object, timeout: object, *, nested: bool) -> None:
    """Rejects a bad option before any SQL; the engine checks the values themselves."""
    if nested and (isolation is not None or timeout is not None):
        raise QueryError(
            "a nested transaction() takes no isolation or timeout; it uses those of the outermost transaction"
        )
    if isolation is not None and not isinstance(isolation, str):
        raise QueryError(f"transaction isolation must be a string, got {type(isolation).__name__}")
    # A bool is an int: it must not turn into 0 or 1 seconds.
    if timeout is not None and (isinstance(timeout, bool) or not isinstance(timeout, (int, float))):
        raise QueryError(f"transaction timeout must be a number of seconds, got {type(timeout).__name__}")


@dataclass(frozen=True, slots=True)
class QueryEvent:
    """One statement the database ran, given to the hooks of :meth:`Database.on_query`."""

    #: The SQL with its parameter placeholders: the statement shape, without values.
    sql: str
    #: When the statement started, in Unix seconds (``time.time()``).
    start: float
    #: How long it ran, in seconds.
    duration: float
    #: Rows returned, or rows affected by a statement that returns no rows.
    rows: int
    #: The database's error message when the statement failed, or ``"cancelled"`` when the
    #: caller stopped waiting for it.
    error: str | None
    #: True for SQL the caller wrote as a string with no parameters (:meth:`Database.execute`):
    #: it can hold literal values. ORM-built SQL and :meth:`Database.fetch` are False.
    raw: bool = False
    #: The database settings in force for the statement, such as ``scope.shop``.
    settings: Mapping[str, str] = field(default_factory=dict)


class Database:
    """A connection pool. Created by :func:`connect`."""

    def __init__(
        self,
        engine: _native.Engine,
        url: str,
        registry: Registry = registry,
        replicas: Sequence[_native.Engine] = (),
        allow_unfiltered_writes: bool = False,
    ) -> None:
        self._base = engine
        self.url = url
        self._registry = registry
        # Hooks live on the root: `primary` views share them.
        self._hooks: dict[object, Callable[[QueryEvent], object]] = {}
        self._replicas = list(replicas)
        self._turn = 0
        self._allow_unfiltered_writes = allow_unfiltered_writes
        # The database a `primary` view belongs to: they share transactions and callbacks.
        self._root: Database = self
        # The engines with settings, by (engine, names, values): one engine for each setting set, so
        # that the statements prepared on it stay valid.
        self._settings_engines: dict[tuple[Any, ...], _native.Engine] = {}
        # The scope keys that a policy or trigger of the database reads; each goes out as `scope.<key>`.
        self._published: tuple[str, ...] = ()

    @property
    def primary(self) -> Database:
        """This database without its replicas: every statement goes to the primary."""
        view = Database(self._base, self.url, self._registry, allow_unfiltered_writes=self._allow_unfiltered_writes)
        view._root = self._root
        return view

    def _with_settings(self, engine: _native.Engine) -> _native.Engine:
        """``engine`` with the database settings of the current blocks: the keys of
        `unscoped()` and the extensions of `skip_triggers()`. Inside a Postgres transaction it
        is always an engine with settings, so that a setting that a block no longer sets
        is reset before the next statement."""
        names: list[str] = []
        values: list[str] = []
        sqlite = self.url.startswith("sqlite://")
        if (skip := _skip.get()) and sqlite:
            raise QueryError("skip_triggers() skips triggers of the database; sqlite has none that a block can skip")
        if (open_ := _open.get()) and not sqlite:
            names.append("monorm.bypass")
            values.append("{" + ",".join(sorted(open_)) + "}")
        allowed = allowed_writes()
        if allowed and not sqlite:
            skip = skip.union(engine.write_rule_skips(list(allowed)))
        if skip:
            names.append("monorm.skip")
            values.append("{" + ",".join(sorted(skip)) + "}")
        if self._root._published and not sqlite:
            current = _scope.get()
            for name in self._root._published:
                if name in current:
                    names.append(f"scope.{name}")
                    values.append(_setting_text(current[name]))
        # Before any block set a setting, no transaction carries one.
        if not names and (sqlite or self._tx() is None or not self._root._settings_engines):
            return engine
        key = (engine, tuple(names), tuple(values))
        held = self._root._settings_engines.get(key)
        if held is None:
            engines = self._root._settings_engines
            # A scope value makes a new key for each tenant; an engine is a thin handle on the pool.
            if len(engines) >= _SETTINGS_ENGINES_MAX:
                del engines[next(iter(engines))]
            held = engines[key] = engine.with_settings(names, values)
        return held

    @property
    def _engine(self) -> _native.Engine:
        """The primary's engine, with the settings of the enclosing blocks."""
        return self._with_settings(self._base)

    def _tx(self) -> _native.Transaction | None:
        return _current_tx.get().get(self._root)

    def _reader(self) -> _native.Engine:
        """The engine for a read: the next replica outside a transaction, else the primary."""
        if not self._replicas or self._tx() is not None:
            return self._engine
        self._turn = (self._turn + 1) % len(self._replicas)
        return self._with_settings(self._replicas[self._turn])

    @contextmanager
    def bind_request(self, tx: _native.Transaction, *, scope: Mapping[str, Any] | None = None) -> Iterator[None]:
        """``with db.bind_request(tx, scope={...}):`` — runs the block on a transaction
        that something else opened and will end, such as the admin service for each host
        call. Every statement of this database in the block uses ``tx``; a ``transaction()``
        block in it is a savepoint; the caller commits or rolls back ``tx``. ``scope`` gives
        the values of :func:`scope`. ``on_commit`` in the block raises
        :class:`~monorm.TransactionRequired`, because the commit happens outside Python.
        Tasks started in the block get the binding."""
        root = self._root
        tx_token = _current_tx.set({**_current_tx.get(), root: tx})
        check_scope(scope or {})
        scope_token = _scope.set({**_scope.get(), **(scope or {})})
        cb_token = _callbacks.set({**_callbacks.get(), root: _BoundCallbacks()})
        try:
            yield
        finally:
            _callbacks.reset(cb_token)
            _scope.reset(scope_token)
            _current_tx.reset(tx_token)

    def on_query(self, hook: Callable[[QueryEvent], object]) -> Callable[[], None]:
        """Call ``hook(event)`` after each statement this database runs (prefetch queries
        from ``load()`` and ``update_many`` batches included), also when it fails or the caller cancels it.
        Returns a function that removes the hook.

        The hook runs in the task that sent the query, after the ORM call ends, so
        context variables (a current span, a request id) are those of the caller. It can
        be a coroutine function: the ORM awaits it. A statement that a hook sends gives
        no event.

        Every hook gets every event. A hook error never replaces the error of the call:
        when the call failed, its own error is raised and each hook error is added to it
        as a note. When the call succeeded, the first hook error propagates to the caller
        and the others are notes on it.
        """
        key = object()
        self._root._hooks[key] = hook

        def remove() -> None:
            self._root._hooks.pop(key, None)

        return remove

    def _call(
        self,
        start: Callable[[_native.Trace | None], Awaitable[T]],
        key: tuple[object, ...] | None = None,
        raw: bool = False,
    ) -> Awaitable[T]:
        """``start(trace)`` starts an engine call. Without hooks or an N+1 scope the
        engine's awaitable comes back as is, untraced. ``key`` (parts of a write's key)
        makes the N+1 finder count the call once; ``raw`` marks its events as raw SQL."""
        tx = self._tx()
        if _in_hook.get() or (not self._root._hooks and debug._scope.get() is None):
            pending = start(None)
            return pending if tx is None else self._restore_cause(pending, tx)
        trace = _native.Trace()
        pending = start(trace)
        if tx is not None:
            pending = self._restore_cause(pending, tx)
        return self._observe(trace, debug.capture(), pending, key, raw)

    async def _restore_cause(self, pending: Awaitable[T], tx: _native.Transaction | None) -> T:
        try:
            return await pending
        except BaseException as error:
            if tx is not None:
                tx.restore_cause(error)
            raise

    async def _observe(
        self,
        trace: _native.Trace,
        origin: tuple[str, str | None] | None,
        call: Awaitable[T],
        key: tuple[object, ...] | None,
        raw: bool,
    ) -> T:
        try:
            result = await call
        except BaseException as error:
            await self._report(trace, origin, error, key, raw)
            raise
        await self._report(trace, origin, None, key, raw)
        return result

    async def _report(
        self,
        trace: _native.Trace,
        origin: tuple[str, str | None] | None,
        failed: BaseException | None,
        key: tuple[object, ...] | None,
        raw: bool,
    ) -> None:
        """Gives the events of a call to the N+1 finder and to every hook. ``failed`` is the
        error of the call: it stays, and the hook errors become notes on it."""
        events = [QueryEvent(*e[:5], raw, dict(e[5])) for e in trace.take()]
        if origin is not None:
            # A write counts by its call, a read by its SQL shape; each key once per engine call.
            call_key = debug.write_key(key) if key is not None else None
            seen: set[str] = set()
            for e in events:
                k = call_key if call_key is not None else e.sql
                if k not in seen:
                    seen.add(k)
                    debug.record(k, e.sql, origin)
        errors: list[Exception] = []
        for hook in list(self._root._hooks.values()):
            for e in events:
                token = _in_hook.set(True)
                try:
                    result = hook(e)
                    if inspect.isawaitable(result):
                        await result
                except Exception as error:
                    errors.append(error)
                finally:
                    _in_hook.reset(token)
        if failed is not None:
            raised, notes = failed, errors
        elif errors:
            raised, notes = errors[0], errors[1:]
        else:
            return
        for note in notes:
            raised.add_note(f"query hook raised {note!r}")
        if failed is None:
            raise raised

    async def _run(
        self, ir: dict[str, Any], params: list[Any], row_cls: type | None = None, db: Database | None = None
    ) -> Any:
        """Runs a query; instances it builds get ``db`` to write back to (``using()``)."""
        op, params = _with_scope(json.dumps(ir), params)
        engine = self._reader() if ir["op"] in _READS else self._engine
        tx, allowed = self._tx(), allowed_writes()
        return await self._call(lambda t: engine.run(op, params, tx, row_cls, db, allowed, t))

    async def _insert(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        conflict: list[str] | None = None,
        update: list[str] | None = None,
        set_: tuple[list[dict[str, Any]], list[Any]] | None = None,
        db: Database | None = None,
        batch_size: int | None = None,
        indexes: list[str] | None = None,
        *,
        returning: bool = True,
        without_defaults: bool | tuple[str, ...] = False,
        model_fields: list[str] | None = None,
    ) -> Any:
        set_json, params = (json.dumps(set_[0]), set_[1]) if set_ is not None else (None, [])
        scope_json, params = _with_scope("{}", params)
        scope = json.dumps(json.loads(scope_json).get("scope", {}))
        tx, allowed = self._tx(), allowed_writes()
        return await self._call(
            lambda t: self._engine.insert(
                model,
                fields,
                rows,
                conflict,
                update,
                set_json,
                params,
                tx,
                db,
                allowed,
                batch_size,
                indexes,
                t,
                returning,
                scope,
                json.dumps(without_defaults),
                model_fields,
            ),
            ("insert", model, fields, conflict),
        )

    async def _copy(self, model: str, fields: list[str], rows: list[list[Any]]) -> int:
        tx, allowed = self._tx(), allowed_writes()
        n: int = await self._call(
            lambda t: self._engine.copy_insert(model, fields, rows, tx, allowed, t), ("copy", model, fields)
        )
        return n

    async def _update_many(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        filters: list[dict[str, Any]],
        params: list[Any],
        returning: bool,
        batch_size: int | None,
        db: Database | None = None,
        without_defaults: bool | tuple[str, ...] = False,
        model_fields: list[str] | None = None,
        many: _native.NativeMany | None = None,
    ) -> Any:
        filters_json, params = _with_scope(json.dumps(filters), params, key="filters")
        tx, allowed = self._tx(), allowed_writes()
        return await self._call(
            lambda t: self._engine.update_many(
                model,
                fields,
                rows,
                filters_json,
                params,
                returning,
                batch_size,
                tx,
                db,
                json.dumps(without_defaults),
                allowed,
                t,
                model_fields,
                many,
            ),
            ("update_many", model, fields, filters_json),
        )

    @asynccontextmanager
    async def transaction(
        self, *, savepoint: bool = True, isolation: Isolation | None = None, timeout: float | None = None
    ) -> AsyncIterator[None]:
        """``async with db.transaction():`` — commits on success, rolls back on error.

        Queries on this database inside the block (in this task, and in tasks it starts)
        run in the transaction. Nested blocks use savepoints.

        A statement that fails marks its transaction (or savepoint) failed, also when
        the block catches the error: later statements and the commit raise
        :class:`~monorm.TransactionAborted` (``__cause__`` is the first error), and nothing
        commits. Catch errors only around a savepoint, a nested ``transaction()`` block,
        whose rollback leaves the enclosing transaction usable.

        ``savepoint=False`` makes a nested block join the enclosing transaction with no
        savepoint: an error that leaves it marks the enclosing transaction failed. On an
        outermost block it has no effect.

        ``isolation`` (``"read_committed"``, ``"repeatable_read"`` or ``"serializable"``)
        sets the isolation level of an outermost transaction. SQLite accepts every level
        and runs it as serializable. ``timeout`` is a number of seconds, finite and above
        zero: one deadline for the whole transaction, counted from the call of
        ``transaction()``. It covers the wait for a connection, every statement, idle time
        in the block, and the commit. When it passes, the engine stops the running
        statement, rolls back, and raises :class:`~monorm.TransactionTimeoutError`; after
        that no statement runs and no commit is sent. A nested block takes no options:
        passing one raises :class:`~monorm.QueryError`, and it uses the options of the
        outermost transaction (also with ``savepoint=False``).
        """
        enclosing = self._tx()
        _check_transaction_options(isolation, timeout, nested=enclosing is not None)
        if enclosing is not None and not savepoint:
            try:
                yield
            except BaseException as e:
                enclosing.mark_failed(f"{type(e).__name__}: {e}", e)
                raise
            return
        outer = enclosing is None
        try:
            tx = await self._restore_cause(self._engine.begin(enclosing, isolation, timeout), enclosing)
        except QueryError as e:
            if _isolated.get() and _SIBLING_SAVEPOINTS in str(e):
                raise QueryError(str(e) + _ISOLATED_HINT) from e
            raise
        token = _current_tx.set({**_current_tx.get(), self._root: tx})
        callbacks = _Callbacks()
        cb_token = _callbacks.set({**_callbacks.get(), self._root: callbacks})
        try:
            yield
        except BaseException:
            callbacks.ended = True
            _callbacks.reset(cb_token)
            _current_tx.reset(token)
            await tx.rollback()
            raise
        callbacks.ended = True
        _callbacks.reset(cb_token)
        _current_tx.reset(token)
        await self._restore_cause(tx.commit(), tx)
        await self._after_commit(callbacks, outer)

    async def _after_commit(self, callbacks: list[Callable[[], Any]], outer: bool) -> None:
        """A released savepoint hands its callbacks to the enclosing transaction."""
        if not outer:
            _callbacks.get()[self._root].extend(callbacks)
            return
        for fn in callbacks:
            result = fn()
            if inspect.isawaitable(result):
                await result

    async def on_commit(self, fn: Callable[[], Awaitable[Any] | Any]) -> None:
        """Call ``fn()`` after the outermost transaction on this database commits; a
        rollback drops it (a rolled-back savepoint drops only the callbacks registered
        inside it). Outside a transaction, ``fn()`` runs at once. An awaitable result is
        awaited. Callbacks run in order, outside the transaction; an error in one goes to
        the caller of ``transaction()`` and the later ones do not run. A task that the
        transaction started and that calls this after the transaction ended raises
        ``TransactionRequired``."""
        callbacks = _callbacks.get().get(self._root)
        if callbacks is not None and callbacks.ended:
            raise TransactionRequired("on_commit(): the transaction of this task has ended")
        if callbacks is not None:
            callbacks.append(fn)
            return
        result = fn()
        if inspect.isawaitable(result):
            await result

    @overload
    def lock(
        self, key: int | str, exclusive: bool = True, *, session: Literal[False] = False, nowait: bool = False
    ) -> Coroutine[Any, Any, bool]: ...
    @overload
    def lock(
        self,
        key: int | str,
        exclusive: bool = True,
        *,
        session: Literal[True],
        nowait: bool = False,
        timeout: float | None = None,
    ) -> AbstractAsyncContextManager[None]: ...
    def lock(
        self,
        key: int | str,
        exclusive: bool = True,
        *,
        session: bool = False,
        nowait: bool = False,
        timeout: float | None = None,
    ) -> Coroutine[Any, Any, bool] | AbstractAsyncContextManager[None]:
        """Take an advisory lock on ``key``: a lock on a name rather than on rows, e.g.
        "only one worker imports this file at a time".

        ``await db.lock(key)`` holds the lock until the transaction ends and must run
        inside ``db.transaction()``. It waits for the lock unless ``nowait``, in which
        case it returns ``False`` instead of waiting.

        ``async with db.lock(key, session=True, timeout=5):`` holds the lock for the
        block, on a connection of its own, with no transaction. It waits at most
        ``timeout`` seconds (forever when ``None``; not at all with ``nowait``) and
        raises ``LockNotAvailable`` when the lock is still held by another session.

        ``exclusive=False`` takes a shared lock (any number of shared holders, but no
        exclusive one). A ``str`` key is hashed to a 64-bit one (the first 8 bytes of its
        BLAKE2b digest, signed big-endian).
        """
        if session:
            return self._session_lock(key, exclusive, nowait, timeout)
        if timeout is not None:
            raise TypeError("lock(timeout=...) needs session=True; a transaction lock waits or uses nowait")
        return self._xact_lock(key, exclusive, nowait)

    def _lock_key(self, key: int | str) -> tuple[int, bytes | None]:
        if isinstance(key, bool) or not isinstance(key, (int, str)):
            raise TypeError(f"lock key must be an int or a str, got {key!r}")
        if isinstance(key, str):
            return 0, key.encode()
        if not -(2**63) <= key < 2**63:
            raise ValueError("lock key must fit in 64 bits")
        return int(key), None

    async def _xact_lock(self, key: int | str, exclusive: bool, nowait: bool) -> bool:
        if self.url.startswith("sqlite://"):
            raise QueryError("sqlite does not support advisory locks")
        tx = self._tx()
        if tx is None:
            raise TransactionRequired(
                "db.lock() outside a transaction would release the lock at once; "
                "run it inside `async with db.transaction():` or use `session=True`"
            )
        k, name = self._lock_key(key)
        return await self._call(lambda t: self._engine.advisory_lock(k, name, bool(exclusive), bool(nowait), tx, t))

    @asynccontextmanager
    async def _session_lock(
        self, key: int | str, exclusive: bool, nowait: bool, timeout: float | None
    ) -> AsyncIterator[None]:
        if self.url.startswith("sqlite://"):
            raise QueryError("sqlite does not support advisory locks")
        k, name = self._lock_key(key)
        if timeout is not None and not (timeout == math.inf or 0 <= timeout <= _MAX_TIMEOUT):
            raise ValueError(f"lock timeout must be a number of seconds from 0 to {_MAX_TIMEOUT}, or inf")
        timeout_ms = None if timeout is None or timeout == math.inf else min(math.ceil(timeout * 1000), 2**31 - 1)
        engine = self._engine
        held = await self._call(lambda t: engine.session_lock(k, name, bool(exclusive), bool(nowait), timeout_ms, t))
        if held is None:
            raise LockNotAvailable(
                f"advisory lock {key!r} is held by another session"
                + ("" if nowait or timeout is None else f" after {timeout}s")
            )
        try:
            yield
        finally:
            await self._call(held.release)

    async def execute(self, sql: str) -> int:
        """Run raw SQL (one or more statements); returns the number of rows affected."""
        tx = self._tx()
        return await self._call(lambda t: self._engine.execute(sql, tx, t), raw=True)

    async def fetch(self, sql: str, *params: Any) -> list[dict[str, Any]]:
        """Run one raw SQL query with parameters; returns its rows as dicts by column name.

        Placeholders are ``$1, $2, ...`` on Postgres and ``?`` on SQLite. A parameter's
        type comes from its Python value (``int`` is ``bigint``, ``str`` is ``text``,
        ``dict`` / ``list`` are JSON); cast in the SQL where the column needs another
        type (``$1::uuid``). Cells come back by the column types the database reports.
        Runs in the current transaction, and query hooks see it.
        """
        tx, args = self._tx(), list(params)
        return await self._call(lambda t: self._engine.fetch(sql, args, tx, t))

    async def _fetch_text(self, sql: str) -> list[tuple[str | None, ...]]:
        return await self._engine.fetch_text(sql, self._tx())

    async def create_tables(self) -> None:
        """Create the whole schema (extensions, tables, constraints, indexes, functions,
        triggers) with ``IF NOT EXISTS`` / ``OR REPLACE`` DDL, in one transaction.

        A development and test helper: it never alters what exists. Use migrations
        (:mod:`monorm.migrations`) for databases that evolve.
        """
        await self._engine.create_tables()

    async def refresh(self, model: type, *, concurrently: bool = False) -> None:
        """``REFRESH MATERIALIZED VIEW`` for a materialized view model (PostgreSQL only).

        ``concurrently=True`` keeps the view readable during the refresh; PostgreSQL needs a
        unique index on the view for it. Raises :class:`monorm.QueryError` for a plain view,
        any other model, and SQLite. Runs in the current transaction.
        """
        meta = getattr(model, "_meta", None) if isinstance(model, type) else None
        if meta is None:
            raise TypeError(f"refresh() takes a model class, got {model!r}")
        await self._engine.refresh(meta.name, concurrently, self._tx())

    async def drop_tables(self) -> None:
        """``DROP TABLE ... CASCADE`` every model table and drop generated functions;
        extensions stay."""
        await self._engine.drop_tables()

    @property
    def registry(self) -> Registry:
        """The read-only models registry that this database was connected with."""
        return self._registry

    async def close(self) -> None:
        """Close the pools of the primary and of each replica."""
        global _default
        if self._root is not self:
            return await self._root.close()
        _declared.pop(self, None)
        _skippable.pop(self, None)
        await self._base.close()
        for replica in self._replicas:
            await replica.close()
        if _default is self:
            _default = None

    def __repr__(self) -> str:
        return f"<Database {self.url.split('@')[-1]}>"


async def connect(
    url: str,
    *,
    max_connections: int = 10,
    default: bool = True,
    registry: Registry = registry,
    replicas: Sequence[str] = (),
    allow_unfiltered_writes: bool = False,
    _disable: tuple[str, ...] = (),
) -> Database:
    """Open a connection pool. Import your model modules first: the schema is compiled
    from the models registered at this point (in ``registry``, the default one unless
    given).

    With ``default=True`` (the default) queries use this database unless
    ``.using(db)`` says otherwise. With ``replicas`` (URLs of read replicas of
    ``url``), reads outside a transaction go to a replica, in turn; writes and every
    statement in a transaction go to ``url``.

    ``update()`` and ``delete()`` on a query set with no caller filter raise
    ``UnfilteredWriteError`` unless it came from ``.all()``.
    With ``allow_unfiltered_writes=True`` this database accepts them.

    ``_disable`` switches database capabilities off (``"ilike"``, ``"update_from_values"``, ...)
    to test the SQL other databases get.
    """
    global _default
    schema = registry.prepare()
    engine = await _native.connect(url, schema, max_connections, list(_disable))
    readers = [await _native.connect(r, schema, max_connections, list(_disable)) for r in replicas]
    db = Database(engine, url, registry, readers, allow_unfiltered_writes)
    _declared[db] = None if engine.scope_keys_open() else frozenset(engine.scope_keys())
    db._published = tuple(sorted(engine.published_scope_keys()))
    _skippable[db] = frozenset(engine.skippable_triggers())
    if default:
        _default = db
    return db


def scope(**values: Any) -> AbstractContextManager[None]:
    """``with monorm.scope(shop=shop.id):`` — the values that ``scope.<name>`` reads in
    default filters (a filter such as ``shop_id == scope.shop``). A query on a model
    whose default filter reads a value that no enclosing ``scope()`` sets raises
    ``QueryError``. Inner values replace outer ones; tasks started in the block get them.

    Raises ``ValueError`` for a key that no open database's schema declares, for ``None``, for an empty
    string and for a list: a scope value is one value per key."""
    check_scope(values)
    return _scope_block(values)


@contextmanager
def _scope_block(values: dict[str, Any]) -> Iterator[None]:
    token = _scope.set({**_scope.get(), **values})
    try:
        yield
    finally:
        _scope.reset(token)


def unscoped(*keys: str) -> AbstractContextManager[None]:
    """``with monorm.unscoped("shop"):`` — turns off the fill, the check and the app read filter
    of each named scope key, in the block and in tasks it starts. On Postgres it also sets
    ``monorm.bypass`` to the open keys for the policies of the database. A write that needs
    a key that is neither open nor set raises :class:`~monorm.ScopeMissing`.

    Needs at least one key, and each key must be declared by the schema of an open database
    (``ValueError`` otherwise). A nested block adds its keys to those of the outer block."""
    if not keys:
        raise ValueError("unscoped() needs at least one key: name each scope key to open")
    check_open(keys)
    return _open_block(frozenset(keys))


@contextmanager
def _open_block(keys: frozenset[str]) -> Iterator[None]:
    token = _open.set(_open.get() | keys)
    try:
        yield
    finally:
        _open.reset(token)


def check_open(keys: Iterable[str]) -> None:
    if _declared and None not in _declared.values():
        valid = frozenset().union(*(k for k in _declared.values() if k is not None))
        if unknown := sorted(set(keys) - valid):
            raise ValueError(f"unscoped() has no key {', '.join(unknown)}: the schema declares {', '.join(sorted(valid)) or 'no scope key'}")


def skip_triggers(*extensions: str) -> AbstractContextManager[None]:
    """``with monorm.skip_triggers("timestamps"):`` — the triggers of the named extensions do
    not run in the block, for a database role that the schema lists in ``bypass_roles``. It
    sets ``monorm.skip``. Only an extension that declares skippable triggers can be named
    (``ValueError`` otherwise); soft-delete and scope triggers never are. A statement on SQLite
    raises :class:`~monorm.QueryError`."""
    if not extensions:
        raise ValueError("skip_triggers() needs at least one extension name")
    if _skippable:
        valid = frozenset().union(*_skippable.values())
        if unknown := sorted(set(extensions) - valid):
            raise ValueError(
                f"skip_triggers() has no skippable triggers for {', '.join(unknown)}: "
                f"the schema declares {', '.join(sorted(valid)) or 'none'}"
            )
    return _skip_block(frozenset(extensions))


@contextmanager
def _skip_block(names: frozenset[str]) -> Iterator[None]:
    token = _skip.set(_skip.get() | names)
    try:
        yield
    finally:
        _skip.reset(token)


def check_scope(values: Mapping[str, Any]) -> None:
    """Refuses a scope value that no statement could use: see :func:`scope`."""
    for key, value in values.items():
        if value is None:
            raise ValueError(f"scope({key}=None): a scope value is never None; leave the key out to set no value")
        if isinstance(value, (list, tuple, set, frozenset)):
            raise ValueError(f"scope({key}=...): a list of values is not available yet; a scope key takes one value")
        if isinstance(value, str) and not value:
            raise ValueError(f"scope({key}=''): a scope value is never empty; a pooled connection reads '' for no value")
    if _declared and None not in _declared.values():
        valid = frozenset().union(*(k for k in _declared.values() if k is not None))
        if unknown := sorted(values.keys() - valid):
            raise ValueError(f"scope() has no key {', '.join(unknown)}: the schema declares {', '.join(sorted(valid)) or 'no scope key'}")


class _Open:
    """The value of a key that `unscoped()` opened: no parameter holds it."""

    def __repr__(self) -> str:
        return "OPEN"


OPEN = _Open()


_SETTINGS_ENGINES_MAX = 1024


def _setting_text(value: Any) -> str:
    """A scope value as the text of a database setting; the policy casts it back to the column type."""
    return ("true" if value else "false") if isinstance(value, bool) else str(value)


def scope_values(extra: Mapping[str, Any] | None = None) -> dict[str, Any]:
    """The scope of a call: ``scope=`` over the ``monorm.scope()`` context, with ``OPEN`` for
    each key of an enclosing `unscoped()` block."""
    values = _scope.get()
    if extra:
        check_scope(extra)
        values = {**values, **extra}
    if open_ := _open.get():
        values = {**values, **dict.fromkeys(open_, OPEN)}
    return values


def scope_shape(scope: Mapping[str, Any]) -> tuple[tuple[str, bool], ...]:
    """The key of a held statement: the scope names, and which of them are open."""
    return tuple((name, value is OPEN) for name, value in scope.items())


def scope_index(base: int, scope: Mapping[str, Any]) -> dict[str, int]:
    """The parameter index of each scope key; ``base`` is the number of parameters before the
    scope values. An open key has no parameter."""
    index: dict[str, int] = {}
    position = base
    for name, value in scope.items():
        if value is OPEN:
            index[name] = OPEN_INDEX
        else:
            index[name] = position
            position += 1
    return index


def scope_params(scope: Mapping[str, Any]) -> list[Any]:
    """The values to append to the parameters: the set keys, in the order of ``scope_index``."""
    return [value for value in scope.values() if value is not OPEN]


def _with_scope(op: str, params: list[Any], key: str | None = None) -> tuple[str, list[Any]]:
    """``op`` (a statement's IR, or with ``key`` a list wrapped under it) with the scope's
    parameter indexes, and ``params`` with the scope's values appended."""
    values = scope_values()
    if not values:
        return op, params
    index = json.dumps(scope_index(len(params), values))
    separator = "," if op[1:-1].strip() else ""
    op = f'{{"{key}":{op},"scope":{index}}}' if key else f'{op[:-1]}{separator}"scope":{index}}}'
    return op, [*params, *scope_params(values)]


def get_database() -> Database:
    if _default is None:
        raise NotConnected("no default database; call `await monorm.connect(url)` first")
    return _default


def resolve(db: Database | None) -> Database:
    return db if db is not None else get_database()
