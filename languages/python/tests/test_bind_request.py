"""``Database.bind_request``: host code runs on a transaction that something else opened and will end."""

import asyncio

import pytest

import monorm
from blog.models import User


async def test_statements_in_the_block_use_the_bound_transaction_and_the_caller_ends_it(clean):
    db = clean
    tx = await db._engine.begin(None, None, None)
    with db.bind_request(tx):
        await User.objects.insert(email="ann@example.com", name="Ann")
        async with db.transaction():  # a savepoint of the bound transaction
            await User.objects.insert(email="bob@example.com", name="Bob")
        assert await User.objects.count() == 2
    await tx.rollback()
    assert await User.objects.count() == 0

    tx = await db._engine.begin(None, None, None)
    with db.bind_request(tx):
        await User.objects.insert(email="cy@example.com", name="Cy")
    await tx.commit()
    assert await User.objects.count() == 1


async def test_the_binding_ends_with_the_block_and_follows_tasks_started_in_it(clean):
    db = clean
    tx = await db._engine.begin(None, None, None)
    with db.bind_request(tx):

        async def insert():
            await User.objects.insert(email="ann@example.com", name="Ann")

        await asyncio.create_task(insert())
    await tx.rollback()
    assert db._tx() is None
    assert await User.objects.count() == 0


async def test_scope_values_are_set_in_the_block(clean, monkeypatch):
    db = clean
    monkeypatch.setitem(monorm.db._declared, db, frozenset({"shop"}))  # the blog schema declares no scope key
    tx = await db._engine.begin(None, None, None)
    with db.bind_request(tx, scope={"shop": 7}):
        assert monorm.db._scope.get() == {"shop": 7}
    await tx.rollback()
    assert monorm.db._scope.get() == {}


async def test_on_commit_in_the_block_raises_because_the_commit_happens_outside(clean):
    db = clean
    tx = await db._engine.begin(None, None, None)
    with db.bind_request(tx):
        with pytest.raises(monorm.TransactionRequired, match="bind_request"):
            await db.on_commit(lambda: None)
        with pytest.raises(monorm.TransactionRequired, match="bind_request"):
            async with db.transaction():
                await db.on_commit(lambda: None)
        async with db.transaction():  # a savepoint with no callback is fine
            pass
    await tx.rollback()


async def test_bind_request_takes_no_tenant_argument(clean):
    db = clean
    tx = await db._engine.begin(None, None, None)
    with pytest.raises(TypeError, match="tenant"):
        with db.bind_request(tx, tenant=1):  # type: ignore[call-arg]
            pass
    await tx.rollback()
