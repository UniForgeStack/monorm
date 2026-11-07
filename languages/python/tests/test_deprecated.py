"""`@deprecated`: the field leaves the model and the SQL, and its column stays in the table."""

import os

import pytest

import monorm

SOURCE = """
model Item {
  id     Int    @id @default(autoincrement())
  title  String
  legacy String @default("old") @deprecated
  @@map("deprecated_items")
}
"""
URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")


async def own_database_url() -> str:
    """A database of its own, so that drift sees no table of another test."""
    name = URL.rsplit("/", 1)[1] + "_deprecated"
    admin = await monorm.connect(URL, max_connections=1, default=False, registry=monorm.Registry())
    try:
        if not await admin._fetch_text(f"SELECT 1 FROM pg_database WHERE datname = '{name}'"):
            await admin.execute(f'CREATE DATABASE "{name}"')
    finally:
        await admin.close()
    return URL.rsplit("/", 1)[0] + "/" + name


def test_a_deprecated_field_is_not_in_the_class():
    Item = monorm.loads(SOURCE, registry=monorm.Registry())["Item"]
    assert list(Item._meta.fields) == ["id", "title"]
    assert not hasattr(Item, "legacy")


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_a_deprecated_not_null_default_column_does_not_stop_writes_and_reads(dialect):
    source = SOURCE if dialect == "postgres" else 'datasource db { provider = "sqlite" }\n' + SOURCE
    registry = monorm.Registry()
    Item = monorm.loads(source, registry=registry)["Item"]
    db = await monorm.connect("sqlite://:memory:" if dialect == "sqlite" else URL, registry=registry, default=False)
    await db.create_tables()
    statements: list[str] = []
    remove = db.on_query(lambda event: statements.append(event.sql))
    try:
        one = await Item.objects.using(db).insert(title="a")
        many = await Item.objects.using(db).insert_many([{"title": "b"}, {"title": "c"}]).returning()
        assert [r.title for r in many] == ["b", "c"]
        assert (await Item.objects.using(db).get(Item.id == one.id)).title == "a"
        await Item.objects.using(db).filter(Item.id == one.id).update(title="a2")
        assert [r.title for r in await Item.objects.using(db).order_by(Item.id)] == ["a2", "b", "c"]
        await one.update(title="a3")
        await Item.objects.using(db).filter(Item.id == many[0].id).delete()
        await many[1].delete()
        assert [r.title for r in await Item.objects.using(db).order_by(Item.id)] == ["a3"]
        remove()
        assert statements and not any("legacy" in sql for sql in statements)
        # The column is still in the table, filled by its database default.
        assert [r["legacy"] for r in await db.fetch("SELECT legacy FROM deprecated_items")] == ["old"]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_a_deprecated_column_that_exists_is_no_drift_and_no_migration(dialect, tmp_path):
    from monorm.migrations import Migrations, Migrator

    prefix = 'datasource db { provider = "sqlite" }\n' if dialect == "sqlite" else ""
    plain = prefix + SOURCE.replace(" @deprecated", "").replace("deprecated_items", "deprecated_drift_items")
    deprecated = prefix + SOURCE.replace("deprecated_items", "deprecated_drift_items")

    def registry(source):
        reg = monorm.Registry()
        monorm.loads(source, registry=reg)
        return reg

    url = "sqlite://:memory:" if dialect == "sqlite" else await own_database_url()
    db = await monorm.connect(url, registry=registry(plain), default=False)
    try:
        await db.execute("DROP TABLE IF EXISTS deprecated_drift_items; DROP TABLE IF EXISTS monorm_migrations")
        Migrations(tmp_path, registry(plain)).make()
        await Migrator(db, Migrations(tmp_path, registry(plain))).upgrade()
        later = Migrations(tmp_path, registry(deprecated))
        assert later.make() == []
        assert not await Migrator(db, later).drift()
        await db.fetch("SELECT legacy FROM deprecated_drift_items")  # the column still exists
    finally:
        await db.execute("DROP TABLE IF EXISTS deprecated_drift_items; DROP TABLE IF EXISTS monorm_migrations")
        await db.close()
