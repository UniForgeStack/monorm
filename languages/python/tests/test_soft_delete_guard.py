"""The guard of a soft-delete cascade child: a row under, or moved to, a soft-deleted parent takes the stamp of the parent."""

from types import SimpleNamespace

import pytest

import monorm
from artifact_caps import has_extension
from conftest import DATABASE_URL

pytestmark = pytest.mark.skipif(not has_extension("monorm.soft-delete"), reason="requires the soft-delete package")

SOFT = '@@soft_delete.deleted_at("deleted")'

SCHEMA = f"""
model Shelf {{
  id    BigInt @id
  books Book[]
  memos Memo[]
  {SOFT}
  @@map("sg_shelf")
}}

model Book {{
  id       BigInt @id
  shelf_id BigInt
  shelf    Shelf  @relation(fields: [shelf_id], references: [id], onDelete: Cascade)
  pages    Page[]
  {SOFT}
  @@map("sg_book")
}}

model Page {{
  id      BigInt @id
  book_id BigInt
  book    Book   @relation(fields: [book_id], references: [id], onDelete: Cascade)
  {SOFT}
  @@map("sg_page")
}}

model Memo {{
  id       BigInt @id
  shelf_id BigInt
  shelf    Shelf  @relation(fields: [shelf_id], references: [id], onDelete: Cascade) @soft_delete.keep_child_rows
  {SOFT}
  @@map("sg_memo")
}}
"""

SQLITE = 'datasource db {\n  provider = "sqlite"\n}\n'
T1 = "'2020-01-01 00:00:00.000'"
T2 = "'2020-01-02 00:00:00.000'"


@pytest.fixture(params=["sqlite", "postgres"])
async def env(request):
    registry = monorm.Registry()
    m = monorm.loads((SQLITE if request.param == "sqlite" else "") + SCHEMA, registry=registry)
    try:
        db = await monorm.connect("sqlite://:memory:" if request.param == "sqlite" else DATABASE_URL, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    await m["Shelf"].objects.using(db).insert_many([{"id": 1}, {"id": 2}])
    await db.execute(f"UPDATE sg_shelf SET deleted = {T1} WHERE id = 1")
    yield SimpleNamespace(db=db, m=m)
    await db.drop_tables()
    await db.close()


async def stamp(db, table, key):
    return (await db.fetch(f"SELECT deleted FROM {table} WHERE id = {key}"))[0]["deleted"]


async def test_a_row_inserted_under_a_deleted_parent_takes_the_stamp_and_under_a_live_parent_none(env):
    db, m = env.db, env.m
    await m["Book"].objects.using(db).insert_many([{"id": 1, "shelf_id": 1}, {"id": 2, "shelf_id": 2}])
    assert await stamp(db, "sg_book", 1) == await stamp(db, "sg_shelf", 1)
    assert await stamp(db, "sg_book", 1) is not None
    assert await stamp(db, "sg_book", 2) is None


async def test_a_row_with_its_own_stamp_keeps_it(env):
    db = env.db
    await db.execute(f"INSERT INTO sg_book (id, shelf_id, deleted) VALUES (1, 1, {T2})")
    assert str(await stamp(db, "sg_book", 1)).startswith("2020-01-02")


async def test_a_live_row_moved_to_a_deleted_parent_takes_the_stamp_and_to_a_live_parent_keeps_none(env):
    db, m = env.db, env.m
    await m["Book"].objects.using(db).insert_many([{"id": 1, "shelf_id": 2}, {"id": 2, "shelf_id": 2}])
    await db.execute("UPDATE sg_book SET shelf_id = 1 WHERE id = 1")
    assert await stamp(db, "sg_book", 1) == await stamp(db, "sg_shelf", 1)
    await db.execute("UPDATE sg_shelf SET deleted = NULL WHERE id = 2")
    await db.execute("UPDATE sg_book SET shelf_id = 2 WHERE id = 2")
    assert await stamp(db, "sg_book", 2) is None


async def test_the_revive_of_the_parent_brings_back_a_row_that_the_guard_stamped(env):
    db, m = env.db, env.m
    await m["Book"].objects.using(db).insert_many([{"id": 1, "shelf_id": 1}])
    await db.execute("UPDATE sg_shelf SET deleted = NULL WHERE id = 1")
    assert await stamp(db, "sg_book", 1) is None


async def test_a_relation_with_keep_child_rows_has_no_guard(env):
    db, m = env.db, env.m
    await m["Memo"].objects.using(db).insert_many([{"id": 1, "shelf_id": 1}])
    assert await stamp(db, "sg_memo", 1) is None


async def test_a_live_row_moved_to_a_deleted_parent_stamps_its_own_children(env):
    db, m = env.db, env.m
    await m["Book"].objects.using(db).insert_many([{"id": 1, "shelf_id": 2}])
    await m["Page"].objects.using(db).insert_many([{"id": 1, "book_id": 1}])
    await db.execute("UPDATE sg_book SET shelf_id = 1 WHERE id = 1")
    assert await stamp(db, "sg_page", 1) == await stamp(db, "sg_shelf", 1)
    assert await stamp(db, "sg_page", 1) is not None
    await db.execute("UPDATE sg_shelf SET deleted = NULL WHERE id = 1")
    assert await stamp(db, "sg_book", 1) is None
    assert await stamp(db, "sg_page", 1) is None
