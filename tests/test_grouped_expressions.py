"""CASE and JSON paths in GROUP BY, HAVING, ORDER BY and DISTINCT: each copy has equal SQL text."""

import os

import pytest

import orm
from orm import func

URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
PLAIN = """
model Item {
  id  BigInt @id @default(autoincrement())
  qty Int
  @@map("grp_items")
}
"""
SQLITE = """
datasource db {
  provider = "sqlite"
}
"""
JSON = """
model Doc {
  id   BigInt @id @default(autoincrement())
  meta Json
  @@map("grp_docs")
}
"""


async def connect(source: str, name: str, url: str):
    registry = orm.Registry()
    model = orm.loads(source, registry=registry)[name]
    db = await orm.connect(url, registry=registry, default=False, max_connections=2)
    await db.drop_tables()
    await db.create_tables()
    return db, model


@pytest.fixture(params=["postgres", "sqlite"])
async def items(request):
    url = URL if request.param == "postgres" else "sqlite://:memory:"
    source = PLAIN if request.param == "postgres" else SQLITE + PLAIN
    try:
        db, Item = await connect(source, "Item", url)
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {URL}: {e}")
    await Item.objects.using(db).insert_many([{"qty": q} for q in (1, 2, 3, 20, 30)])
    yield Item.objects.using(db), Item
    await db.drop_tables()
    await db.close()


@pytest.fixture
async def docs():
    try:
        db, Doc = await connect(JSON, "Doc", URL)
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {URL}: {e}")
    await Doc.objects.using(db).insert_many(
        [{"meta": {"kind": k, "n": n}} for k, n in (("a", 1), ("a", 2), ("b", 3), ("it's \\ $1", 4))]
    )
    yield Doc.objects.using(db), Doc
    await db.drop_tables()
    await db.close()


def size(Item):
    return func.case((Item.qty < 10, "small"), default="large")


async def test_case_in_group_by_having_and_order_by(items):
    objects, Item = items
    case = size(Item)
    rows = await objects.select(case.label("size"), func.count().label("n")).group_by(case).having(func.count() > 2).order_by(case)
    assert [tuple(r) for r in rows] == [("small", 3)]
    rows = await objects.select(case.label("size"), func.count().label("n")).group_by(case).order_by(case.desc())
    assert [tuple(r) for r in rows] == [("small", 3), ("large", 2)]


async def test_case_in_having_matching_the_group(items):
    objects, Item = items
    case = size(Item)
    rows = await objects.select(case.label("size")).group_by(case).having(case == "large")
    assert [tuple(r) for r in rows] == [("large",)]


async def test_case_in_distinct_and_order_by(items):
    objects, Item = items
    case = size(Item)
    assert await objects.select(case).distinct().order_by(case).scalars() == ["large", "small"]


async def test_json_paths_in_group_by_having_order_by_distinct(docs):
    objects, Doc = docs
    kind = Doc.meta["kind"].as_text()
    rows = await objects.select(kind.label("kind"), func.count().label("n")).group_by(kind).order_by(kind)
    assert [tuple(r) for r in rows] == [("a", 2), ("b", 1), ("it's \\ $1", 1)]
    rows = await objects.select(kind.label("kind")).group_by(kind).having(kind == "a")
    assert [tuple(r) for r in rows] == [("a",)]
    assert await objects.select(kind).distinct().order_by(kind).scalars() == ["a", "b", "it's \\ $1"]
    assert await objects.filter(kind == "it's \\ $1").select(Doc.id).scalars() == [4]
    nested = Doc.meta["kind"]
    assert len(await objects.select(nested).distinct().order_by(nested)) == 3
