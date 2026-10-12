"""Composite primary keys (`@@id`) and composite foreign keys on Postgres and SQLite."""
import pytest

import orm
from conftest import DATABASE_URL
from orm.__main__ import main as cli

SCHEMA = """
model Item {
  shop  BigInt
  id    BigInt
  title String
  lines Line[]
  @@id([shop, id])
  @@map("ck_item")
}

model Line {
  id      BigInt @id @default(autoincrement())
  shop    BigInt
  item_id BigInt
  qty     Int
  item    Item   @relation(fields: [shop, item_id], references: [shop, id])
  @@map("ck_line")
}
"""


@pytest.fixture(params=["sqlite", "postgres"])
async def models(request):
    registry = orm.Registry()
    source = SCHEMA if request.param == "postgres" else 'datasource db {\n  provider = "sqlite"\n}\n' + SCHEMA
    m = orm.loads(source, registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else DATABASE_URL
    try:
        db = await orm.connect(url, registry=registry, default=False)
    except orm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    yield db, m["Item"], m["Line"]
    await db.drop_tables()
    await db.close()


async def test_crud_by_the_whole_key(models):
    db, Item, Line = models
    items = await Item.objects.using(db).insert_many(
        [{"shop": 1, "id": 1, "title": "a"}, {"shop": 2, "id": 1, "title": "b"}, {"shop": 1, "id": 2, "title": "c"}]
    ).returning()
    assert [i.pk for i in items] == [(1, 1), (2, 1), (1, 2)]
    assert Item._meta.key_names == ("shop", "id")

    b = await Item.objects.using(db).require().get(Item.shop == 2, Item.id == 1)
    assert b.title == "b" and b == items[1] and hash(b) == hash(items[1])
    assert await Item.objects.using(db).get(Item.shop == 3, Item.id == 1) is None

    await b.update(title="B")
    assert b.title == "B"
    assert [i.title for i in await Item.objects.using(db).order_by(Item.shop, Item.id)] == ["a", "c", "B"]

    await db.execute("UPDATE ck_item SET title = 'bb' WHERE shop = 2 AND id = 1")
    assert await b.refresh() and b.title == "bb"

    by_key = await Item.objects.using(db).in_bulk([(1, 2), (2, 1)])
    assert sorted(by_key) == [(1, 2), (2, 1)]

    await Item.objects.using(db).update_many([{"shop": 1, "id": 1, "title": "x"}, {"shop": 1, "id": 2, "title": "y"}])
    assert [i.title for i in await Item.objects.using(db).order_by(Item.shop, Item.id)] == ["x", "y", "bb"]

    await b.delete()
    assert await Item.objects.using(db).count() == 2
    assert await Item.objects.using(db).get(Item.shop == 2, Item.id == 1) is None


async def test_pagination_and_batches_walk_the_whole_key(models):
    db, Item, _ = models
    await Item.objects.using(db).insert_many([{"shop": s, "id": i, "title": f"{s}.{i}"} for s in (1, 2) for i in (1, 2, 3)])
    titles = []
    cursor = None
    while True:
        page = await Item.objects.using(db).paginate(first=4, after=cursor)
        titles += [i.title for i in page.items]
        if not page.has_next:
            break
        cursor = page.next_cursor
    assert titles == ["1.1", "1.2", "1.3", "2.1", "2.2", "2.3"]
    back = await Item.objects.using(db).paginate(last=2)
    assert [i.title for i in back.items] == ["2.2", "2.3"]
    assert [[i.title for i in batch] async for batch in Item.objects.using(db).batches(4)] == [
        ["1.1", "1.2", "1.3", "2.1"], ["2.2", "2.3"]
    ]


async def test_relations_through_a_composite_foreign_key(models):
    db, Item, Line = models
    items = await Item.objects.using(db).insert_many(
        [{"shop": 1, "id": 1, "title": "a"}, {"shop": 2, "id": 1, "title": "b"}]
    ).returning()
    a, b = items
    await Line.objects.using(db).insert_many(
        [{"item": a, "qty": 1}, {"item": a, "qty": 2}, {"shop": 2, "item_id": 1, "qty": 3}]
    )
    lines = await Line.objects.using(db).load(Line.item).order_by(Line.id)
    assert [(line.item.title, line.qty) for line in lines] == [("a", 1), ("a", 2), ("b", 3)]
    prefetched = await Line.objects.using(db).load(Line.item.objects.filter(Item.title >= "a")).order_by(Line.id)
    assert [line.item.title for line in prefetched] == ["a", "a", "b"]

    loaded = await Item.objects.using(db).load(Item.lines).order_by(Item.shop)
    assert [[line.qty for line in i.lines.cached] for i in loaded] == [[1, 2], [3]]
    assert [line.qty for line in await a.lines.using(db).order_by(Line.qty)] == [1, 2]
    await b.lines.using(db).insert(qty=4)
    assert [line.qty for line in await b.lines.using(db).order_by(Line.qty)] == [3, 4]

    assert await Line.objects.using(db).filter(Line.item == a).count() == 2
    assert await Line.objects.using(db).filter(Line.item.title == "b").count() == 2
    assert await Item.objects.using(db).filter(Item.lines.qty > 3).count() == 1

    line = await Line.objects.using(db).require().get(Line.id == lines[0].id)
    await line.update(item=b)
    assert (line.shop, line.item_id) == (2, 1)
    await line.load(Line.item)
    assert line.item.title == "b"



def test_generated_key_types(tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text(SCHEMA)
    assert cli(["--schema", str(schema), "generate", "python", "-o", str(tmp_path / "models.py")]) == 0
    assert cli(["--schema", str(schema), "generate", "typescript", "-o", str(tmp_path / "models.ts")]) == 0
    stub = (tmp_path / "models.pyi").read_text()
    assert "    def pk(self) -> tuple[int, int]: ..." in stub.split("class Item(Model):")[1].split("class ")[0]
    assert "def pk(" not in stub.split("class Line(Model):")[1].split("class ")[0]
    ts = (tmp_path / "models.ts").read_text()
    assert "readonly pk: { readonly shop: bigint; readonly id: bigint };" in ts
