"""JSON path keys are written into the SQL text: hostile keys must stay inside one literal."""

import os

import pytest

import monorm
from monorm import QueryError, func

SOURCE = """
model KeyDoc {
  id    BigInt @id @default(autoincrement())
  title String
  meta  Json
  @@map("json_key_docs")
}
"""
URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
SETTING = "SELECT current_setting('standard_conforming_strings') AS v"
KEYS = ["a'); DROP TABLE json_key_docs;--\\", "$1", "it's", "back\\slash", "--", ";", "line\nbreak", "\\'", "$", "'"]


@pytest.fixture(params=["on", "off"])
async def docs(request):
    registry = monorm.Registry()
    Doc = monorm.loads(SOURCE, registry=registry)["KeyDoc"]
    try:
        db = await monorm.connect(URL, registry=registry, default=False, max_connections=1)
    except monorm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {URL}: {e}")
    await db.execute(f"SET standard_conforming_strings = {request.param}")
    await db.drop_tables()
    await db.create_tables()
    objects = Doc.objects.using(db)
    rows = [{"title": f"hit{i}", "meta": {k: f"v{i}"}} for i, k in enumerate(KEYS)]
    rows.append({"title": "decoy", "meta": {"other": "v0"}})
    await objects.insert_many(rows)
    yield db, Doc, objects, request.param
    await db.drop_tables()
    await db.close()


async def test_hostile_keys_filter_select_and_group(docs, observe_queries):
    db, Doc, objects, setting = docs
    assert await db.fetch(SETTING) == [{"v": setting}]
    for i, key in enumerate(KEYS):
        with observe_queries(db) as events:
            found = await objects.filter(Doc.meta[key].as_text() == f"v{i}").select(Doc.title).scalars()
            picked = await objects.filter(Doc.title == f"hit{i}").select(Doc.meta[key].as_text().label("v")).scalars()
            grouped = await (
                objects.filter(Doc.meta[key].as_text().is_not_null())
                .select(Doc.meta[key].as_text().label("v"), func.count().label("n"))
                .group_by(Doc.meta[key].as_text())
            )
        assert found == [f"hit{i}"], key
        assert picked == [f"v{i}"], key
        assert [tuple(r) for r in grouped] == [(f"v{i}", 1)], key
        assert len(events) == 3, key
    assert await db.fetch(SETTING) == [{"v": setting}]
    assert await objects.count() == len(KEYS) + 1


def test_nul_key_is_rejected():
    Doc = monorm.loads(SOURCE, registry=monorm.Registry())["KeyDoc"]
    with pytest.raises(QueryError, match="NUL"):
        Doc.objects.filter(Doc.meta["a\0b"].as_text() == "x").sql()


async def test_sqlite_rejects_json_paths():
    registry = monorm.Registry()
    Doc = monorm.loads('datasource db { provider = "sqlite" }\n' + SOURCE, registry=registry)["KeyDoc"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        with pytest.raises(QueryError, match="sqlite does not support JSON paths"):
            await Doc.objects.using(db).filter(Doc.meta["it's"].as_text() == "x").count()
    finally:
        await db.close()
