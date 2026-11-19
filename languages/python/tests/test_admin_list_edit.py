"""Edit in the list (`list.editable`): the host dispatch of one save of many rows, on PostgreSQL and SQLite."""

import os

import pytest

import monorm
from monorm import admin
from monorm.adapters import register_field_adapter
from monorm.errors import ValidationError

class Lower:
    def validate(self, value):
        return value.lower()

    async def prepare(self, value):
        return value

    def decode(self, stored):
        return stored


PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SCHEMA = """datasource db {
  provider = "%(provider)s"
}
import "app:monorm-admin" (prefix: "admin_")

model LNote {
  id    Int    @id @default(autoincrement())
  title String @db.VarChar(20)
  slug  String @default("")
  @@map("l_notes")
}
"""

NOTES = "/admin/api/resources/LNote"
SU = {"key": 1, "is_superuser": True}


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request, tmp_path):
    (tmp_path / "schema.prisma").write_text(SCHEMA % {"provider": request.param})
    registry = monorm.Registry()
    models = monorm.load(tmp_path / "schema.prisma", registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else PG_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    await models["LNote"].objects.using(db).insert_many([{"title": "One"}, {"title": "Two"}, {"title": "Three"}])
    yield registry, db, models
    await db.drop_tables()
    await db.close()


def client_of(registry, models, config, db):
    site = admin.Admin(registry)
    site.register(models["LNote"], {"list": {"columns": ["title", "slug"], "editable": [models["LNote"].title, "slug"], "order": [models["LNote"].id]}, **config})
    return site.test_client(db, user=SU)


async def titles(db, models):
    note = models["LNote"]
    return [r.title for r in await note.objects.using(db).order_by(note.id)]


def rows(*pairs):
    return {"rows": [{"key": str(key), "set": {"title": new}, "loaded": {"title": old}} for key, new, old in pairs]}


async def test_the_list_names_its_editable_columns_and_a_wrong_name_is_a_build_error(world):
    registry, db, models = world
    r = await client_of(registry, models, {}, db).get("/admin/api/resources")
    meta = next(res for res in r.json()["resources"] if res["name"] == "LNote")
    assert meta["editable"] == ["title", "slug"]
    site = admin.Admin(registry)
    site.register(models["LNote"], {"list": {"columns": ["title"], "editable": ["slug"]}})
    with pytest.raises(admin.AdminConfigError, match="not a column of the list"):
        site.check()


async def test_the_default_writer_saves_the_rows_of_a_list_in_one_request_and_a_failing_row_rolls_all_back(world):
    registry, db, models = world
    client = client_of(registry, models, {}, db)
    r = await client.patch(NOTES, json=rows((1, "Uno", "One"), (3, "Tres", "Three")))
    assert r.status == 200, r.text
    assert await titles(db, models) == ["Uno", "Two", "Tres"]
    r = await client.patch(NOTES, json=rows((1, "Eins", "Uno"), (2, "x" * 21, "Two")))
    assert r.status == 400, r.text
    assert await titles(db, models) == ["Uno", "Two", "Tres"]


async def test_a_service_runs_once_per_changed_row_in_one_transaction_and_a_failure_in_row_three_rolls_back_rows_one_and_two(world):
    registry, db, models = world
    note = models["LNote"]
    seen = []
    fired = []

    async def update(ctx, qs, changes):
        row = await qs.first()
        seen.append(row.id)
        if changes["title"] == "boom":
            raise ValidationError({"title": ["No boom."]})
        async with ctx.database.transaction():
            await qs.update(title=changes["title"])
        await ctx.database.on_commit(lambda: fired.append(row.id))

    client = client_of(registry, models, {"update": update}, db)
    r = await client.patch(NOTES, json=rows((1, "Uno", "One"), (2, "Two", "Two"), (3, "Tres", "Three")))
    assert r.status == 200, r.text
    assert seen == [1, 3]
    assert await titles(db, models) == ["Uno", "Two", "Tres"]
    assert fired == [1, 3]
    seen.clear()
    fired.clear()
    r = await client.patch(NOTES, json=rows((1, "Eins", "Uno"), (2, "Zwei", "Two"), (3, "boom", "Tres")))
    assert r.status == 400, r.text
    assert r.json()["rows"]["2"]["fields"]["title"] == ["No boom."]
    assert seen == [1, 2, 3]
    assert await titles(db, models) == ["Uno", "Two", "Tres"]
    assert fired == []
    assert len(await note.objects.using(db).all()) == 3


async def test_a_threaded_sync_service_writes_in_the_save_transaction_and_a_later_failure_rolls_it_back(world):
    registry, db, models = world
    fired = []

    @admin.threaded
    def update(ctx, qs, changes):
        if changes["title"] == "boom":
            raise ValidationError({"title": ["No boom."]})
        ctx.run(qs.update(title=changes["title"]))
        ctx.run(ctx.database.on_commit(lambda: fired.append(changes["title"])))

    client = client_of(registry, models, {"update": update}, db)
    r = await client.patch(NOTES, json=rows((1, "Uno", "One"), (2, "Dos", "Two")))
    assert r.status == 200, r.text
    assert (await titles(db, models), fired) == (["Uno", "Dos", "Three"], ["Uno", "Dos"])
    fired.clear()
    r = await client.patch(NOTES, json=rows((1, "Eins", "Uno"), (3, "boom", "Three")))
    assert r.status == 400, r.text
    assert (await titles(db, models), fired) == (["Uno", "Dos", "Three"], [])


async def test_a_changed_row_answers_409_by_row_and_the_save_writes_nothing(world):
    registry, db, models = world
    note = models["LNote"]
    client = client_of(registry, models, {}, db)
    await note.objects.using(db).filter(note.id == 2).update(title="Dos")
    r = await client.patch(NOTES, json=rows((1, "Uno", "One"), (2, "Zwei", "Two")))
    assert r.status == 409, r.text
    assert r.json()["conflicts"] == {"1": {"title": {"loaded": "Two", "current": "Dos"}}}
    assert await titles(db, models) == ["One", "Dos", "Three"]


async def test_a_model_with_host_behavior_saves_through_the_default_writer_in_one_transaction(world):
    registry, db, models = world
    note = models["LNote"]
    register_field_adapter(note, "slug", Lower())
    client = client_of(registry, models, {}, db)
    body = {"rows": [{"key": "1", "set": {"slug": "AbC"}, "loaded": {"slug": ""}}, {"key": "2", "set": {"title": "Zwei"}, "loaded": {"title": "Two"}}]}
    r = await client.patch(NOTES, json=body)
    assert r.status == 200, r.text
    assert [(x.title, x.slug) for x in await note.objects.using(db).order_by(note.id)] == [("One", "abc"), ("Zwei", ""), ("Three", "")]
    entry = models["AdminLogEntry"]
    assert [(x.action, x.record_key) for x in await entry.objects.using(db).order_by(entry.id)] == [("update", "1"), ("update", "2")]
    r = await client.patch(NOTES, json=rows((1, "Uno", "One"), (3, "x" * 21, "Three")))
    assert r.status == 400, r.text
    assert "3" not in r.json()["rows"] and r.json()["rows"]["1"]["fields"]["title"]
    r = await client.patch(NOTES, json=rows((1, "Uno", "One"), (2, "Dos", "Old")))
    assert r.status == 409, r.text
    assert r.json()["conflicts"] == {"1": {"title": {"loaded": "Old", "current": "Zwei"}}}
    assert await titles(db, models) == ["One", "Zwei", "Three"]
    assert len(await entry.objects.using(db).all()) == 2
