"""`@@scope.by`: one scope for the app and the database; needs the `monorm.scope` package."""

import os

import pytest

import monorm
from artifact_caps import has_extension
from monorm import QueryError

pytestmark = pytest.mark.skipif(not has_extension("monorm.scope", "monorm.timestamps"), reason="requires the scope and timestamps packages")

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
 @@timestamps.updated_at("changed")
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
    with pytest.raises(ValueError, match="needs scope_prefix"):
        monorm.scope(shop=[1, 2])


async def seed(db, Order):
    for shop in (1, 2):
        with monorm.scope(shop=shop):
            await Order.objects.using(db).insert(total=shop * 10)


async def test_unscoped_opens_one_key_for_reads_and_writes_and_closes_after_the_block(orders):
    db, Order = orders
    await seed(db, Order)
    with monorm.scope(shop=1):
        with monorm.unscoped("shop"):
            assert sorted(o.total for o in await Order.objects.using(db).all()) == [10, 20]
            await Order.objects.using(db).insert(shop_id=2, total=21)
            await Order.objects.using(db).filter(Order.shop_id == 2).update(shop_id=3)
        assert [o.total for o in await Order.objects.using(db).all()] == [10]
        with pytest.raises(monorm.ScopeMismatch):
            await Order.objects.using(db).insert(shop_id=2, total=1)
    # no scope value at all: a write inside `unscoped` needs none
    with monorm.unscoped("shop"):
        await Order.objects.using(db).insert(shop_id=9, total=90)
    with pytest.raises(monorm.ScopeMissing):
        await Order.objects.using(db).insert(total=1)
    assert await rows(db, Order) == [(1, 10), (3, 20), (3, 21), (9, 90)]


async def test_unscoped_needs_a_declared_key(orders):
    with pytest.raises(ValueError, match="at least one key"):
        monorm.unscoped()
    with pytest.raises(ValueError, match="no key nope"):
        monorm.unscoped("nope")
    with pytest.raises(ValueError, match="no key nope"):
        monorm.unscoped("shop", "nope")


async def test_nested_unscoped_blocks_add_keys_and_a_prepared_read_never_keeps_a_block(orders):
    db, Order = orders
    await seed(db, Order)
    q = Order.objects.using(db).order_by(Order.id).prepare()
    with monorm.unscoped("shop"):
        with monorm.unscoped("shop"):
            assert [o.total for o in await q()] == [10, 20]
        assert [o.total for o in await q()] == [10, 20]
    with monorm.scope(shop=2):
        assert [o.total for o in await q()] == [20]
        with monorm.unscoped("shop"):
            assert [o.total for o in await q()] == [10, 20]
        assert [o.total for o in await q()] == [20]
    with monorm.scope(shop=1):
        assert [o.total for o in await q()] == [10]


async def test_a_prepared_write_checks_the_scope_unless_unscoped(orders):
    db, Order = orders
    await seed(db, Order)
    with monorm.scope(shop=1):
        await Order.objects.using(db).filter(Order.total == monorm.param("t")).update(total=monorm.param("n")).prepare()(t=10, n=11)
    with monorm.unscoped("shop"):
        await Order.objects.using(db).filter(Order.total == monorm.param("t")).update(total=monorm.param("n")).prepare()(t=20, n=21)
    assert await rows(db, Order) == [(1, 11), (2, 21)]


async def test_unscoped_publishes_the_bypass_setting_on_postgresql(orders):
    db, Order = orders
    events = []
    db.on_query(events.append)
    with monorm.scope(shop=1), monorm.unscoped("shop"):
        await Order.objects.using(db).all()
    await Order.objects.using(db).without_defaults()
    pg = db.url.startswith("postgres")
    assert events[0].settings.get("monorm.bypass") == ("{shop}" if pg else None)
    assert "monorm.bypass" not in events[1].settings


async def test_a_block_in_an_open_transaction_changes_the_setting_and_restores_it(orders):
    db, Order = orders
    if not db.url.startswith("postgres"):
        pytest.skip("settings are PostgreSQL only")
    events = []
    db.on_query(events.append)
    async with db.transaction():
        await Order.objects.using(db).without_defaults()
        with monorm.unscoped("shop"):
            await Order.objects.using(db).without_defaults()
        await Order.objects.using(db).without_defaults()
    seen = [e.settings.get("monorm.bypass") for e in events if "orders" in e.sql]
    assert seen == [None, "{shop}", None], seen


async def test_skip_triggers_raises_on_sqlite_and_publishes_the_setting_on_postgresql(orders):
    db, Order = orders
    if db.url.startswith("sqlite"):
        with pytest.raises(QueryError, match="sqlite has none"):
            with monorm.skip_triggers("timestamps"):
                await Order.objects.using(db).without_defaults()
        return
    events = []
    db.on_query(events.append)
    with monorm.skip_triggers("timestamps"):
        await Order.objects.using(db).without_defaults()
    assert events[0].settings.get("monorm.skip") == "{timestamps}"
    with pytest.raises(ValueError, match="no skippable triggers"):
        monorm.skip_triggers("soft-delete")
    with pytest.raises(ValueError, match="at least one"):
        monorm.skip_triggers()


async def test_skip_triggers_switches_the_timestamps_trigger_off_for_a_bypass_role():
    if not os.environ.get("ORM_TEST_DATABASE_URL"):
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    source = SOURCES["postgresql"].replace('provider = "postgresql"', 'provider = "postgresql"\n bypass_roles = ["postgres"]') + SCHEMA
    registry = monorm.Registry()
    models = monorm.loads(source, registry=registry)
    db = await monorm.connect(url("postgresql"), registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        Order = models["Order"]
        year = "SELECT extract(year FROM changed)::int AS y FROM sc_orders"
        with monorm.scope(shop=1):
            await Order.objects.using(db).insert(total=1)
        await db.execute("UPDATE sc_orders SET changed = '2000-01-01'")
        await db.execute("UPDATE sc_orders SET total = 2")
        touched = (await db.fetch(year))[0]["y"]
        await db.execute("UPDATE sc_orders SET changed = '2000-01-01'")
        with monorm.skip_triggers("timestamps"):
            await db.execute("UPDATE sc_orders SET total = 3")
        kept = (await db.fetch(year))[0]["y"]
        assert touched > 2000, touched
        assert kept == 2000, kept
    finally:
        await db.drop_tables()
        await db.close()
