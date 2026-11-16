"""Extension packages (`Admin.package`): the files, the session, the version check and the metadata, through
`admin.test_client` on PostgreSQL and SQLite."""

import os

import pytest

import monorm
from monorm import admin
from monorm_auth import UserMethods, UserQueries, make_password

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
BASE = "/admin/api"
ROOT = {"is_superuser": True, "key": 1}
SCHEMA = """datasource db {
  provider = "%s"
}
import "app:monorm-auth" (prefix: "auth_")
import "app:monorm-admin" (prefix: "admin_")
"""


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
    package = tmp_path / "rating"
    (package / "lib").mkdir(parents=True)
    (package / "stars.js").write_text('import React from "react";\nexport const adminMajor = 1;\nexport default () => null;\n')
    (package / "lib" / "form.mjs").write_text("export const adminMajor = 1;\nexport default () => null;\n")
    (package / "lib" / "reviews.js").write_text("export const adminMajor = 1;\nexport default () => null;\n")
    (package / "stars.css").write_text(".stars { color: gold }\n")
    (tmp_path / "secret.js").write_text("secret")
    yield registry, db, models, package
    await db.drop_tables()
    await db.close()


def build(world, **options):
    registry, db, models, package = world
    site = admin.Admin(registry, widgets={"Role.name": "rating.stars"})
    site.register(models["Role"], {"detail_tabs": ["rating.reviews"]})
    site.package("rating", package, widgets={"stars": "stars.js"}, forms={"edit": "lib/form.mjs"}, tabs={"reviews": "lib/reviews.js"}, styles=["stars.css"], **options)
    return site, db


async def test_a_package_file_is_served_to_a_signed_in_user_with_its_type(world):
    site, db = build(world)
    client = site.test_client(db, user=ROOT)
    js = await client.get(f"{BASE}/packages/rating/stars.js")
    assert js.status == 200
    assert "import React" in js.text
    assert (await client.get(f"{BASE}/packages/rating/lib/form.mjs")).status == 200
    css = await client.get(f"{BASE}/packages/rating/stars.css")
    assert (css.status, ".stars" in css.text) == (200, True)


async def test_a_package_file_needs_a_session_and_only_loaded_files_are_served(world):
    site, db = build(world)
    assert (await site.test_client(db).get(f"{BASE}/packages/rating/stars.js")).status == 401
    client = site.test_client(db, user=ROOT)
    for path in ("secret.js", "../secret.js", "%2e%2e/secret.js", "nothing.js", "lib"):
        assert (await client.get(f"{BASE}/packages/rating/{path}")).status == 404, path
    assert (await client.get(f"{BASE}/packages/other/stars.js")).status == 404


async def test_the_resources_route_lists_the_package_and_the_admin_major(world):
    site, db = build(world)
    body = (await site.test_client(db, user=ROOT).get(f"{BASE}/resources")).json()
    assert body["admin_major"] == 1
    role = next(r for r in body["resources"] if r["name"] == "Role")
    assert role["package_tabs"] == [{"name": "rating.reviews", "title": "Reviews", "module": "packages/rating/lib/reviews.js"}]
    assert body["packages"] == [
        {"name": "rating", "admin_major": 1, "widgets": {"stars": "packages/rating/stars.js"}, "forms": {"edit": "packages/rating/lib/form.mjs"}, "tabs": {"reviews": "packages/rating/lib/reviews.js"}, "styles": ["packages/rating/stars.css"]}
    ]


async def test_a_package_built_for_another_admin_major_is_a_build_error(world):
    site, _ = build(world, admin_major=2)
    with pytest.raises(admin.AdminConfigError, match='built for admin major 2; this admin is major 1'):
        site.permissions


async def test_check_reports_a_package_for_another_admin_major_as_a_build_error(world):
    site, _ = build(world, admin_major=2)
    with pytest.raises(admin.AdminConfigError, match="built for admin major 2; this admin is major 1"):
        site.check()


async def test_a_package_that_is_added_twice_or_names_a_missing_file_is_refused(world):
    site, _ = build(world)
    with pytest.raises(admin.AdminConfigError, match="added twice"):
        site.package("rating", world[3])
    registry, _, models, package = world
    bad = admin.Admin(registry)
    bad.register(models["Role"])
    bad.package("rating", package, widgets={"stars": "missing.js"})
    with pytest.raises(admin.AdminConfigError, match="missing.js"):
        bad.permissions


async def test_a_detail_tab_of_no_package_is_a_build_error(world):
    registry, _, models, package = world
    site = admin.Admin(registry)
    site.register(models["Role"], {"detail_tabs": ["rating.nothing"]})
    site.package("rating", package, tabs={"reviews": "lib/reviews.js"})
    with pytest.raises(admin.AdminConfigError, match="rating.nothing"):
        site.permissions


async def test_a_widget_of_no_package_is_a_build_error(world):
    registry, _, models, package = world
    site = admin.Admin(registry, widgets={"Role.name": "rating.nothing"})
    site.register(models["Role"])
    site.package("rating", package, widgets={"stars": "stars.js"})
    with pytest.raises(admin.AdminConfigError, match="rating.nothing"):
        site.permissions
