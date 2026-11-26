"""The `banners` resource option: one host call per detail read, the banners in the detail body, one danger banner on a failure."""

import pytest

import monorm
from monorm import admin

API = "/admin/api/resources/Note"
ROOT = {"is_superuser": True, "key": 1}
SCHEMA = """datasource db {
  provider = "sqlite"
}
import "app:monorm-auth" (prefix: "auth_")
import "app:monorm-admin" (prefix: "admin_")

model Note {
  id    Int @id
  title String
}
"""


@pytest.fixture
async def world(tmp_path):
    (tmp_path / "schema.prisma").write_text(SCHEMA)
    registry = monorm.Registry()
    models = monorm.load(tmp_path / "schema.prisma", registry=registry)
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    await models["Note"].objects.using(db).insert_many([{"id": 1, "title": "Alpha"}, {"id": 2, "title": "Beta"}])
    yield registry, models, db
    await db.close()


def site_of(world, banners):
    registry, models, _ = world
    site = admin.Admin(registry)
    site.register(models["Note"], {"banners": banners, "list": {"columns": ["title"], "order": ["id"]}})
    return site


def client_of(world, banners):
    return site_of(world, banners).test_client(world[2], user=ROOT)


async def test_a_banners_function_gets_the_record_once_per_detail_read(world):
    seen = []

    async def banners(record):
        seen.append(record)
        return [{"level": "warning", "text": f"Check {record['title']}"}]

    client = client_of(world, banners)
    body = (await client.get(f"{API}/2")).json()
    assert body["banners"] == [{"level": "warning", "text": "Check Beta"}]
    assert [r["title"] for r in seen] == ["Beta"]
    assert (await client.get(API)).status == 200
    assert len(seen) == 1, "a list read does not call the function"


async def test_a_failing_or_wrong_banners_function_gives_one_danger_banner_and_the_page_loads(world):
    async def fails(record):
        raise ValueError("secret detail")

    async def wrong(record):
        return [{"level": "loud", "text": "x"}]

    for function in (fails, wrong):
        response = await client_of(world, function).get(f"{API}/1")
        assert response.status == 200
        banners = response.json()["banners"]
        assert [b["level"] for b in banners] == ["danger"]
        assert "secret" not in banners[0]["text"]


async def test_banners_over_a_limit_are_cut_and_no_danger_banner_shows(world):
    async def many(record):
        return [{"level": "info", "text": f"n{i}"} for i in range(25)]

    async def long(record):
        return [{"level": "warning", "text": "é" * 600}]

    banners = (await client_of(world, many).get(f"{API}/1")).json()["banners"]
    assert [b["text"] for b in banners] == [f"n{i}" for i in range(20)]
    assert {b["level"] for b in banners} == {"info"}
    banners = (await client_of(world, long).get(f"{API}/1")).json()["banners"]
    assert [b["level"] for b in banners] == ["warning"]
    assert len(banners[0]["text"]) == 500


async def test_a_banners_function_that_is_not_callable_is_refused(world):
    with pytest.raises(admin.AdminConfigError, match="not a host function"):
        site_of(world, "warn").check()
