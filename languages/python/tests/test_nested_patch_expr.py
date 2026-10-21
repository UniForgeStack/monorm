"""An expression as the value of a subfield in a JSON patch, on PostgreSQL and SQLite."""

import datetime as dt
import os
from decimal import Decimal

import pytest

import orm

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SQLITE = 'datasource db { provider = "sqlite" }\n'
WHEN = dt.datetime(2024, 1, 2, 3, 4, 5, tzinfo=dt.UTC)
LATER = dt.datetime(2025, 6, 7, 8, 9, 10, tzinfo=dt.UTC)


def schema(sqlite):
    rent = "" if sqlite else "  rent Decimal\n"
    src_rent = "" if sqlite else "  src_rent Decimal\n"
    return f"""
type Address {{
  city    String
  country String
  floor   Int
  big     BigInt
  open    Boolean
  opened  DateTime
{rent}  meta    Json
}}

model JShop {{
  id         Int      @id
  address    Address? @storage(json)
  src_big    BigInt
  src_open   Boolean
  src_opened DateTime
{src_rent}  src_meta   Json
  @@map("jshop")
}}

model CShop {{
  id         Int      @id
  address    Address?
  src_big    BigInt
  src_open   Boolean
  src_opened DateTime
{src_rent}  src_meta   Json
  @@map("cshop")
}}
"""


@pytest.fixture(params=["postgres", "sqlite"])
async def world(request):
    registry = orm.Registry()
    sqlite = request.param == "sqlite"
    if sqlite:
        classes = orm.loads(SQLITE + schema(True), registry=registry)
        db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    else:
        classes = orm.loads(schema(False), registry=registry)
        try:
            db = await orm.connect(PG_URL, registry=registry, default=False, max_connections=2)
        except orm.DatabaseError as e:
            pytest.skip(f"Postgres not reachable at {PG_URL}: {e}")
    await db.drop_tables()
    await db.create_tables()
    yield db, classes, sqlite
    await db.drop_tables()
    await db.close()


@pytest.fixture(params=["JShop", "CShop"])
async def shop(request, world):
    db, classes, sqlite = world
    S = classes[request.param]
    q = S.objects.using(db)
    address = {"city": "Oslo", "country": "Norway", "floor": 1, "big": 1, "open": False, "opened": WHEN, "meta": {"old": True}}
    src = {"src_big": 2**60, "src_open": True, "src_opened": LATER, "src_meta": [1, 2]}
    if not sqlite:
        address["rent"] = Decimal("1.50")
        src["src_rent"] = Decimal("12.50")
    await q.insert_many([{"id": 1, "address": classes["Address"](**address), **src}])
    return q, S, sqlite


async def test_a_json_patch_takes_an_expression_for_each_leaf_type(shop):
    q, S, sqlite = shop
    patch = {"big": S.src_big, "open": S.src_open, "opened": S.src_opened, "meta": S.src_meta}
    if not sqlite:
        patch["rent"] = S.src_rent
    await q.filter(S.id == 1).update(address=patch)
    a = (await q.get(S.id == 1)).address
    assert (a.big, a.open, a.opened, a.meta) == (2**60, True, LATER, [1, 2])
    assert (a.city, a.country, a.floor) == ("Oslo", "Norway", 1)
    if not sqlite:
        assert a.rent == Decimal("12.50")


async def test_a_json_patch_takes_an_expression_over_a_subfield(shop):
    q, S, _ = shop
    await q.filter(S.id == 1).update(address={"city": S.address.country, "floor": S.address.floor + 41})
    a = (await q.get(S.id == 1)).address
    assert (a.city, a.floor, a.country) == ("Norway", 42, "Norway")


async def test_a_json_patch_swaps_two_leaves(shop):
    q, S, _ = shop
    await q.filter(S.id == 1).update(address={"city": S.address.country, "country": S.address.city})
    a = (await q.get(S.id == 1)).address
    assert (a.city, a.country) == ("Norway", "Oslo")
