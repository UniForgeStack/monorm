"""Nested leaves selected, decoded and checked on JSON and columns storage (PostgreSQL and SQLite)."""

import datetime as dt
import os
from decimal import Decimal

import pytest

import monorm
from monorm import DecodeError

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SCHEMA = """
enum Color {
  RED
  GREEN
  @@storage(text)
}

type Leaves {
  price Decimal
  at    DateTime
  big   BigInt
  color Color
}

type Plain {
  at    DateTime
  big   BigInt
  color Color
}

model TJ {
  id   Int    @id
  item Leaves @storage(json)
  @@map("tj")
}

model TC {
  id   Int    @id
  item Plain
  @@map("tc")
}
"""

LISTS = """
type Tagged {
  tags String[]
}

model TL {
  id   Int     @id
  item Tagged? @storage(json)
  @@map("tl")
}

model TLC {
  id   Int     @id
  item Tagged?
  @@map("tlc")
}
"""

SQLITE = 'datasource db { provider = "sqlite" }\n'
WHEN = dt.datetime(2024, 1, 2, 3, 4, 5, tzinfo=dt.UTC)
LEAVES = {"price": Decimal("12.50"), "at": WHEN, "big": 2**60 + 1, "color": "RED"}


async def open_world(request, schema, drop=True):
    registry = monorm.Registry()
    if request == "sqlite":
        classes = monorm.loads(SQLITE + schema, registry=registry)
        db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    else:
        classes = monorm.loads(schema, registry=registry)
        try:
            db = await monorm.connect(PG_URL, registry=registry, default=False, max_connections=2)
        except monorm.DatabaseError as e:
            pytest.skip(f"Postgres not reachable at {PG_URL}: {e}")
    await db.drop_tables()
    await db.create_tables()
    return db, classes


@pytest.fixture(params=["postgres", "sqlite"])
async def world(request):
    db, classes = await open_world(request.param, SCHEMA)
    yield db, classes, request.param
    await db.drop_tables()
    await db.close()


@pytest.fixture
async def pg_lists():
    db, classes = await open_world("postgres", LISTS)
    yield db, classes
    await db.drop_tables()
    await db.close()


async def test_select_of_every_leaf_type_on_json_storage(world):
    db, classes, _ = world
    J, C = classes["TJ"], classes["TC"]
    await J.objects.using(db).insert_many([{"id": 1, "item": LEAVES}])
    row = await J.objects.using(db).select(J.item.price, J.item.at, J.item.big, J.item.color).first()
    assert row == (Decimal("12.50"), WHEN, 2**60 + 1, classes["Color"].RED)
    plain = {k: v for k, v in LEAVES.items() if k != "price"}
    await C.objects.using(db).insert_many([{"id": 1, "item": plain}])
    row = await C.objects.using(db).select(C.item.at, C.item.big, C.item.color).first()
    assert row == (WHEN, 2**60 + 1, classes["Color"].RED)


async def test_select_of_a_list_leaf_on_json_storage(pg_lists):
    db, classes = pg_lists
    M = classes["TL"]
    await M.objects.using(db).insert_many([{"id": 1, "item": {"tags": ["x", "y"]}}])
    (row,) = await M.objects.using(db).select(M.item.tags)
    assert tuple(row[0]) == ("x", "y")


async def test_select_of_a_list_leaf_on_columns_storage(pg_lists):
    db, classes = pg_lists
    M = classes["TLC"]
    await M.objects.using(db).insert_many([{"id": 1, "item": {"tags": ["x", "y"]}}])
    (row,) = await M.objects.using(db).select(M.item.tags)
    assert tuple(row[0]) == ("x", "y")


async def test_an_unknown_enum_value_in_a_json_cell_raises_decode_error(world):
    db, classes, dialect = world
    M = classes["TJ"]
    await M.objects.using(db).insert_many([{"id": 1, "item": LEAVES}])
    cast = "::jsonb" if dialect == "postgres" else ""
    payload = '{"price":"1","at":"2024-01-02T03:04:05.000000Z","big":"1","color":"BLUE"}' 
    await db.execute(f"UPDATE tj SET item = '{payload}'{cast} WHERE id = 1")
    with pytest.raises(DecodeError, match="item.color"):
        await M.objects.using(db).all()
    with pytest.raises(DecodeError, match="item.color"):
        await M.objects.using(db).select(M.item.color).all()


async def test_an_unknown_enum_value_is_refused_on_write(world):
    db, classes, _ = world
    for name in ("TJ", "TC"):
        item = {**LEAVES, "color": "BLUE"}
        if name == "TC":
            del item["price"]
        with pytest.raises(TypeError, match="not a value of Color"):
            await classes[name].objects.using(db).insert(id=1, item=item)
