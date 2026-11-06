"""Transaction options: `isolation` and `timeout` of `db.transaction()`.

The timeout is one deadline for the whole outermost transaction: waiting for a connection, setup,
statements, idle time and commit. The cases run on Postgres and on SQLite where the behavior is shared.
Cases that need a driver seam (a stalled BEGIN or COMMIT, a failed ROLLBACK) are Rust tests in
crates/engine/tests/transaction_deadline.rs.
"""

import asyncio
import math
import os
import time

import pytest

import monorm

DATABASE_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
SQLITE = 'datasource db { provider = "sqlite" }\nmodel Unused {\n  id Int @id\n}\n'
TIMEOUT = 0.3
# A statement that runs for a long time and stops only when the engine interrupts it.
LONG = {
    "postgres": "SELECT pg_sleep(30)",
    "sqlite": "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 2000000000) SELECT count(*) FROM c",
}
# A statement of a few milliseconds.
SHORT = {
    "postgres": "SELECT pg_sleep(0.1)",
    "sqlite": "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 200000) SELECT count(*) FROM c",
}


class Backend:
    def __init__(self, kind, url):
        self.kind, self.url, self.opened = kind, url, []

    async def open(self, max_connections=4):
        registry = monorm.Registry()
        if self.kind == "sqlite":
            monorm.loads(SQLITE, registry=registry)
        db = await monorm.connect(self.url, registry=registry, default=False, max_connections=max_connections)
        self.opened.append(db)
        return db

    async def count(self, db=None):
        db = db or self.opened[0]
        return (await db.fetch("SELECT count(*) AS n FROM tx_opt"))[0]["n"]


async def make_backend(kind, url):
    backend = Backend(kind, url)
    try:
        first = await backend.open()
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
        raise
    await first.execute("DROP TABLE IF EXISTS tx_opt")
    await first.execute("CREATE TABLE tx_opt (id integer PRIMARY KEY, v integer NOT NULL)")
    return backend


async def close_backend(backend):
    if backend.kind == "postgres":
        await backend.opened[0].execute("DROP TABLE IF EXISTS tx_opt")
    for db in backend.opened:
        await db.close()


@pytest.fixture
async def pg():
    backend = await make_backend("postgres", DATABASE_URL)
    yield backend
    await close_backend(backend)


@pytest.fixture(params=["postgres", "sqlite"])
async def backend(request, tmp_path):
    url = DATABASE_URL if request.param == "postgres" else f"sqlite://{tmp_path / 'tx.db'}"
    result = await make_backend(request.param, url)
    yield result
    await close_backend(result)


def watch(db):
    events = []
    db.on_query(events.append)
    return events


async def open_transactions(db):
    """Postgres sessions of the test database that sit in an open transaction."""
    rows = await db.fetch(
        "SELECT count(*) AS n FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid() AND state LIKE 'idle in transaction%'"
    )
    return rows[0]["n"]


async def test_options_validate_before_sql(backend):
    db = await backend.open()
    events = watch(db)
    bad = [
        {"isolation": "bogus"},
        {"isolation": "READ COMMITTED"},
        {"isolation": 3},
        {"timeout": 0},
        {"timeout": -1},
        {"timeout": math.nan},
        {"timeout": math.inf},
        {"timeout": 1e300},
        {"timeout": True},
        {"timeout": False},
        {"timeout": "1"},
        {"isolation": "serializable", "timeout": 0},
    ]
    for options in bad:
        with pytest.raises(monorm.QueryError):
            async with db.transaction(**options):
                raise AssertionError("the body must not run")
    assert events == []
    # A short duration is a deadline, never "disabled".
    with pytest.raises(monorm.TransactionTimeoutError):
        async with db.transaction(timeout=1e-9):
            await db.execute("SELECT 1")
    async with db.transaction(isolation=None, timeout=None):
        await db.execute("INSERT INTO tx_opt VALUES (1, 1)")
    assert await backend.count(db) == 1


async def test_nested_options_reject(backend):
    db = await backend.open()
    events = watch(db)
    async with db.transaction():
        await db.execute("INSERT INTO tx_opt VALUES (1, 1)")
        before = len(events)
        for options in ({"isolation": "serializable"}, {"timeout": 5}, {"savepoint": False, "timeout": 5}, {"savepoint": False, "isolation": "read_committed"}):
            with pytest.raises(monorm.QueryError):
                async with db.transaction(**options):
                    raise AssertionError("the body must not run")
        assert len(events) == before
        async with db.transaction(isolation=None, timeout=None):
            await db.execute("INSERT INTO tx_opt VALUES (2, 2)")
    assert await backend.count(db) == 2


async def test_outer_no_savepoint_accepts_options(backend):
    db = await backend.open()
    async with db.transaction(savepoint=False, isolation="repeatable_read", timeout=5):
        await db.execute("INSERT INTO tx_opt VALUES (1, 1)")
        async with db.transaction(savepoint=False):
            await db.execute("INSERT INTO tx_opt VALUES (2, 2)")
    assert await backend.count(db) == 2
    with pytest.raises(monorm.TransactionTimeoutError):
        async with db.transaction(savepoint=False, timeout=TIMEOUT):
            await db.execute(LONG[backend.kind])


async def test_isolation_levels(backend):
    db = await backend.open()
    if backend.kind == "sqlite":
        # SQLite runs every level as serializable; it accepts them all.
        for i, level in enumerate(("read_committed", "repeatable_read", "serializable")):
            async with db.transaction(isolation=level):
                await db.execute(f"INSERT INTO tx_opt VALUES ({i}, {i})")
        assert await backend.count(db) == 3
        # A second writer on the same file waits for the first one and runs after its commit.
        other = await backend.open()

        async def second_writer():
            async with other.transaction(isolation="serializable"):
                await other.execute("INSERT INTO tx_opt VALUES (11, 11)")

        async with db.transaction(isolation="serializable"):
            await db.execute("INSERT INTO tx_opt VALUES (10, 10)")
            waiting = asyncio.create_task(second_writer())
            await asyncio.sleep(0.2)
            assert not waiting.done()
        await waiting
        assert await backend.count(db) == 5
        return
    other = await backend.open()
    names = {"read_committed": "read committed", "repeatable_read": "repeatable read", "serializable": "serializable"}
    for level, name in names.items():
        async with db.transaction(isolation=level):
            assert (await db.fetch("SHOW transaction_isolation"))[0]["transaction_isolation"] == name
    await db.execute("INSERT INTO tx_opt VALUES (1, 1)")
    # Read committed sees a commit that happens inside the transaction; repeatable read does not.
    async with db.transaction(isolation="read_committed"):
        assert await backend.count(db) == 1
        await other.execute("INSERT INTO tx_opt VALUES (2, 2)")
        assert await backend.count(db) == 2
    async with db.transaction(isolation="repeatable_read"):
        assert await backend.count(db) == 2
        await other.execute("INSERT INTO tx_opt VALUES (3, 3)")
        assert await backend.count(db) == 2
    assert await backend.count(db) == 3
    # A serializable write skew: one of the two transactions fails and nothing retries it.
    await db.execute("DELETE FROM tx_opt")
    read = [asyncio.Event(), asyncio.Event()]

    async def skew(me):
        async with (db, other)[me].transaction(isolation="serializable"):
            await (db, other)[me].fetch("SELECT count(*) AS n FROM tx_opt")
            read[me].set()
            await asyncio.wait_for(read[1 - me].wait(), 5)
            await (db, other)[me].execute(f"INSERT INTO tx_opt VALUES ({10 + me}, 1)")

    results = await asyncio.gather(skew(0), skew(1), return_exceptions=True)
    failures = [r for r in results if isinstance(r, BaseException)]
    assert len(failures) == 1
    assert isinstance(failures[0], monorm.DatabaseError)
    assert failures[0].sqlstate == "40001"
    assert await backend.count(db) == 1


async def test_isolation_does_not_leak(pg):
    solo = await pg.open(max_connections=1)
    async with solo.transaction(isolation="serializable"):
        assert (await solo.fetch("SHOW transaction_isolation"))[0]["transaction_isolation"] == "serializable"
    async with solo.transaction():
        assert (await solo.fetch("SHOW transaction_isolation"))[0]["transaction_isolation"] == "read committed"
    with pytest.raises(monorm.TransactionTimeoutError):
        async with solo.transaction(isolation="repeatable_read", timeout=TIMEOUT):
            await solo.execute(LONG["postgres"])
    async with solo.transaction():
        assert (await solo.fetch("SHOW transaction_isolation"))[0]["transaction_isolation"] == "read committed"
    assert (await solo.fetch("SHOW transaction_isolation"))[0]["transaction_isolation"] == "read committed"


async def test_timeout_waiting_for_connection(backend):
    solo = await backend.open(max_connections=1)
    events = watch(solo)
    held, release = asyncio.Event(), asyncio.Event()

    async def owner():
        async with solo.transaction():
            await solo.execute("INSERT INTO tx_opt VALUES (1, 1)")
            held.set()
            await asyncio.wait_for(release.wait(), 10)

    task = asyncio.create_task(owner())
    await asyncio.wait_for(held.wait(), 5)
    before = len(events)
    start = time.monotonic()
    with pytest.raises(monorm.TransactionTimeoutError) as caught:
        async with solo.transaction(timeout=TIMEOUT):
            raise AssertionError("the body must not run")
    assert time.monotonic() - start < 3
    assert caught.value.outcome == "rolled_back"
    assert isinstance(caught.value, monorm.DatabaseError)
    assert len(events) == before
    release.set()
    await task
    assert await backend.count(solo) == 1


async def test_timeout_during_begin(backend):
    if backend.kind == "postgres":
        solo = await backend.open(max_connections=1)
        # BEGIN needs a round trip, so a deadline of a nanosecond passes in it or right after it.
        # A stalled BEGIN is a Rust test with a driver seam.
        with pytest.raises(monorm.TransactionTimeoutError) as caught:
            async with solo.transaction(isolation="serializable", timeout=1e-9):
                await solo.execute("SELECT 1")
        assert caught.value.outcome == "rolled_back"
        await asyncio.sleep(0.2)
        assert await open_transactions(await backend.open()) == 0
        async with solo.transaction():
            await solo.execute("INSERT INTO tx_opt VALUES (1, 1)")
        return
    # SQLite: another connection holds the write lock, and BEGIN IMMEDIATE waits for it.
    holder, waiter = await backend.open(), await backend.open()
    held, release = asyncio.Event(), asyncio.Event()

    async def owner():
        async with holder.transaction():
            await holder.execute("INSERT INTO tx_opt VALUES (1, 1)")
            held.set()
            await asyncio.wait_for(release.wait(), 10)

    task = asyncio.create_task(owner())
    await asyncio.wait_for(held.wait(), 5)
    start = time.monotonic()
    with pytest.raises(monorm.TransactionTimeoutError) as caught:
        async with waiter.transaction(timeout=TIMEOUT):
            raise AssertionError("the body must not run")
    assert time.monotonic() - start < 3
    assert caught.value.outcome == "rolled_back"
    release.set()
    await task
    assert await backend.count(waiter) == 1
    async with waiter.transaction():
        await waiter.execute("INSERT INTO tx_opt VALUES (2, 2)")


async def test_timeout_at_transaction_start(pg):
    # The deadline has passed at once, so it ends the start of the transaction: in the acquisition or in
    # BEGIN and the settings. Both leave nothing open when the error arrives. The Rust fake-driver tests
    # pin each phase; a real server cannot slow the settings step deterministically.
    solo = await pg.open(max_connections=1)
    with solo.tenant(7):
        with pytest.raises(monorm.TransactionTimeoutError) as caught:
            async with solo.transaction(timeout=1e-9):
                await solo.execute("SELECT 1")
        assert caught.value.outcome == "rolled_back"
        assert await open_transactions(await pg.open()) == 0
        async with solo.transaction():
            assert (await solo.fetch("SELECT current_setting('app.tenant', true) AS t"))[0]["t"] == "7"


async def test_timeout_running_statement(backend):
    db = await backend.open(max_connections=1)
    start = time.monotonic()
    with pytest.raises(monorm.TransactionTimeoutError) as caught:
        async with db.transaction(timeout=TIMEOUT):
            await db.execute("INSERT INTO tx_opt VALUES (1, 1)")
            await db.execute(LONG[backend.kind])
    assert time.monotonic() - start < 5
    assert caught.value.outcome == "rolled_back"
    assert await backend.count(db) == 0
    if backend.kind == "postgres":
        # The cancel is a client-side deadline: the server holds no SQLSTATE for it.
        assert caught.value.sqlstate in (None, "57014")


async def test_timeout_is_cumulative(backend):
    db = await backend.open(max_connections=1)
    done = 0
    start = time.monotonic()
    with pytest.raises(monorm.TransactionTimeoutError):
        async with db.transaction(timeout=0.6):
            for _ in range(200):
                await db.execute(SHORT[backend.kind])
                done += 1
    assert time.monotonic() - start < 5
    # Each statement is shorter than the deadline; their sum is not.
    assert done >= 1


async def test_timeout_while_body_idle(backend):
    solo = await backend.open(max_connections=1)
    observer = solo if backend.kind == "postgres" else await backend.open()
    observed = asyncio.Event()

    async def observe():
        await asyncio.sleep(0.05)
        # Needs the connection (Postgres) or the write lock (SQLite) that the idle body holds.
        async with observer.transaction():
            await observer.execute("INSERT INTO tx_opt VALUES (7, 7)")
        observed.set()

    task = asyncio.create_task(observe())
    start = time.monotonic()
    with pytest.raises(monorm.TransactionTimeoutError) as caught:
        async with solo.transaction(timeout=TIMEOUT):
            await solo.execute("INSERT INTO tx_opt VALUES (1, 1)")
            # The body stays idle: the engine releases the lease at the deadline, without the body.
            await asyncio.wait_for(observed.wait(), 5)
            await solo.execute("SELECT 1")
    await task
    assert time.monotonic() - start < 5
    assert caught.value.outcome == "rolled_back"
    assert [r["id"] for r in await solo.fetch("SELECT id FROM tx_opt")] == [7]


async def test_timeout_invalidates_savepoints(backend):
    db = await backend.open(max_connections=1)
    events = watch(db)
    with pytest.raises(monorm.TransactionTimeoutError):
        async with db.transaction(timeout=TIMEOUT):
            await db.execute("INSERT INTO tx_opt VALUES (1, 1)")
            try:
                async with db.transaction():
                    await db.execute(LONG[backend.kind])
            except monorm.TransactionTimeoutError:
                pass
            before = len(events)
            # The caught timeout ended the whole transaction: no savepoint, no statement.
            with pytest.raises(monorm.TransactionTimeoutError):
                async with db.transaction():
                    await db.execute("SELECT 1")
            with pytest.raises(monorm.TransactionTimeoutError):
                await db.execute("SELECT 1")
            # The engine refuses each one with the timeout error: no statement runs.
            assert all(e.error and "timeout" in e.error for e in events[before:])
    assert await backend.count(db) == 0
    # An ordinary failure in a savepoint still recovers.
    async with db.transaction():
        await db.execute("INSERT INTO tx_opt VALUES (1, 1)")
        with pytest.raises(monorm.DatabaseError):
            async with db.transaction():
                await db.execute("INSERT INTO tx_opt VALUES (1, 2)")
        await db.execute("INSERT INTO tx_opt VALUES (2, 2)")
    assert await backend.count(db) == 2


async def test_timeout_invalidates_joined_scope(backend):
    db = await backend.open(max_connections=1)
    with pytest.raises(monorm.TransactionTimeoutError):
        async with db.transaction(timeout=TIMEOUT):
            await db.execute("INSERT INTO tx_opt VALUES (1, 1)")
            try:
                async with db.transaction(savepoint=False):
                    await db.execute(LONG[backend.kind])
            except monorm.TransactionTimeoutError:
                pass
            with pytest.raises(monorm.TransactionTimeoutError):
                await db.execute("SELECT 1")
    assert await backend.count(db) == 0


async def test_caught_timeout_prevents_commit(backend):
    db = await backend.open(max_connections=1)
    seen = []
    with pytest.raises(monorm.TransactionTimeoutError) as caught:
        async with db.transaction(timeout=TIMEOUT):
            await db.execute("INSERT INTO tx_opt VALUES (1, 1)")
            await db.on_commit(lambda: seen.append("committed"))
            try:
                await db.execute(LONG[backend.kind])
            except monorm.TransactionTimeoutError:
                pass
    # The body caught the error and left normally: the exit still reports it, and nothing commits.
    assert caught.value.outcome == "rolled_back"
    assert seen == []
    assert await backend.count(db) == 0


async def test_reuse_after_timeout(backend):
    db = await backend.open(max_connections=1)
    for round_ in range(2):
        with pytest.raises(monorm.TransactionTimeoutError):
            async with db.transaction(timeout=TIMEOUT):
                await db.execute(LONG[backend.kind])
        async with db.transaction(timeout=5):
            await db.execute(f"INSERT INTO tx_opt VALUES ({round_}, {round_})")
        assert await db.fetch("SELECT 1 AS one") == [{"one": 1}]
    assert await backend.count(db) == 2


async def test_stale_task_after_timeout(backend):
    db = await backend.open(max_connections=1)
    go, finished = asyncio.Event(), asyncio.Event()
    outcome = []

    async def stale():
        await go.wait()
        try:
            await db.execute("INSERT INTO tx_opt VALUES (99, 99)")
        except Exception as e:
            outcome.append(e)
        finished.set()

    with pytest.raises(monorm.TransactionTimeoutError):
        async with db.transaction(timeout=TIMEOUT):
            # The task inherits the handle of this transaction.
            task = asyncio.create_task(stale())
            await db.execute(LONG[backend.kind])
    async with db.transaction():
        # The new transaction holds the lease the old one had.
        go.set()
        await asyncio.wait_for(finished.wait(), 5)
        await db.execute("INSERT INTO tx_opt VALUES (1, 1)")
    await task
    assert len(outcome) == 1
    assert [r["id"] for r in await db.fetch("SELECT id FROM tx_opt")] == [1]


async def test_callbacks_outside_deadline(backend):
    db = await backend.open(max_connections=1)
    seen = []

    async def slow():
        await asyncio.sleep(0.8)
        seen.append("slow")

    async with db.transaction(timeout=0.5):
        await db.execute("INSERT INTO tx_opt VALUES (1, 1)")
        await db.on_commit(slow)
    assert seen == ["slow"]
    assert await backend.count(db) == 1
