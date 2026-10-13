"""A failed statement marks its transaction failed, also when the block catches the error."""
from types import SimpleNamespace

import pytest

import orm
from blog.models import User
from test_sqlite import models


@pytest.fixture(params=["postgres", "sqlite"])
async def case(request, clean):
    if request.param == "postgres":
        yield SimpleNamespace(
            db=clean, model=User, make=lambda i: {"email": f"{i}@x.io", "name": "n"}
        )
        return
    registry, classes = models()
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    yield SimpleNamespace(db=db, model=classes["Author"], make=lambda i: {"email": f"{i}@x.io", "name": "n"})
    await db.drop_tables()
    await db.close()


async def count(c):
    return await c.model.objects.using(c.db).count()


async def duplicate(c):
    await c.model.objects.using(c.db).insert(**c.make("dup"))


async def test_a_caught_error_aborts_the_commit(case):
    c, seen = case, []
    await c.model.objects.using(c.db).insert(**c.make("dup"))
    with pytest.raises(orm.TransactionAborted) as e:
        async with c.db.transaction():
            await c.model.objects.using(c.db).insert(**c.make("kept"))
            with pytest.raises(orm.IntegrityError):
                await duplicate(c)
            await c.db.on_commit(lambda: seen.append("ran"))
    assert isinstance(e.value.__cause__, orm.IntegrityError)
    assert e.value.__cause__.sqlstate == "23505" and "earlier error" in str(e.value)
    assert isinstance(e.value, orm.DatabaseError)
    assert await count(c) == 1 and seen == []


async def test_a_later_statement_raises_transaction_aborted(case):
    c = case
    await c.model.objects.using(c.db).insert(**c.make("dup"))
    with pytest.raises(orm.IntegrityError):
        async with c.db.transaction():
            with pytest.raises(orm.IntegrityError) as first:
                await duplicate(c)
            with pytest.raises(orm.TransactionAborted) as later:
                await c.model.objects.using(c.db).insert(**c.make("later"))
            assert later.value.__cause__ is not first.value
            assert isinstance(later.value.__cause__, orm.IntegrityError)
            with pytest.raises(orm.TransactionAborted):
                await count(c)
            raise first.value
    assert await count(c) == 1


async def test_a_savepoint_in_a_try_keeps_the_other_writes(case):
    c, seen = case, []
    await c.model.objects.using(c.db).insert(**c.make("dup"))
    async with c.db.transaction():
        await c.model.objects.using(c.db).insert(**c.make("before"))
        with pytest.raises(orm.IntegrityError):
            async with c.db.transaction():
                await c.db.on_commit(lambda: seen.append("inner"))
                await duplicate(c)
        await c.model.objects.using(c.db).insert(**c.make("after"))
        await c.db.on_commit(lambda: seen.append("outer"))
    assert await count(c) == 3 and seen == ["outer"]


async def test_a_caught_error_in_a_savepoint_block_aborts_that_savepoint(case):
    c = case
    await c.model.objects.using(c.db).insert(**c.make("dup"))
    async with c.db.transaction():
        with pytest.raises(orm.TransactionAborted):
            async with c.db.transaction():
                with pytest.raises(orm.IntegrityError):
                    await duplicate(c)
        await c.model.objects.using(c.db).insert(**c.make("after"))
    assert await count(c) == 2


async def test_savepoint_false_joins_the_enclosing_transaction(case):
    c = case
    await c.model.objects.using(c.db).insert(**c.make("dup"))
    async with c.db.transaction():
        async with c.db.transaction(savepoint=False):
            await c.model.objects.using(c.db).insert(**c.make("joined"))
    assert await count(c) == 2
    with pytest.raises(orm.TransactionAborted):
        async with c.db.transaction():
            await c.model.objects.using(c.db).insert(**c.make("lost"))
            with pytest.raises(orm.IntegrityError):
                async with c.db.transaction(savepoint=False):
                    await duplicate(c)
    assert await count(c) == 2
    with pytest.raises(orm.TransactionAborted):
        async with c.db.transaction():
            with pytest.raises(ValueError):
                async with c.db.transaction(savepoint=False):
                    raise ValueError("not a database error")
    assert await count(c) == 2
    async with c.db.transaction(savepoint=False):  # no effect on an outermost block
        await c.model.objects.using(c.db).insert(**c.make("outer"))
    assert await count(c) == 3


async def test_an_uncaught_error_rolls_back_and_raises_the_original(case):
    c = case
    await c.model.objects.using(c.db).insert(**c.make("dup"))
    with pytest.raises(orm.IntegrityError) as e:
        async with c.db.transaction():
            await c.model.objects.using(c.db).insert(**c.make("lost"))
            await duplicate(c)
    assert not isinstance(e.value, orm.TransactionAborted)
    assert await count(c) == 1
