"""`@@scope.by` from the schema language to a query; needs the `monorm-scope` package."""

import asyncio
import os
from pathlib import Path

import monorm
from monorm._packages import resolve_extensions

FIXTURE = Path(__file__).parent.parent.joinpath("fixtures/schema.prisma").read_text()
assert "monorm.scope" in resolve_extensions(), "install the monorm-scope package"


async def run(provider: str, url: str) -> None:
    registry = monorm.Registry()
    models = monorm.loads(FIXTURE.replace('"sqlite"', f'"{provider}"'), registry=registry)
    Order = models["Order"]
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        orders = Order.objects.using(db)
        with monorm.scope(shop=1):
            await orders.insert(total=5)
            assert (await orders.get()).shop_id == 1
        with monorm.scope(shop=2):
            await orders.insert_many([{"total": 6}, {"total": 7, "shop_id": 2}])
            assert await orders.count() == 2
            try:
                await orders.insert(total=8, shop_id=1)
            except monorm.ScopeMismatch as e:
                assert (e.model, e.key, e.expected, e.actual) == ("Order", "shop", "2", "1"), e
            else:
                raise AssertionError("an insert for another scope ran")
        try:
            await orders.insert(total=9)
        except monorm.ScopeMissing as e:
            assert (e.model, e.key) == ("Order", "shop"), e
        else:
            raise AssertionError("an insert with no scope ran")
        for bad in ({"nope": 1}, {"shop": None}, {"shop": [1]}):
            try:
                monorm.scope(**bad)
            except ValueError:
                pass
            else:
                raise AssertionError(f"scope accepted {bad}")
    finally:
        await db.drop_tables()
        await db.close()


async def main() -> None:
    await run("sqlite", "sqlite://:memory:")
    if os.environ.get("ORM_TEST_DATABASE_URL"):
        await run("postgresql", os.environ["ORM_TEST_DATABASE_URL"])
    print("Python scope extension: passed")


asyncio.run(main())
