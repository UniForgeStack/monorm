"""Owning relations (`owns: true`): the database deletes an owned row with its last owner, on PostgreSQL and SQLite."""

import asyncio

import pytest

import monorm
from conftest import DATABASE_URL

SCHEMA = """
model Spec {
  id      BigInt   @id
  body    String
  guitars Guitar[]
  photos  Photo[]
  @@map("ow_spec")
}

model Guitar {
  id      BigInt  @id
  spec_id BigInt?
  spec    Spec?   @relation(fields: [spec_id], references: [id], onDelete: NoAction, owns: true)
  @@map("ow_guitar")
}

model Photo {
  id      BigInt @id
  spec_id BigInt
  spec    Spec   @relation(fields: [spec_id], references: [id])
  @@map("ow_photo")
}

model Order {
  id        BigInt  @id
  basket_id BigInt
  basket    Basket  @relation(fields: [basket_id], references: [id], onDelete: NoAction, owns: true)
  @@map("ow_order")
}

model Basket {
  id        BigInt  @id
  orders    Order[]
  detail_id BigInt
  detail    Detail  @relation(fields: [detail_id], references: [id], onDelete: NoAction, owns: true)
  @@map("ow_basket")
}

model Detail {
  id      BigInt   @id
  baskets Basket[]
  @@map("ow_detail")
}
"""


CYCLE = """
model Left {
  id       BigInt @id
  right_id BigInt?
  right    Right? @relation("LeftOwnsRight", fields: [right_id], references: [id], onDelete: NoAction, owns: true)
  lefts    Right[] @relation("RightOwnsLeft")
  @@map("ow_left")
}

model Right {
  id      BigInt @id
  left_id BigInt?
  left    Left?  @relation("RightOwnsLeft", fields: [left_id], references: [id], onDelete: NoAction, owns: true)
  rights  Left[] @relation("LeftOwnsRight")
  @@map("ow_right")
}
"""

LONG_CYCLE = """
model Ring {
  id      BigInt @id
  next_id BigInt?
  next    Ring2? @relation("RingNext", fields: [next_id], references: [id], onDelete: NoAction, owns: true)
  prevs   Ring2[] @relation("Ring2Next")
  @@map("ow_ring")
}

model Ring2 {
  id      BigInt @id
  next_id BigInt?
  next    Ring?  @relation("Ring2Next", fields: [next_id], references: [id], onDelete: NoAction, owns: true)
  prevs   Ring[] @relation("RingNext")
  @@map("ow_ring2")
}
"""


@pytest.fixture(params=["sqlite", "postgres"])
async def models(request):
    registry = monorm.Registry()
    source = SCHEMA if request.param == "postgres" else 'datasource db {\n  provider = "sqlite"\n}\n' + SCHEMA
    m = monorm.loads(source, registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else DATABASE_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    yield db, m
    await db.drop_tables()
    await db.close()


async def ids(db, table):
    return [r["id"] for r in await db.fetch(f"SELECT id FROM {table} ORDER BY id")]


async def test_an_owned_row_goes_with_its_only_owner(models):
    db, m = models
    await m["Spec"].objects.using(db).insert_many([{"id": 1, "body": "a"}, {"id": 2, "body": "b"}])
    await m["Guitar"].objects.using(db).insert_many([{"id": 1, "spec_id": 1}, {"id": 2, "spec_id": 2}])
    await m["Guitar"].objects.using(db).filter(m["Guitar"].id == 1).delete()
    assert await ids(db, "ow_spec") == [2]
    assert await ids(db, "ow_guitar") == [2]


async def test_a_shared_owned_row_stays_until_its_last_owner_goes(models):
    db, m = models
    Guitar = m["Guitar"]
    await m["Spec"].objects.using(db).insert_many([{"id": 1, "body": "a"}])
    await Guitar.objects.using(db).insert_many([{"id": 1, "spec_id": 1}, {"id": 2, "spec_id": 1}])
    await Guitar.objects.using(db).filter(Guitar.id == 1).delete()
    assert await ids(db, "ow_spec") == [1]
    await Guitar.objects.using(db).filter(Guitar.id == 2).delete()
    assert await ids(db, "ow_spec") == []


async def test_all_owners_deleted_in_one_statement_delete_the_shared_row(models):
    db, m = models
    Guitar = m["Guitar"]
    await m["Spec"].objects.using(db).insert_many([{"id": 1, "body": "a"}, {"id": 2, "body": "b"}])
    await Guitar.objects.using(db).insert_many(
        [{"id": 1, "spec_id": 1}, {"id": 2, "spec_id": 1}, {"id": 3, "spec_id": 2}]
    )
    assert await Guitar.objects.using(db).filter(Guitar.id < 3).delete() == 2
    assert await ids(db, "ow_spec") == [2]


async def test_another_foreign_key_keeps_the_owned_row(models):
    db, m = models
    Guitar = m["Guitar"]
    await m["Spec"].objects.using(db).insert_many([{"id": 1, "body": "a"}])
    await m["Photo"].objects.using(db).insert_many([{"id": 1, "spec_id": 1}])
    await Guitar.objects.using(db).insert_many([{"id": 1, "spec_id": 1}])
    await Guitar.objects.using(db).filter(Guitar.id == 1).delete()
    assert await ids(db, "ow_spec") == [1]
    await db.execute("DELETE FROM ow_photo")
    await Guitar.objects.using(db).insert_many([{"id": 2, "spec_id": 1}])
    await Guitar.objects.using(db).filter(Guitar.id == 2).delete()
    assert await ids(db, "ow_spec") == []


async def test_a_null_owning_key_deletes_nothing(models):
    db, m = models
    await m["Spec"].objects.using(db).insert_many([{"id": 1, "body": "a"}])
    await m["Guitar"].objects.using(db).insert_many([{"id": 1, "spec_id": None}])
    await m["Guitar"].objects.using(db).filter(m["Guitar"].id == 1).delete()
    assert await ids(db, "ow_spec") == [1]


async def test_a_two_level_chain_deletes_every_level(models):
    db, m = models
    await m["Detail"].objects.using(db).insert_many([{"id": 1}])
    await m["Basket"].objects.using(db).insert_many([{"id": 1, "detail_id": 1}])
    await m["Order"].objects.using(db).insert_many([{"id": 1, "basket_id": 1}])
    await m["Order"].objects.using(db).filter(m["Order"].id == 1).delete()
    assert await ids(db, "ow_basket") == []
    assert await ids(db, "ow_detail") == []


async def test_a_two_model_cycle_deletes_both_rows(request):
    registry = monorm.Registry()
    m = monorm.loads(CYCLE, registry=registry)
    try:
        db = await monorm.connect(DATABASE_URL, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    try:
        await db.drop_tables()
        await db.create_tables()
        await m["Left"].objects.using(db).insert_many([{"id": 1, "right_id": None}])
        await m["Right"].objects.using(db).insert_many([{"id": 1, "left_id": 1}])
        await db.execute("UPDATE ow_left SET right_id = 1 WHERE id = 1")
        await m["Left"].objects.using(db).filter(m["Left"].id == 1).delete()
        assert await ids(db, "ow_left") == []
        assert await ids(db, "ow_right") == []
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("schema", [CYCLE, LONG_CYCLE])
def test_an_owning_cycle_is_refused_on_sqlite(schema):
    # A trigger does not fire again while it runs on SQLite, so a longer chain of the cycle would leave an orphan.
    with pytest.raises(Exception, match="owning cycle"):
        monorm.loads('datasource db {\n  provider = "sqlite"\n}\n' + schema, registry=monorm.Registry())


async def test_two_concurrent_deletes_of_the_owners_delete_the_shared_row():
    registry = monorm.Registry()
    m = monorm.loads(SCHEMA, registry=registry)
    try:
        db = await monorm.connect(DATABASE_URL, registry=registry, default=False, max_connections=1)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    other = await monorm.connect(DATABASE_URL, registry=registry, default=False, max_connections=1)
    try:
        await db.drop_tables()
        await db.create_tables()
        await m["Spec"].objects.using(db).insert_many([{"id": 1, "body": "a"}])
        await m["Guitar"].objects.using(db).insert_many([{"id": 1, "spec_id": 1}, {"id": 2, "spec_id": 1}])

        async def second_owner():
            async with other.transaction():
                await other.execute("DELETE FROM ow_guitar WHERE id = 2")

        async with db.transaction():
            await db.execute("DELETE FROM ow_guitar WHERE id = 1")
            task = asyncio.create_task(second_owner())
            await asyncio.sleep(0.5)
            assert not task.done(), "the second delete must wait for the lock on the shared row"
        await asyncio.wait_for(task, 10)
        assert await ids(db, "ow_spec") == []
    finally:
        await db.drop_tables()
        await db.close()
        await other.close()


async def test_a_raw_sql_delete_gives_the_same_result_as_the_orm_call(models):
    db, m = models
    Guitar = m["Guitar"]
    await m["Spec"].objects.using(db).insert_many([{"id": 1, "body": "a"}, {"id": 2, "body": "b"}])
    await Guitar.objects.using(db).insert_many(
        [{"id": 1, "spec_id": 1}, {"id": 2, "spec_id": 1}, {"id": 3, "spec_id": 2}]
    )
    await db.execute("DELETE FROM ow_guitar WHERE id = 1")
    assert await ids(db, "ow_spec") == [1, 2]
    await db.execute("DELETE FROM ow_guitar WHERE id IN (2, 3)")
    assert await ids(db, "ow_spec") == []
