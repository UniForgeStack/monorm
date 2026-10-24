"""Inferred conflict targets prefer a plain unique index to partial indexes."""

import pytest

import orm
from conftest import DATABASE_URL


@pytest.fixture(params=["sqlite", "postgresql"])
async def inference(request):
    provider = request.param
    registry = orm.Registry()
    models = orm.loads(
        f'datasource db {{ provider = "{provider}" }}' + """
        model Mixed {
          id BigInt @id @default(autoincrement())
          slug String
          active Boolean @default(true)
          @@index([slug], unique: true, where: raw("active"), map: "mixed_active")
          @@index([slug], unique: true, map: "mixed_plain")
          @@map("oci_mixed_inference")
        }
        model Partial {
          id BigInt @id @default(autoincrement())
          slug String
          active Boolean @default(true)
          shown Boolean @default(true)
          @@index([slug], unique: true, where: raw("active"), map: "partial_active")
          @@index([slug], unique: true, where: raw("shown"), map: "partial_shown")
          @@map("oci_partial_inference")
        }
        model Single {
          id BigInt @id @default(autoincrement())
          slug String
          active Boolean @default(true)
          @@index([slug], unique: true, where: raw("active"), map: "single_active")
          @@map("oci_single_inference")
        }
        """,
        registry=registry,
    )
    db = await orm.connect("sqlite://:memory:" if provider == "sqlite" else DATABASE_URL, registry=registry, default=False)
    try:
        await db.create_tables()
        events = []
        db.on_query(events.append)
        yield db, models, events
    finally:
        await db.drop_tables()
        await db.close()


async def test_inference_plain_plus_partial_insert(inference):
    db, models, events = inference
    Mixed = models["Mixed"]
    rows = Mixed.objects.using(db)
    first = await rows.insert(slug="a", active=False)
    assert await rows.insert(slug="a").on_conflict(Mixed.slug, update=False) == 0
    updated = await rows.insert(slug="a", active=True).on_conflict(Mixed.slug, update=True).returning()
    assert updated.id == first.id and updated.active
    assert await rows.insert_many([{"slug": "a"}, {"slug": "b"}]).on_conflict(Mixed.slug, update=False) == 1
    sql = [e.sql for e in events if "ON CONFLICT" in e.sql]
    assert len(sql) == 3 and all('ON CONFLICT ("slug") DO ' in s for s in sql)
    assert await rows.count() == 2


async def test_inference_plain_plus_partial_get_or_insert(inference):
    db, models, events = inference
    rows = models["Mixed"].objects.using(db)
    row, created = await rows.get_or_insert(slug="a", defaults={"active": False})
    again, created_again = await rows.get_or_insert(slug="a")
    assert (created, created_again, again.id, row.active) == (True, False, row.id, False)
    sql = next(e.sql for e in events if "ON CONFLICT" in e.sql)
    assert 'ON CONFLICT ("slug") DO NOTHING' in sql
    assert await rows.count() == 1


async def test_inference_two_partial_indexes_raise(inference):
    db, models, events = inference
    Partial = models["Partial"]
    rows = Partial.objects.using(db)
    message = "matches 2 unique indexes of Partial: partial_active, partial_shown"
    with pytest.raises(orm.QueryError, match=message):
        await rows.insert(slug="a").on_conflict(Partial.slug, update=False)
    with pytest.raises(orm.QueryError, match=message):
        await rows.get_or_insert(slug="a")
    assert not any("INSERT" in e.sql for e in events)
    assert await rows.count() == 0


async def test_inference_single_partial_keeps_predicate(inference):
    db, models, events = inference
    Single = models["Single"]
    rows = Single.objects.using(db)
    await rows.insert_many([{"slug": "a", "active": False}, {"slug": "a", "active": False}])
    await rows.insert(slug="a").on_conflict(Single.slug, update=False)
    assert await rows.insert(slug="a").on_conflict(Single.slug, update=False) == 0
    row, created = await rows.get_or_insert(slug="b")
    again, created_again = await rows.get_or_insert(slug="b")
    assert (created, created_again, again.id) == (True, False, row.id)
    sql = [e.sql for e in events if "ON CONFLICT" in e.sql]
    assert len(sql) == 3 and all('ON CONFLICT ("slug") WHERE active DO NOTHING' in s for s in sql)
    assert await rows.count() == 4
