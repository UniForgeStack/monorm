"""`@@scope.by`: one scope for the app and the database; needs the `monorm.scope` package."""

import os

import pytest

import monorm
from artifact_caps import has_extension
from monorm import QueryError

pytestmark = pytest.mark.skipif(not has_extension("monorm.scope"), reason="requires the monorm-scope package")

SOURCES = {
    "postgresql": 'datasource db {\n provider = "postgresql"\n}\n',
    "sqlite": 'datasource db {\n provider = "sqlite"\n}\n',
}

SCHEMA = """
model Order {
 id Int @id @default(autoincrement())
 shop_id Int
 total Int
 @@scope.by(shop: shop_id)
 @@map("sc_orders")
}
model Plain {
 id Int @id @default(autoincrement())
 name String
 @@map("sc_plain")
}
"""


def url(provider):
    if provider == "sqlite":
        return "sqlite://:memory:"
    u = os.environ.get("ORM_TEST_DATABASE_URL")
    if not u:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    return str(u)


@pytest.fixture(params=["sqlite", "postgresql"])
async def orders(request):
    registry = monorm.Registry()
    models = monorm.loads(SOURCES[request.param] + SCHEMA, registry=registry)
    db = await monorm.connect(url(request.param), registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        yield db, models["Order"]
    finally:
        await db.drop_tables()
        await db.close()


async def rows(db, Order):
    with monorm.scope(shop=1):
        pass
    return sorted((r.shop_id, r.total) for r in await Order.objects.using(db).without_defaults())


async def test_an_insert_takes_the_scope_value_and_reads_see_the_scope(orders):
    db, Order = orders
    with monorm.scope(shop=1):
        mine = await Order.objects.using(db).insert(total=10)
    with monorm.scope(shop=2):
        await Order.objects.using(db).insert(total=20)
    assert mine.shop_id == 1
    with monorm.scope(shop=1):
        assert [o.total for o in await Order.objects.using(db).all()] == [10]
    with monorm.scope(shop=2):
        assert [o.total for o in await Order.objects.using(db).all()] == [20]
    assert await rows(db, Order) == [(1, 10), (2, 20)]


async def test_an_explicit_equal_value_passes_and_another_value_raises(orders):
    db, Order = orders
    with monorm.scope(shop=1):
        await Order.objects.using(db).insert(shop_id=1, total=1)
        with pytest.raises(monorm.ScopeMismatch) as raised:
            await Order.objects.using(db).insert(shop_id=2, total=2)
    e = raised.value
    assert (e.model, e.key, e.expected, e.actual) == ("Order", "shop", "1", "2"), e
    assert await rows(db, Order) == [(1, 1)]


async def test_an_update_that_changes_the_scope_column_raises(orders):
    db, Order = orders
    with monorm.scope(shop=1):
        row = await Order.objects.using(db).insert(total=1)
        with pytest.raises(monorm.ScopeMismatch):
            await Order.objects.using(db).filter(Order.id == row.id).update(shop_id=2)
        await Order.objects.using(db).filter(Order.id == row.id).update(total=5, shop_id=1)
    assert await rows(db, Order) == [(1, 5)]


async def test_insert_many_fills_and_checks_every_row(orders):
    db, Order = orders
    with monorm.scope(shop=3):
        await Order.objects.using(db).insert_many([{"total": 1}, {"total": 2, "shop_id": 3}])
        with pytest.raises(monorm.ScopeMismatch) as raised:
            await Order.objects.using(db).insert_many([{"total": 3}, {"total": 4, "shop_id": 4}])
    assert raised.value.actual == "4"
    assert await rows(db, Order) == [(3, 1), (3, 2)]


async def test_an_upsert_checks_the_row_and_keeps_the_scope_column(orders):
    db, Order = orders
    with monorm.scope(shop=1):
        row = await Order.objects.using(db).insert(total=1)
        with pytest.raises(monorm.ScopeMismatch):
            await Order.objects.using(db).insert(id=row.id, shop_id=9, total=2).on_conflict(Order.id, update=True)
        await Order.objects.using(db).insert(id=row.id, total=7).on_conflict(Order.id, update=True)
    assert await rows(db, Order) == [(1, 7)]


async def test_a_write_without_the_key_raises_scope_missing(orders):
    db, Order = orders
    with pytest.raises(monorm.ScopeMissing) as raised:
        await Order.objects.using(db).insert(total=1)
    assert (raised.value.model, raised.value.key) == ("Order", "shop")
    assert await rows(db, Order) == []


def test_the_errors_are_query_errors():
    assert issubclass(monorm.ScopeMissing, QueryError)
    assert issubclass(monorm.ScopeMismatch, QueryError)


async def test_scope_refuses_what_the_schema_does_not_declare(orders):
    with pytest.raises(ValueError, match="no key 'nope'|no key nope"):
        monorm.scope(nope=1)
    with pytest.raises(ValueError, match="never None"):
        monorm.scope(shop=None)
    with pytest.raises(ValueError, match="not available yet"):
        monorm.scope(shop=[1, 2])
