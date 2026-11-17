"""The base path of the admin and the cookie scope, through `admin.test_client` on PostgreSQL and SQLite."""

import csv
import io
import os

import pytest

import monorm
from monorm import admin
from monorm_auth import UserMethods, UserQueries, make_password

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
SECRET_A = "a" * 40
SECRET_B = "b" * 40
SU = {"is_superuser": True}
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
    await models["User"].objects.using(db).insert_many([{"username": "ann", "email": "ann@example.com", "password": make_password("pw"), "is_superuser": True}])
    await models["Role"].objects.using(db).insert_many([{"name": "editors"}, {"name": "readers"}])
    yield registry, db, models
    await db.drop_tables()
    await db.close()


def site_at(registry, models, base_path, **options):
    site = admin.Admin(registry, base_path=base_path, **options)
    site.register(models["Role"])
    return site


def set_cookie(reply, name):
    return next((v for k, v in reply.headers if k.lower() == "set-cookie" and v.startswith(name + "=")), None)


async def summary(ctx, request):
    return {"rows": 2}


async def test_a_site_at_a_base_path_serves_list_detail_write_export_and_a_custom_route_there_only(world):
    registry, db, models = world
    site = site_at(registry, models, "/bundler/admin/")
    site.route("GET", "reports/summary", summary)
    assert (site.base_path, site.prefix) == ("/bundler/admin/", None)
    client = site.test_client(db, user=SU)
    api = "/bundler/admin/api"
    preamble = await client.get(f"{api}/resources")
    listing = await client.get(f"{api}/resources/Role")
    assert listing.status == 200, listing.text
    assert (await client.get(f"{api}/resources/Role/1")).status == 200
    created = await client.post(f"{api}/resources/Role", json={"set": {"name": "ops"}})
    assert created.status == 201, created.text
    assert (listing.queries, created.queries) == (preamble.queries + 1, preamble.queries + 2)  # the base path adds no statement
    role = await models["Role"].objects.using(db).filter(models["Role"].name == "ops").first()
    assert role is not None and created.json()["key"] == str(role.id)
    entry = models["AdminLogEntry"]
    rows = [r for r in await entry.objects.using(db).order_by(entry.id)]
    assert [(r.action, r.resource, r.record_key) for r in rows] == [("create", "Role", str(role.id))]
    exported = await client.get(f"{api}/export/Role")
    assert exported.status == 200, exported.text
    assert {"editors", "ops"} <= {cell for row in csv.reader(io.StringIO(exported.text, newline="")) for cell in row}
    assert (await client.get(f"{api}/routes/reports/summary")).json() == {"rows": 2}
    assert (await client.get("/admin/api/resources/Role")).status != 200
    assert site.url(models["Role"], 1) == "/bundler/admin/Role/1"


async def test_two_sites_on_one_host_keep_their_own_cookie_paths_and_sessions(world):
    registry, db, models = world
    a = site_at(registry, models, "/a/admin").test_client(db, secret=SECRET_A)
    b = site_at(registry, models, "/b/admin").test_client(db, secret=SECRET_B)
    login_a = await a.login("ann", "pw")
    assert login_a.status == 200, login_a.text
    session = set_cookie(login_a, "__Secure-monorm-session")
    csrf = set_cookie(login_a, "__Secure-monorm-csrf")
    assert session and "Path=/a/admin/" in session and "HttpOnly" in session and "Secure" in session and "SameSite=Lax" in session
    assert csrf and "Path=/a/admin/" in csrf and "HttpOnly" not in csrf
    assert (await a.get("/a/admin/api/me")).status == 200
    assert (await b.get("/b/admin/api/me")).status == 401
    login_b = await b.login("ann", "pw")
    assert "Path=/b/admin/" in set_cookie(login_b, "__Secure-monorm-session")
    out = await a.logout()
    assert all("Path=/a/admin/" in v for k, v in out.headers if k.lower() == "set-cookie")
    assert (await a.get("/a/admin/api/me")).status == 401
    assert (await b.get("/b/admin/api/me")).status == 200


def check_error(site):
    with pytest.raises(admin.AdminConfigError) as e:
        site.check()
    return str(e.value)


async def test_check_names_a_bad_base_path_and_a_prefix_outside_the_base(world):
    registry, db, models = world
    assert "must start with /" in check_error(site_at(registry, models, "admin"))
    assert "is empty" in check_error(site_at(registry, models, "/"))
    assert "use letters, digits" in check_error(site_at(registry, models, "/a b"))
    assert "empty or dot segment" in check_error(site_at(registry, models, "/a/../b"))
    assert "must sit under the base path" in check_error(site_at(registry, models, "/bundler/admin", prefix="/other/api"))
    assert "path of the UI" in check_error(site_at(registry, models, "/bundler/admin", prefix="/bundler/admin"))
    assert "API prefix" in check_error(site_at(registry, models, "/bundler/admin", prefix="/bundler/admin/Role/v1"))
    for taken in ["assets", "shared", "index.html", "-", "-/login"]:
        assert "for its pages and files" in check_error(site_at(registry, models, "/bundler/admin", prefix=f"/bundler/admin/{taken}")), taken
    assert site_at(registry, models, "/bundler/admin", prefix="/bundler/admin/back").check() == []
