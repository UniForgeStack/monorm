"""Every built-in model extension in one schema: the schema loads, and reads, writes and history rows work together.

The loader picks the extensions that a schema uses, so a test with one or two of them cannot see a clash between others.
"""

import os

import pytest

import monorm
from artifact_caps import has_extension

pytestmark = pytest.mark.skipif(
    not has_extension("monorm.scope", "monorm.soft-delete", "monorm.history", "monorm.timestamps", "monorm.locking", "monorm.query-defaults"),
    reason="requires the scope, soft-delete, history, timestamps, locking and query-defaults packages",
)

SOURCES = {
    "postgresql": 'datasource db {\n provider = "postgresql"\n}\n',
    "sqlite": 'datasource db {\n provider = "sqlite"\n}\n',
}

SCHEMA = """
model Doc {
 id Int @id @default(autoincrement())
 shop_id Int
 title String
 hidden Boolean @default(false)
 @@scope.by(shop: shop_id)
 @@soft_delete.deleted_at("deleted")
 @@timestamps.updated_at("changed")
 @@locking.version("version")
 @@history.track
 @@query.defaults(filter: hidden == false)
 @@map("cx_docs")
}
model Note {
 id Int @id @default(autoincrement())
 name String
 @@soft_delete.deleted_at("deleted")
 @@map("cx_notes")
}
model Tag {
 id Int @id @default(autoincrement())
 shop_id Int
 name String
 @@scope.by(shop: shop_id)
 @@history.track
 @@map("cx_tags")
}
"""


def url(provider):
    if provider == "sqlite":
        return "sqlite://:memory:"
    return os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")


@pytest.fixture(params=["sqlite", "postgresql"])
async def world(request):
    registry = monorm.Registry()
    models = monorm.loads(SOURCES[request.param] + SCHEMA, registry=registry)
    db = await monorm.connect(url(request.param), registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        yield db, models
    finally:
        await db.drop_tables()
        await db.close()


async def seed(db, Doc):
    with monorm.scope(shop=1):
        await Doc.objects.using(db).insert_many([{"title": "a"}, {"title": "b"}, {"title": "hidden", "hidden": True}])
    with monorm.scope(shop=2):
        await Doc.objects.using(db).insert(title="other shop")


async def titles(query):
    return sorted(r.title for r in await query)


async def test_one_schema_with_every_model_extension_loads(world):
    db, models = world
    Doc = models["Doc"]
    assert {"deleted", "changed", "version"} <= set(Doc._meta.input_fields) | set(Doc._meta.fields)
    assert "DocEvent" in models and "TagEvent" in models


async def test_a_read_applies_the_scope_the_soft_delete_and_the_query_default_filter(world):
    db, models = world
    Doc = models["Doc"]
    await seed(db, Doc)
    with monorm.scope(shop=1):
        docs = {d.title: d for d in await Doc.objects.using(db).all()}
        assert sorted(docs) == ["a", "b"]
        await docs["b"].delete()
        assert await titles(Doc.objects.using(db).all()) == ["a"]
        # `all_with_deleted` lifts the soft-delete filter only: the scope and the query default stay.
        assert await titles(Doc.objects.using(db).all_with_deleted()) == ["a", "b"]
        # `without_defaults("query")` lifts the query default only: the scope and the soft-delete filter stay.
        assert await titles(Doc.objects.using(db).without_defaults("query")) == ["a", "hidden"]
    with monorm.scope(shop=2):
        assert await titles(Doc.objects.using(db).all()) == ["other shop"]


async def test_a_write_takes_the_scope_stamps_the_row_and_bumps_the_version(world):
    db, models = world
    Doc = models["Doc"]
    with monorm.scope(shop=1):
        doc = await Doc.objects.using(db).insert(title="a")
        assert (doc.shop_id, doc.version, doc.deleted) == (1, 0, None)
        assert doc.changed is not None
        await doc.update(title="b")
        assert doc.version == 1
        with pytest.raises(monorm.ScopeMismatch):
            await Doc.objects.using(db).insert(shop_id=2, title="x")
    with monorm.scope(shop=2):
        assert await titles(Doc.objects.using(db).all()) == []


async def test_updates_and_a_soft_delete_write_history_rows_of_the_scoped_model(world):
    db, models = world
    Doc = models["Doc"]
    with monorm.scope(shop=1):
        doc = await Doc.objects.using(db).insert(title="a")
        await doc.update(title="b")
        await doc.delete()
        assert await titles(Doc.objects.using(db).all()) == []
        entries = await Doc.history.using(db).timeline(doc)
    assert [e.event.label for e in entries] == ["insert", "update", "update"]
    assert [e.event.data["title"] for e in entries] == ["a", "b", "b"]
    assert entries[-1].event.data["deleted"] is not None


async def test_models_with_one_extension_each_keep_only_their_own_filter(world):
    db, models = world
    Note, Tag = models["Note"], models["Tag"]
    await Note.objects.using(db).insert(name="kept")
    gone = await Note.objects.using(db).insert(name="gone")
    await gone.delete()
    assert sorted(n.name for n in await Note.objects.using(db).all()) == ["kept"]
    with monorm.scope(shop=1):
        await Tag.objects.using(db).insert_many([{"name": "x"}])
    with monorm.scope(shop=2):
        tag = await Tag.objects.using(db).insert(name="y")
        assert sorted(t.name for t in await Tag.objects.using(db).all()) == ["y"]
        assert [e.event.label for e in await Tag.history.using(db).timeline(tag)] == ["insert"]
