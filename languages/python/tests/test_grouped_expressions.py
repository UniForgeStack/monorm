"""CASE and JSON paths in GROUP BY, HAVING, ORDER BY and DISTINCT: each copy has equal SQL text."""

import os
from decimal import Decimal

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


async def test_json_key_with_quote_backslash_and_dollar_reaches_postgres(docs):
    objects, Doc = docs
    await objects.insert_many([{"meta": {"it's \\ $1 --": "v"}}, {"meta": {"it's \\ $1 --": "v"}}])
    key = Doc.meta["it's \\ $1 --"].as_text()
    rows = await objects.select(key.label("k"), func.count().label("n")).group_by(key).order_by(key)
    assert [tuple(r) for r in rows] == [("v", 2), (None, 4)]


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


NUM = """
model Num {
  id BigInt  @id @default(autoincrement())
  p  Decimal?
  a  Decimal @default(0)
  b  Decimal @default(0)
  f  Float?
  @@map("grp_nums")
}
"""


@pytest.fixture
async def nums():
    try:
        db, Num = await connect(NUM, "Num", URL)
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {URL}: {e}")
    await Num.objects.using(db).insert_many([{"a": Decimal("5")}])
    yield Num.objects.using(db), Num
    await db.drop_tables()
    await db.close()


async def test_equal_numeric_values_with_different_scale_keep_separate_binds(nums):
    objects, Num = nums
    rows = await objects.select(func.coalesce(Num.p, Decimal("1.0")).label("x"), func.coalesce(Num.p, Decimal("1.00")).label("y")).order_by(Num.id)
    assert [tuple(map(str, r)) for r in rows] == [("1.0", "1.00")]
    rows = await objects.select(func.coalesce(Num.f, 0.0).label("x"), func.coalesce(Num.f, -0.0).label("y")).order_by(Num.id)
    assert [tuple(map(str, r)) for r in rows] == [("0.0", "-0.0")]


async def test_update_keeps_the_scale_with_a_distinct_subquery(nums):
    objects, Num = nums
    await objects.filter(Num.id.in_(objects.select(Num.id).distinct())).update(a=Decimal("1.0"), b=Decimal("1.00"))
    row = await objects.select(Num.a, Num.b).first()
    assert tuple(map(str, row)) == ("1.0", "1.00")


async def test_typed_read_of_a_real_column():
    db, Num = await connect(NUM, "Num", URL)
    try:
        await db.execute("ALTER TABLE grp_nums ALTER COLUMN f TYPE real")
        objects = Num.objects.using(db)
        await objects.insert_many([{"f": 0.5}])
        assert [n.f for n in await objects.all()] == [0.5]
        assert await objects.select(Num.f).scalars() == [0.5]
        assert await objects.select(func.array_agg(Num.f)).scalars() == [[0.5]]
    finally:
        await db.drop_tables()
        await db.close()
