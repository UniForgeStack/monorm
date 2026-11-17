"""The built-in resources of the admin on the auth tables: roles, permissions, role membership and the read-only audit log."""

import os

import pytest

import monorm
from monorm import admin
from monorm_auth import UserMethods, UserQueries, make_password

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
SCHEMA = """datasource db {
  provider = "%s"
}
import "app:monorm-auth" (prefix: "auth_")
import "app:monorm-admin" (prefix: "admin_")
"""
API = "/admin/api"
SU = {"is_superuser": True}


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
    await models["User"].objects.using(db).insert_many([{"username": "ann", "email": "ann@example.com", "password": make_password("pw")}])
    await models["Role"].objects.using(db).insert_many([{"name": "editor"}])
    site = admin.Admin(registry)
    site.register(models["User"])
    await site.sync_permissions(db)
    # A permission that no admin option names must not reach the role form.
    await models["Permission"].objects.using(db).insert_many([{"label": "other", "codename": "x", "name": "Can x"}])
    yield site, db, models
    await db.drop_tables()
    await db.close()


async def test_the_built_in_resources_are_listed_without_any_registration(world):
    site, db, _ = world
    body = (await site.test_client(db, user=SU).get(f"{API}/resources")).json()
    assert [r["name"] for r in body["resources"]] == ["User", "AdminLogEntry", "Permission", "Role", "UserRole"]


async def test_the_role_form_offers_only_the_permission_names_of_the_site(world):
    site, db, _ = world
    client = site.test_client(db, user=SU)
    items = (await client.get(f"{API}/autocomplete/Role/permissions?limit=50")).json()["items"]
    labels = [i["label"] for i in items]
    assert len(labels) == len(site.permissions) and "Can x" not in labels
    assert "Can read role" in labels


async def test_the_audit_log_is_read_only_even_for_a_superuser(world):
    site, db, _ = world
    client = site.test_client(db, user=SU)
    assert (await client.get(f"{API}/resources/AdminLogEntry")).status == 200
    for reply in (
        await client.post(f"{API}/resources/AdminLogEntry", json={"set": {"action": "login"}}),
        await client.patch(f"{API}/resources/AdminLogEntry/1", json={"set": {"action": "x"}}),
        await client.delete(f"{API}/resources/AdminLogEntry/1"),
    ):
        assert reply.status == 403, reply.text


async def test_the_role_membership_of_a_user_is_a_resource(world):
    site, db, models = world
    client = site.test_client(db, user=SU)
    user = await models["User"].objects.using(db).filter(models["User"].username == "ann").first()
    role = await models["Role"].objects.using(db).filter(models["Role"].name == "editor").first()
    made = await client.post(f"{API}/resources/UserRole", json={"set": {"user_id": user.id, "role_id": role.id}})
    assert made.status == 201, made.text
    listed = (await client.get(f"{API}/resources/UserRole")).json()
    names = [c["label"] for c in listed["columns"]]
    rows = [dict(zip(names, row)) for row in listed["rows"]]
    assert [(r["user_id"], r["role_id"]) for r in rows] == [(user.id, role.id)]
