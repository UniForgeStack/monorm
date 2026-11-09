"""Admin form layout and widgets: sections, read-only fields, widgets and the custom form module, on PostgreSQL and SQLite."""

import os

import pytest

import monorm
from artifact_caps import has_extension
from monorm import admin

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SCHEMA = """datasource db {
  provider = "%(provider)s"
}
import "app:monorm-admin" (prefix: "admin_")

model FBook {
  id    Int     @id @default(autoincrement())
  title String  @db.VarChar(20)
  blurb String?
  pages Int     @default(0)
  meta  Json?
  @@map("f_books")
}
"""

GUARDED = """datasource db {
  provider = "%(provider)s"
}
import "app:monorm-admin" (prefix: "admin_")

model FGuard {
  id    Int    @id @default(autoincrement())
  title String @db.VarChar(20)
  @@locking.version
  @@map("f_guards")
}
"""

SU = {"key": 1, "is_superuser": True}
BOOKS = "/admin/api/resources/FBook"


async def open_world(provider, tmp_path, schema):
    (tmp_path / "schema.prisma").write_text(schema % {"provider": provider})
    registry = monorm.Registry()
    models = monorm.load(tmp_path / "schema.prisma", registry=registry)
    url = "sqlite://:memory:" if provider == "sqlite" else PG_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    return registry, models, db


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request, tmp_path):
    registry, models, db = await open_world(request.param, tmp_path, SCHEMA)
    await models["FBook"].objects.using(db).insert_many([{"title": "First", "pages": 10}])
    yield registry, models, db
    await db.drop_tables()
    await db.close()


def site_of(registry, models, config, **options):
    site = admin.Admin(registry, **options)
    site.register(models["FBook"], config)
    return site


def form_of(meta, name):
    resource = next(r for r in meta["resources"] if r["name"] == "FBook")
    return resource, {f["name"]: f for f in resource["form"]}


async def test_sections_and_widgets_reach_the_form_metadata(world):
    registry, models, db = world
    config = {
        "form": {"create": [{"title": "Main", "fields": ["title", "pages"]}], "edit": [{"title": "Main", "fields": ["title"]}, {"title": "More", "fields": ["meta"], "collapsed": True}]},
        "widgets": {"title": "url"},
    }
    site = site_of(registry, models, config, widgets={"String": "email"})
    meta = (await site.test_client(db, user=SU).get("/admin/api/resources")).json()
    resource, fields = form_of(meta, "FBook")
    assert resource["sections"]["create"][0] == {"title": "Main", "fields": ["title", "pages"], "collapsed": False}
    assert resource["sections"]["create"][1]["title"] is None and "blurb" in resource["sections"]["create"][1]["fields"]
    assert resource["sections"]["edit"][1]["collapsed"] is True
    assert fields["title"]["widget"] == "url", "a field key beats a type key"
    assert fields["blurb"]["widget"] == "email", "a site type key applies to a field of that type"
    assert fields["pages"]["widget"] == "number"
    assert resource["form_component"] is False


async def test_a_read_only_field_is_a_422_on_create_and_on_update_and_nothing_is_written(world):
    registry, models, db = world
    client = site_of(registry, models, {"readonly": ["pages"]}).test_client(db, user=SU)
    _, fields = form_of((await client.get("/admin/api/resources")).json(), "FBook")
    assert fields["pages"]["readonly"] is True and fields["title"]["readonly"] is False
    r = await client.post(BOOKS, json={"set": {"title": "T", "pages": 5}})
    assert (r.status, r.json()["fields"]["pages"]) == (422, ["This field is read-only."]), r.text
    r = await client.patch(f"{BOOKS}/1", json={"set": {"pages": 5}, "loaded": {"pages": 10}})
    assert (r.status, r.json()["fields"]["pages"]) == (422, ["This field is read-only."]), r.text
    assert (await models["FBook"].objects.using(db).first()).pages == 10
    assert (await client.post(BOOKS, json={"set": {"title": "T"}})).status == 201


@pytest.mark.parametrize(
    ("config", "match"),
    [
        ({"form": [{"fields": ["nope"]}]}, "is not a field of the form"),
        ({"form": [{"fields": ["title"]}, {"fields": ["title"]}]}, "is in two places"),
        ({"form": [{"fields": ["title"], "collapsed": True}]}, "a collapsed section needs a title"),
        ({"widgets": {"title": "slider"}}, "not one of"),
        ({"widgets": {"pages": "checkbox"}}, "does not fit"),
        ({"readonly": ["title"]}, "read-only and required with no default"),
        ({"form_component": "/no/such/file.js"}, "form_component"),
    ],
)
async def test_a_bad_form_config_fails_the_build(world, config, match):
    registry, models, _ = world
    with pytest.raises(admin.AdminConfigError, match=match):
        site_of(registry, models, config).check()


async def test_a_config_of_the_wrong_shape_is_refused_before_the_build(world):
    registry, models, _ = world
    for config in ({"form": "x"}, {"form": [{"title": "T"}]}, {"form": {"create": {"fields": ["title"]}}}, {"readonly": "pages"}, {"widgets": {"title": 1}}):
        with pytest.raises(admin.AdminConfigError):
            site_of(registry, models, config)._config_json()


async def test_the_form_module_comes_back_to_staff_as_javascript(world, tmp_path):
    registry, models, db = world
    module = tmp_path / "form.js"
    module.write_text("export default (React) => function Form() { return null; };")
    client = site_of(registry, models, {"form_component": str(module)}).test_client(db, user=SU)
    assert form_of((await client.get("/admin/api/resources")).json(), "FBook")[0]["form_component"] is True
    r = await client.get("/admin/api/forms/FBook")
    assert r.status == 200 and "export default" in r.text
    assert (await site_of(registry, models, {"form_component": str(module)}).test_client(db).get("/admin/api/forms/FBook")).status == 401
    plain = site_of(registry, models, {}).test_client(db, user=SU)
    assert (await plain.get("/admin/api/forms/FBook")).status == 404


@pytest.mark.skipif(not has_extension("monorm.locking"), reason="requires locking")
@pytest.mark.parametrize("provider", ["sqlite", "postgres"])
async def test_a_read_only_guard_field_is_still_compared_and_never_written(provider, tmp_path):
    registry, models, db = await open_world(provider, tmp_path, GUARDED)
    try:
        await models["FGuard"].objects.using(db).insert_many([{"title": "One"}])
        site = admin.Admin(registry)
        site.register(models["FGuard"], {"readonly": ["version"]})
        client = site.test_client(db, user=SU)
        url = "/admin/api/resources/FGuard/1"
        r = await client.patch(url, json={"set": {"version": 9}, "loaded": {"version": 0}})
        assert (r.status, r.json()["fields"]["version"]) == (422, ["This field is read-only."]), r.text
        r = await client.patch(url, json={"set": {"title": "Two"}, "loaded": {"title": "One", "version": 5}})
        assert r.status == 409 and r.json()["conflicts"]["version"] == {"loaded": 5, "current": 0}, r.text
        r = await client.patch(url, json={"set": {"title": "Two"}, "loaded": {"title": "One", "version": 0}})
        assert (r.status, r.json()["changed"]) == (200, ["title"]), r.text
        row = await models["FGuard"].objects.using(db).first()
        assert (row.title, row.version) == ("Two", 1)
    finally:
        await db.drop_tables()
        await db.close()
