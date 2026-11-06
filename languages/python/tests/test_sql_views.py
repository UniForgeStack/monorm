"""A SQL view model reads, refuses every write with `ReadOnlyModel` before SQL, and a materialized view refreshes."""

import os
import re
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit

import pytest

import monorm
from monorm import _native
from monorm.fields import HasMany
from monorm.migrations import Migrations, Migrator, pull

URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
BODIES = {
    "ActiveUser": 'SELECT id, name FROM "sv_py_users" WHERE active',
    "UserNames": 'SELECT id, name FROM "sv_py_users"',
}


def _schema(directory: Path, dialect: str) -> Path:
    head = 'datasource db { provider = "sqlite" }\n' if dialect == "sqlite" else ""
    text = head + """
model User {
  id     Int @id
  name   String
  active Boolean
  @@map("sv_py_users")
}
view ActiveUser {
  id   Int @id
  name String
  @@map("sv_py_active")
}
"""
    if dialect == "postgres":
        text += """
view UserNames {
  id   Int @id
  name String
  @@materialized
  @@index([id], unique: true)
  @@map("sv_py_names")
}
"""
    path = directory / "schema.prisma"
    path.write_text(text)
    sidecars = directory / "views" / ("main" if dialect == "sqlite" else "public")
    sidecars.mkdir(parents=True)
    for name, body in BODIES.items():
        (sidecars / f"{name}.sql").write_text(body)
    return path


async def _private_url() -> str:
    """A database of its own: drift reads the whole database, and the shared one holds other tests' tables."""
    if at := os.environ.get("ORM_SQL_VIEWS_DATABASE_URL"):
        return at
    parts = urlsplit(URL)
    name = parts.path.lstrip("/") + "_views"
    admin = await monorm.connect(URL, max_connections=1, default=False, registry=monorm.Registry())
    try:
        if not await admin._fetch_text(f"SELECT 1 FROM pg_database WHERE datname = '{name}'"):
            await admin.execute(f'CREATE DATABASE "{name}"')
    finally:
        await admin.close()
    return urlunsplit(parts._replace(path="/" + name))


async def _open(tmp_path: Path, dialect: str, url: str = URL):
    registry = monorm.Registry()
    models = monorm.load(_schema(tmp_path, dialect), registry=registry)
    db = await monorm.connect("sqlite://:memory:" if dialect == "sqlite" else url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    await models["User"].objects.using(db).insert_many(
        [{"id": 1, "name": "ada", "active": True}, {"id": 2, "name": "bob", "active": False}]
    )
    return models, db


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_a_view_model_reads_and_every_write_is_refused_before_sql(dialect, tmp_path):
    models, db = await _open(tmp_path, dialect)
    ActiveUser, User = models["ActiveUser"], models["User"]
    try:
        rows = await ActiveUser.objects.using(db).order_by(ActiveUser.id)
        assert [(r.id, r.name) for r in rows] == [(1, "ada")]
        row = rows[0]
        objects = ActiveUser.objects.using(db)
        for write in (
            lambda: objects.insert(id=9, name="x"),
            lambda: objects.insert_many([{"id": 9, "name": "x"}]),
            lambda: objects.insert_many([{"id": 9, "name": "x"}], copy=True),
            lambda: objects.get_or_insert(id=9, name="x"),
            lambda: objects.insert(id=9, name="x").on_conflict(ActiveUser.id, update=True),
            lambda: objects.filter(ActiveUser.id == 1).update(name="x"),
            lambda: objects.update_many([{"id": 1, "name": "x"}]),
            lambda: objects.filter(ActiveUser.id == 1).delete(),
            lambda: objects.all().delete(),
            lambda: row.update(name="x"),
            lambda: row.delete(),
        ):
            with pytest.raises(monorm.ReadOnlyModel, match="ActiveUser is a view"):
                await write()
        # PostgreSQL would take a write to a simple view; the base table shows that none arrived.
        assert sorted((r.id, r.name) for r in await User.objects.using(db).all()) == [(1, "ada"), (2, "bob")]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_refresh_refuses_a_plain_view_and_a_non_view(dialect, tmp_path):
    models, db = await _open(tmp_path, dialect)
    try:
        for model in (models["ActiveUser"], models["User"]):
            with pytest.raises(monorm.QueryError, match="only a materialized view can be refreshed|no materialized views"):
                await db.refresh(model)
    finally:
        await db.drop_tables()
        await db.close()


async def test_refresh_reloads_a_materialized_view_with_and_without_concurrently(tmp_path):
    models, db = await _open(tmp_path, "postgres")
    UserNames, User = models["UserNames"], models["User"]
    try:
        assert await UserNames.objects.using(db).count() == 0
        await db.refresh(UserNames)
        assert sorted(r.name for r in await UserNames.objects.using(db).all()) == ["ada", "bob"]
        await User.objects.using(db).insert(id=3, name="cy", active=True)
        assert await UserNames.objects.using(db).count() == 2
        await db.refresh(UserNames, concurrently=True)
        assert await UserNames.objects.using(db).count() == 3
    finally:
        await db.drop_tables()
        await db.close()


def test_the_generated_module_has_no_write_types_for_a_view(tmp_path):
    module, stub = _native.generate_python(str(_schema(tmp_path, "postgres")))
    text = module + stub
    for shape in ("ActiveUserInsert", "ActiveUserUpdate", "ActiveUserUpdateRow", "ActiveUserLookup"):
        assert shape not in text, shape
    assert "UserInsert" in text
    queries = re.search(r"class ActiveUserQuerySet\(.*?(?=\nclass |\Z)", stub, re.S)
    assert queries
    for method in ("insert", "insert_many", "get_or_insert", "update", "update_many", "delete"):
        assert f"def {method}(self, _view: Never" in queries.group(0), method


async def test_create_tables_runs_twice_and_a_hand_written_body_has_no_drift(tmp_path):
    empty = await _empty_database("postgres")
    await empty.close()
    models, db = await _open(tmp_path, "postgres", await _private_url())
    try:
        await db.create_tables()
        assert await models["ActiveUser"].objects.using(db).count() == 1
        schema = tmp_path / "schema.prisma"
        migrator = Migrator(db, Migrations(tmp_path / "migrations", schema))
        await migrator.baseline()
        assert (await migrator.drift()).steps == []
    finally:
        await db.execute("DROP TABLE IF EXISTS monorm_migrations")
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_related_set_insert_into_a_view_is_refused_before_sql(dialect, tmp_path, observe_queries):
    models, db = await _open(tmp_path, dialect)
    try:
        owner = await models["User"].objects.using(db).get(models["User"].id == 1)
        # Back relation declarations to views are forbidden in v1. Construct the
        # runtime related set directly to exercise its independent insert path.
        relation = HasMany(models["ActiveUser"], via="id")
        relation.__set_name__(models["User"], "active_rows")
        related = monorm.RelatedSet(relation, owner)
        with observe_queries(db) as events:
            with pytest.raises(monorm.ReadOnlyModel, match="ActiveUser is a view"):
                await related.insert(name="changed")
        assert events == []
    finally:
        await db.drop_tables()
        await db.close()


async def _empty_database(dialect, schema=None):
    registry = monorm.Registry()
    if schema is not None:
        monorm.load(schema, registry=registry)
    elif dialect == "sqlite":
        monorm.loads('datasource db { provider = "sqlite" }', registry=registry)
    db = await monorm.connect("sqlite://:memory:" if dialect == "sqlite" else await _private_url(),
                              registry=registry, default=False, max_connections=1)
    if dialect == "postgres":
        await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
    return db


def _migration_schema(directory, dialect, *, field="Int", body=None, extra=""):
    path = directory / "schema.prisma"
    path.write_text(('datasource db { provider = "sqlite" }\n' if dialect == "sqlite" else "") + f'''
model ReviewItem {{
  id Int @id
  value {field}
  {extra}
  @@map("sv_review_items")
}}
view ReviewView {{
  id Int @id
  value {field if body is None else "String"}
  @@map("sv_review_view")
}}
''')
    sidecars = directory / "views" / ("main" if dialect == "sqlite" else "public")
    sidecars.mkdir(parents=True, exist_ok=True)
    (sidecars / "ReviewView.sql").write_text(body or 'SELECT id, value FROM sv_review_items')
    return path


@pytest.mark.parametrize("change", ["body", "column_type", "table"])
async def test_postgres_recreates_a_view_and_runs_the_migration(change, tmp_path):
    schema = _migration_schema(tmp_path, "postgres")
    db = await _empty_database("postgres", schema)
    try:
        migrations = Migrations(tmp_path / "migrations", schema)
        migrations.make()
        migrator = Migrator(db, migrations)
        await migrator.upgrade()
        registry = monorm.Registry()
        models = monorm.load(schema, registry=registry)
        await models["ReviewItem"].objects.using(db).insert_many([{"id": 1, "value": 7}, {"id": 2, "value": 9}])
        if change == "body":
            _migration_schema(tmp_path, "postgres", body="SELECT id, value::text AS value FROM sv_review_items")
        elif change == "column_type":
            _migration_schema(tmp_path, "postgres", field="BigInt")
        else:
            _migration_schema(tmp_path, "postgres", extra="added Int?")
        (migration,) = migrations.make(change)
        sql = migration.up_sql
        assert "OR REPLACE" not in sql
        assert sql.index("DROP VIEW") < sql.index("CREATE VIEW")
        if change != "body":
            assert sql.index("DROP VIEW") < sql.index("ALTER TABLE") < sql.index("CREATE VIEW")
        assert [m.name for m in await migrator.upgrade(allow_destructive=True)] == [migration.name]
        assert await db._fetch_text("SELECT id, value FROM sv_review_view ORDER BY id") == [("1", "7"), ("2", "9")]
        expected = {"body": "text", "column_type": "bigint", "table": "integer"}[change]
        assert await db._fetch_text("SELECT pg_typeof(value)::text FROM sv_review_view LIMIT 1") == [(expected,)]
    finally:
        await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        await db.close()


async def test_sqlite_rebuild_under_a_view_copies_rows_with_legacy_alter_table_off(tmp_path):
    schema = _migration_schema(tmp_path, "sqlite")
    db = await _empty_database("sqlite", schema)
    try:
        await db.execute("PRAGMA legacy_alter_table=0")
        assert await db._fetch_text("PRAGMA legacy_alter_table") == [("0",)]
        migrations = Migrations(tmp_path / "migrations", schema)
        migrations.make()
        migrator = Migrator(db, migrations)
        await migrator.upgrade()
        models = monorm.load(schema, registry=monorm.Registry())
        await models["ReviewItem"].objects.using(db).insert_many([{"id": 1, "value": 7}, {"id": 2, "value": 9}])
        _migration_schema(tmp_path, "sqlite", extra="@@check(value >= 0)")
        (migration,) = migrations.make("rebuild")
        assert "__orm_new_" in migration.up_sql
        await migrator.upgrade(allow_destructive=True)
        assert await db._fetch_text("PRAGMA legacy_alter_table") == [("0",)]
        assert await db._fetch_text("SELECT id, value FROM sv_review_view ORDER BY id") == [("1", "7"), ("2", "9")]
        assert await db._fetch_text("SELECT id, value FROM sv_review_items ORDER BY id") == [("1", "7"), ("2", "9")]
    finally:
        await db.close()


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_views_pull_back_as_blocks_and_sidecars_without_drift(dialect, tmp_path):
    db = await _empty_database(dialect)
    try:
        await db.execute("CREATE TABLE sv_pull_items (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);"
                         "CREATE VIEW sv_pull_view AS SELECT id, value FROM sv_pull_items WHERE value > 0")
        if dialect == "postgres":
            await db.execute("CREATE MATERIALIZED VIEW sv_pull_totals AS SELECT id, value FROM sv_pull_view;"
                             "CREATE UNIQUE INDEX sv_pull_totals_id ON sv_pull_totals(id)")
        pulled = await pull(db)
        assert "view SvPullView {" in pulled.schema
        assert not [gap for gap in pulled.gaps if "view" in gap]
        assert pulled.differences == []
        schema = tmp_path / "pulled.prisma"
        pulled.write(schema)
        sidecars = tmp_path / "views" / ("main" if dialect == "sqlite" else "public")
        assert "value > 0" in (sidecars / "SvPullView.sql").read_text()
        if dialect == "postgres":
            assert "view SvPullTotals {" in pulled.schema
            assert "@@materialized" in pulled.schema
            assert "@@depends_on([SvPullView])" in pulled.schema
            assert "sv_pull_view" in (sidecars / "SvPullTotals.sql").read_text()
        monorm.load(schema, registry=monorm.Registry())
        migrator = Migrator(db, Migrations(tmp_path / "migrations", schema))
        await migrator.baseline()
        assert (await migrator.drift()).steps == []
    finally:
        if dialect == "postgres":
            await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        await db.close()
