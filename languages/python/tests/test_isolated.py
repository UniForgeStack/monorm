"""`monorm.testing.isolated()`: generated test files run through pytest, the outcome is the assertion."""

import pytest

pytest_plugins = ["monorm.testing", "pytester"]

PREAMBLE = '''
import asyncio
import monorm
import pytest
from monorm.testing import isolated, run_on_commit

pytest_plugins = ["monorm.testing"]

SOURCE = """datasource db {{
  provider = "{provider}"
}}
model Item {{
  id   Int    @id @default(autoincrement())
  name String
  @@map("iso_items")
}}
"""
registry = monorm.Registry()
Item = monorm.loads(SOURCE, registry=registry)["Item"]


@pytest.fixture(scope="session", autouse=True)
async def database():
    db = await monorm.connect({url!r}, registry=registry, max_connections=4)
    await db.drop_tables()
    await db.create_tables()
    yield db
    await db.drop_tables()
    await db.close()


async def count():
    return await Item.objects.count()
'''


@pytest.fixture(params=["sqlite", "postgres"])
def run(request, pytester, tmp_path, db):
    provider, url = (
        ("sqlite", f"sqlite://{tmp_path / 'iso.db'}")
        if request.param == "sqlite"
        else ("postgresql", db.url)
    )

    def go(body, *args):
        pytester.makeini("[pytest]\nasyncio_mode = auto\nasyncio_default_fixture_loop_scope = session\nasyncio_default_test_loop_scope = session\n")
        pytester.makepyfile(PREAMBLE.format(provider=provider, url=url) + body)
        return pytester.runpytest_subprocess("-p", "no:cacheprovider", *args)

    return go


def test_rollback_leaves_no_rows_in_both_forms(run):
    result = run("""
@isolated()
async def test_a():
    await Item.objects.insert_many([{"name": "x"}, {"name": "y"}])
    assert await count() == 2

async def test_b():
    assert await count() == 0
    async with isolated():
        await Item.objects.insert_many([{"name": "z"}])
        assert await count() == 1
    assert await count() == 0
""")
    result.assert_outcomes(passed=2)


def test_an_exception_in_the_test_still_rolls_back(run):
    result = run("""
async def test_a():
    with pytest.raises(RuntimeError):
        async with isolated():
            await Item.objects.insert_many([{"name": "x"}])
            raise RuntimeError("boom")
    assert await count() == 0
""")
    result.assert_outcomes(passed=1)


def test_a_nested_transaction_is_a_savepoint(run):
    result = run("""
@isolated()
async def test_a():
    db = monorm.get_database()
    with pytest.raises(RuntimeError):
        async with db.transaction():
            await Item.objects.insert_many([{"name": "gone"}])
            raise RuntimeError
    async with db.transaction():
        await Item.objects.insert_many([{"name": "kept"}])
    assert [i.name for i in await Item.objects.all()] == ["kept"]

async def test_b():
    assert await count() == 0
""")
    result.assert_outcomes(passed=2)


def test_on_commit_callbacks_are_dropped_and_run_on_request(run):
    result = run("""
calls = []

@isolated()
async def test_a():
    db = monorm.get_database()
    async with db.transaction():
        await db.on_commit(lambda: calls.append("inner"))
    await db.on_commit(lambda: calls.append("outer"))
    assert calls == []

@isolated()
async def test_b():
    assert calls == []
    db = monorm.get_database()
    await db.on_commit(lambda: calls.append("run"))
    assert await run_on_commit() == 1
    assert calls == ["run"]
    assert await run_on_commit() == 0

async def test_c():
    assert calls == ["run"]
    with pytest.raises(monorm.TransactionRequired):
        await run_on_commit()
""")
    result.assert_outcomes(passed=3)


def test_truncate_empties_every_table_and_restarts_ids(run):
    result = run("""
@isolated(mode="truncate")
async def test_a():
    async with monorm.get_database().transaction():
        await Item.objects.insert_many([{"name": "x"}, {"name": "y"}])
    assert await count() == 2

@isolated(mode="truncate")
async def test_b():
    assert await count() == 0
    await Item.objects.insert_many([{"name": "x"}])
    assert [i.id for i in await Item.objects.all()] == [1]
""")
    result.assert_outcomes(passed=2)


def test_a_wrong_mode_and_no_default_database_raise(run):
    result = run("""
def test_mode():
    with pytest.raises(ValueError, match="mode"):
        isolated(mode="fast")

async def test_no_database(monkeypatch):
    import monorm.db
    monkeypatch.setattr(monorm.db, "_default", None)
    with pytest.raises(monorm.NotConnected):
        async with isolated():
            pass
""")
    result.assert_outcomes(passed=2)


def test_parallel_savepoints_raise_with_the_hint_under_rollback_only(run):
    result = run("""
async def both():
    db = monorm.get_database()

    async def one(i):
        async with db.transaction():
            await asyncio.sleep(0.05)
            await Item.objects.insert_many([{"name": str(i)}])

    results = await asyncio.gather(one(1), one(2), return_exceptions=True)
    for r in results:
        if isinstance(r, BaseException):
            raise r

@isolated()
async def test_rollback():
    with pytest.raises(monorm.QueryError, match=r'isolated\\(mode="truncate"\\)'):
        await both()
""")
    result.assert_outcomes(passed=1)


def test_the_fixture_the_mark_and_the_ini_option(run, pytester):
    result = run("""
async def test_fixture(monorm_db):
    assert monorm_db is monorm.get_database()
    await Item.objects.insert_many([{"name": "x"}])

@pytest.mark.monorm(mode="truncate")
async def test_mark():
    assert await count() == 0
    async with monorm.get_database().transaction():
        await Item.objects.insert_many([{"name": "x"}])

async def test_nothing_is_left():
    assert await count() == 0
""")
    result.assert_outcomes(passed=3)
    pytester.makeini("[pytest]\nasyncio_mode = auto\nmonorm_autouse = true\nasyncio_default_fixture_loop_scope = session\nasyncio_default_test_loop_scope = session\n")
    result = pytester.runpytest_subprocess("-p", "no:cacheprovider")
    result.assert_outcomes(passed=3)
