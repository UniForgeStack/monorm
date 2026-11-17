"""pytest plugin and test isolation of the ORM: add ``pytest_plugins = ["monorm.testing"]`` to a ``conftest.py``.

``isolated()`` runs a test so that it leaves no rows. It is a decorator, an async context
manager, the ``monorm_db`` fixture and the ``monorm`` mark::

    @isolated()                      # rollback: one transaction, rolled back after the test
    async def test_create(): ...

    async with isolated(mode="truncate"):   # no outer transaction; every table is emptied after
        ...

    @pytest.mark.monorm(mode="truncate")    # the same for a test of the plugin
    async def test_commit(): ...

Set ``monorm_autouse = true`` in the pytest ini file to isolate every async test.
The fixture and the mark need ``pytest-asyncio>=1.1``: it keeps the context variables of an
async fixture for the test.

The ``n_plus_one`` fixture fails a test that sends an N+1 (see :mod:`monorm.debug`)::

    async def test_list_view(n_plus_one):
        await render_customers()   # fails if one query shape runs more than 5 times

``n_plus_one.threshold = 10`` in the test changes the threshold of that test.
"""

from __future__ import annotations

import functools
import inspect
from collections.abc import AsyncIterator, Awaitable, Callable, Generator
from contextlib import asynccontextmanager
from importlib.metadata import PackageNotFoundError, version
from types import TracebackType
from typing import Any, Literal, ParamSpec, TypeVar

import pytest

from .db import Database, _callbacks, _isolated, resolve
from .debug import Report, _scope
from .errors import TransactionRequired

try:
    _asyncio_version = tuple(int(part) for part in version("pytest-asyncio").split(".")[:2] if part.isdigit())
except PackageNotFoundError:
    _asyncio_version = ()
if _asyncio_version and _asyncio_version < (1, 1):
    raise ImportError("monorm.testing needs pytest-asyncio>=1.1: older versions drop the context of an async fixture")

Mode = Literal["rollback", "truncate"]
P = ParamSpec("P")
T = TypeVar("T")


class _Rollback(BaseException):
    """Ends the outer transaction of a test by rolling it back."""


@asynccontextmanager
async def _rollback(db: Database) -> AsyncIterator[None]:
    token = _isolated.set(True)
    try:
        async with db.transaction():
            yield
            raise _Rollback
    except _Rollback:
        pass
    finally:
        _isolated.reset(token)


@asynccontextmanager
async def _truncate(db: Database) -> AsyncIterator[None]:
    try:
        yield
    finally:
        await db._base.truncate_tables()


class Isolated:
    """What :func:`isolated` returns: a decorator for an async test function and an async
    context manager that gives the database."""

    def __init__(self, db: Database | None, mode: Mode) -> None:
        if mode not in ("rollback", "truncate"):
            raise ValueError(f"isolated() mode must be 'rollback' or 'truncate', got {mode!r}")
        self._db = db
        self._mode: Mode = mode
        self._manager: Any = None

    def __call__(self, fn: Callable[P, Awaitable[T]]) -> Callable[P, Awaitable[T]]:
        if not inspect.iscoroutinefunction(fn):
            raise TypeError("isolated() decorates an async test function")

        @functools.wraps(fn)
        async def wrapper(*args: P.args, **kwargs: P.kwargs) -> T:
            async with Isolated(self._db, self._mode):
                return await fn(*args, **kwargs)

        return wrapper

    async def __aenter__(self) -> Database:
        db = resolve(self._db)
        self._manager = (_rollback if self._mode == "rollback" else _truncate)(db)
        await self._manager.__aenter__()
        return db

    async def __aexit__(
        self, exc_type: type[BaseException] | None, exc: BaseException | None, tb: TracebackType | None
    ) -> bool | None:
        manager, self._manager = self._manager, None
        return await manager.__aexit__(exc_type, exc, tb)  # type: ignore[no-any-return]


def isolated(db: Database | None = None, mode: Mode = "rollback") -> Isolated:
    """Isolates a test from the others. ``mode="rollback"`` (the default) opens a transaction on
    ``db`` (the default database when ``None``), binds it to the current context and rolls it
    back at the end, also after an exception; ``db.transaction()`` in the code under test is a
    savepoint, and ``on_commit`` callbacks never run (see :func:`run_on_commit`). Sibling
    savepoints that run in parallel raise ``QueryError``. ``mode="truncate"`` runs the test with
    no outer transaction, then empties every model table and restarts identities: use it for
    code that needs a real commit. Raises ``NotConnected`` when there is no database."""
    return Isolated(db, mode)


async def run_on_commit(db: Database | None = None) -> int:
    """Runs the ``on_commit`` callbacks that the test registered under ``isolated()`` and clears
    them; gives how many ran. Raises ``TransactionRequired`` outside a transaction."""
    pending = _callbacks.get().get(resolve(db)._root)
    if pending is None or pending.ended:
        raise TransactionRequired("run_on_commit(): no open isolated() or transaction block on this database")
    ready = list(pending)
    pending.clear()
    for fn in ready:
        result = fn()
        if inspect.isawaitable(result):
            await result
    return len(ready)


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addini("monorm_autouse", "isolate every async test with isolated() (rollback)", type="bool", default=False)


def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line("markers", "monorm(mode='rollback'): isolate the test with monorm.testing.isolated()")


def _mode(request: pytest.FixtureRequest) -> Mode | None:
    """The isolation mode of a test: from its mark, else rollback when ``monorm_autouse`` is set."""
    mark = request.node.get_closest_marker("monorm")
    if mark is not None:
        return mark.kwargs.get("mode", mark.args[0] if mark.args else "rollback")  # type: ignore[no-any-return]
    return "rollback" if request.config.getini("monorm_autouse") else None


@pytest.fixture
async def monorm_db(request: pytest.FixtureRequest) -> AsyncIterator[Database]:
    """The default database, isolated for the test (rollback, or the mode of the ``monorm`` mark)."""
    if _isolated.get():
        yield resolve(None)
        return
    async with isolated(mode=_mode(request) or "rollback") as db:
        yield db


@pytest.fixture(autouse=True)
async def _monorm_isolation(request: pytest.FixtureRequest) -> AsyncIterator[None]:
    mode = _mode(request)
    if mode is None or "monorm_db" in request.fixturenames or not inspect.iscoroutinefunction(request.node.obj):
        yield
        return
    async with isolated(mode=mode):
        yield


@pytest.fixture
def n_plus_one() -> Generator[Report]:
    """Counts the queries of the test by statement shape; fails the test at teardown
    when one shape ran more than ``threshold`` (default 5) times."""
    report = Report(5)
    token = _scope.set(report)
    try:
        yield report
    finally:
        _scope.reset(token)
    if report.repeated:
        pytest.fail(f"N+1 queries in the test:\n{report.message()}", pytrace=False)
