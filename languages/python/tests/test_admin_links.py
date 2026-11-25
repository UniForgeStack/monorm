"""Admin read-only links: a generic relation and its reverse show as links to the related rows, through `admin.test_client`, on SQLite."""

import pytest

import monorm
from artifact_caps import has_extension
from monorm import admin
from monorm.__main__ import main as cli

SU = {"key": 1, "is_superuser": True}
API = "/admin/api"

SCHEMA = """datasource db {
  provider = "sqlite"
}

model GPost {
  id   Int    @id @default(autoincrement())
  name String
  tags GTag[] @generic.reverse
  @@map("gl_posts")
}

model GPhoto {
  id   Int    @id @default(autoincrement())
  name String
  @@map("gl_photos")
}

model GTag {
  id     Int      @id @default(autoincrement())
  name   String
  target Generic? @generic.relation(targets: ["GPost", "GPhoto"])
  @@map("gl_tags")
}
"""

pytestmark = pytest.mark.skipif(not has_extension("monorm.generic"), reason="requires the monorm.generic package")


@pytest.fixture
async def world(tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text(SCHEMA)
    assert cli(["--schema", str(schema), "identities"]) == 0
    registry = monorm.Registry()
    models = monorm.load(schema, registry=registry)
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    using = lambda name: models[name].objects.using(db)  # noqa: E731
    await using("GPost").insert_many([{"name": "Hello"}, {"name": "Other"}])
    await using("GPhoto").insert_many([{"name": "Sunset"}])
    kind = models["ContentType"]
    await using("GTag").insert_many(
        [
            {"name": "red", "target_type": kind.GPost, "target_id": 1},
            {"name": "blue", "target_type": kind.GPost, "target_id": 1},
            {"name": "warm", "target_type": kind.GPhoto, "target_id": 1},
            {"name": "none"},
            {"name": "gone", "target_type": kind.GPost, "target_id": 9},
        ]
    )
    post, photo, tag = models["GPost"], models["GPhoto"], models["GTag"]
    site = admin.Admin(registry)
    site.register(post, {"list": {"columns": [post.name], "order": [post.id]}, "relations": {"tags": {}}})
    site.register(photo, {"list": {"columns": [photo.name], "order": [photo.id]}})
    site.register(tag, {"list": {"columns": [tag.name, tag.target_id, tag.target_type], "order": [tag.id]}, "relations": {"target": {}}})
    yield site.test_client(db, user=SU)
    await db.drop_tables()
    await db.close()


def items(reply):
    return [(i["resource"], i["label"]) for i in reply.json()["items"]]


@pytest.mark.case("admin_links::a_union_link_leads_to_the_row_that_the_type_selects")
async def test_a_union_link_leads_to_the_row_that_the_type_selects(world):
    assert items(await world.get(f"{API}/links/GTag/1/target")) == [("GPost", "Hello")]
    assert items(await world.get(f"{API}/links/GTag/3/target")) == [("GPhoto", "Sunset")]
    for key in (4, 5):
        assert items(await world.get(f"{API}/links/GTag/{key}/target")) == [], "no type, or a row that is gone, leaves no link"
    assert (await world.get(f"{API}/links/GTag/99/target")).status == 404


@pytest.mark.case("admin_links::a_conditioned_to_many_link_lists_the_rows_that_match")
async def test_a_conditioned_to_many_link_lists_the_rows_that_match(world):
    assert items(await world.get(f"{API}/links/GPost/1/tags")) == [("GTag", "red"), ("GTag", "blue")]
    assert items(await world.get(f"{API}/links/GPost/2/tags")) == []


@pytest.mark.case("admin_links::a_link_has_no_picker_and_the_meta_names_it")
async def test_a_link_has_no_picker_and_the_meta_names_it(world):
    assert (await world.get(f"{API}/autocomplete/GTag/target")).status == 404
    meta = (await world.get(f"{API}/site")).json()
    tag = next(r for r in meta["resources"] if r["name"] == "GTag")
    assert tag["linked"] == [{"name": "target", "title": "Target", "many": False}]
    column = next(c for c in tag["fields"] if c["label"] == "target_id")
    assert column["ref_by"] == {"column": "target_type", "targets": {"GPost": "GPost", "GPhoto": "GPhoto"}}
