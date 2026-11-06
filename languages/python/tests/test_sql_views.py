"""A SQL view model reads, refuses every write with `ReadOnlyModel` before SQL, and a materialized view refreshes."""

import os
import re
from pathlib import Path

import pytest

import monorm
from monorm import _native

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


async def _open(tmp_path: Path, dialect: str):
    registry = monorm.Registry()
    models = monorm.load(_schema(tmp_path, dialect), registry=registry)
    db = await monorm.connect("sqlite://:memory:" if dialect == "sqlite" else URL, registry=registry, default=False)
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
