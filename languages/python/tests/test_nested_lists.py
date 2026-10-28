"""List subfields of a nested type on columns and JSON storage (PostgreSQL: SQLite has no list columns)."""

import os

import pytest

import monorm

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SCHEMA = """
type Item {
  name String
  tags String[]
  ids  BigInt[]
}

model LJ {
  id   Int   @id
  item Item? @storage(json)
  @@map("lj")
}

model LC {
  id   Int   @id
  item Item?
  @@map("lc")
}
"""

BIG = 2**60 + 1


@pytest.fixture
async def world():
    registry = monorm.Registry()
    classes = monorm.loads(SCHEMA, registry=registry)
    try:
        db = await monorm.connect(PG_URL, registry=registry, default=False, max_connections=2)
    except monorm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {PG_URL}: {e}")
    await db.drop_tables()
    await db.create_tables()
    yield db, classes
    await db.drop_tables()
    await db.close()


@pytest.fixture(params=["LJ", "LC"])
def shop(request, world):
    db, classes = world
    return classes[request.param].objects.using(db), classes[request.param], classes


async def test_a_list_subfield_on_columns_storage_round_trips(shop):
    q, S, classes = shop
    await q.insert_many(
        [
            {"id": 1, "item": classes["Item"](name="a", tags=["x", "y"], ids=[1])},
            {"id": 2, "item": {"name": "b", "tags": ["z"], "ids": [2]}},
        ]
    )
    rows = {s.id: s.item for s in await q.order_by(S.id)}
    assert tuple(rows[1].tags) == ("x", "y")
    assert tuple(rows[2].tags) == ("z",)


async def test_a_list_of_typed_elements_keeps_them_exact(shop):
    q, S, classes = shop
    await q.insert_many([{"id": 1, "item": {"name": "a", "tags": [], "ids": [BIG, 3]}}])
    (row,) = await q
    assert tuple(row.item.ids) == (BIG, 3)


async def test_an_empty_list_round_trips(shop):
    q, S, classes = shop
    await q.insert_many([{"id": 1, "item": classes["Item"](name="a", tags=[], ids=[])}])
    (row,) = await q
    assert tuple(row.item.tags) == ()
    assert tuple(row.item.ids) == ()
