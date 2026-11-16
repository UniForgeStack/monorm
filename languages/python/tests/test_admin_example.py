"""The example extension package (`examples/admin-package/rating.py`): one widget, one detail tab, one route and one action,
plugged in with `install(admin, model)`, through `admin.test_client` on PostgreSQL and SQLite."""

import importlib.util
import os
from pathlib import Path

import pytest

import monorm
from monorm import admin
from monorm_auth import UserMethods, UserQueries, make_password

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
BASE = "/admin/api"
ROOT = {"is_superuser": True, "key": 1}
EXAMPLE = Path(__file__).resolve().parents[3] / "examples" / "admin-package" / "rating.py"
SCHEMA = """datasource db {
  provider = "%s"
}
import "app:monorm-auth" (prefix: "auth_")
import "app:monorm-admin" (prefix: "admin_")

model Movie {
  id    BigInt @id @default(autoincrement())
  title String
  stars Int?
  @@map("movies")
}
"""


def load_example():
    spec = importlib.util.spec_from_file_location("rating_example", EXAMPLE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request, tmp_path):
    (tmp_path / "schema.prisma").write_text(SCHEMA % request.param)
    registry = monorm.Registry()
    models = monorm.load(tmp_path / "schema.prisma", registry=registry, model_methods={"User": UserMethods})
    monorm.use_query_set(models["User"], UserQueries)
    url = "sqlite://:memory:" if request.param == "sqlite" else PG_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    await models["User"].objects.using(db).insert_many([{"username": "root", "email": "root@example.com", "password": make_password("pw"), "is_superuser": True}])
    await models["Movie"].objects.using(db).insert_many([{"title": "Alpha", "stars": 4}, {"title": "Beta", "stars": 2}, {"title": "Gamma", "stars": None}])
    site = admin.Admin(registry)
    load_example().install(site, models["Movie"])
    yield site, db, models
    await db.drop_tables()
    await db.close()


async def movie_id(db, models, title):
    Movie = models["Movie"]
    return (await Movie.objects.using(db).filter(Movie.title == title).first()).id


async def test_the_example_package_plugs_in_with_one_call_and_its_files_and_metadata_are_served(world):
    site, db, _ = world
    client = site.test_client(db, user=ROOT)
    body = (await client.get(f"{BASE}/resources")).json()
    movie = next(r for r in body["resources"] if r["name"] == "Movie")
    assert movie["package_tabs"] == [{"name": "rating.summary", "title": "Summary", "module": "packages/rating/summary.js"}]
    assert [p["name"] for p in body["packages"]] == ["rating"]
    stars = next(c for c in movie["form"] if c["name"] == "stars")
    assert stars["widget"] == "rating.stars"
    widget = await client.get(f"{BASE}/packages/rating/stars.js")
    assert (widget.status, 'from "react"' in widget.text, "adminMajor" in widget.text) == (200, True, True)
    assert (await client.get(f"{BASE}/packages/rating/rating.css")).status == 200


async def test_the_example_route_reads_the_record_and_the_average_behind_the_session(world):
    site, db, models = world
    first = await movie_id(db, models, "Alpha")
    assert (await site.test_client(db).get(f"{BASE}/routes/rating/{first}/summary")).status == 401
    client = site.test_client(db, user=ROOT)
    r = await client.get(f"{BASE}/routes/rating/{first}/summary")
    assert (r.status, r.json()) == (200, {"title": "Alpha", "stars": 4, "count": 2, "average": 3.0})
    assert (await client.get(f"{BASE}/routes/rating/999999/summary")).status == 404


async def test_the_example_action_clears_the_stars_of_one_record(world):
    site, db, models = world
    client = site.test_client(db, user=ROOT)
    first = await movie_id(db, models, "Alpha")
    r = await client.post(f"{BASE}/actions/Movie/reset_stars", json={"keys": [str(first)], "inputs": {}})
    assert r.status == 200
    Movie = models["Movie"]
    assert [(m.title, m.stars) for m in await Movie.objects.using(db).order_by(Movie.title)] == [("Alpha", None), ("Beta", 2), ("Gamma", None)]
