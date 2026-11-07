"""Permission strategies, sync and seeded roles, through `admin.test_client(user=...)`."""

import os

import pytest

import monorm
from monorm import admin
from monorm._admin_permissions import resolve_pattern_grants
from monorm_auth import UserMethods, UserQueries, make_password

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
SCHEMA = """datasource db {
  provider = "%s"
}
import "app:monorm-auth" (prefix: "auth_")
import "app:monorm-admin" (prefix: "admin_")
"""
ROLES = "/admin/api/resources/Role"
LIST = "/admin/api/resources"


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
    pw = make_password("pw")
    await models["User"].objects.using(db).insert_many(
        [{"username": name, "email": f"{name}@example.com", "password": pw, "is_superuser": name == "root"} for name in ("ann", "bob", "root")]
    )
    site = admin.Admin(registry)
    site.register(models["Role"])
    site.register(models["Permission"], {"allow": ["read"]})
    yield site, db, models
    await db.drop_tables()
    await db.close()


async def grant(db, models, username, names):
    """The user holds `names` through a role of the same name as the user."""
    perms = models["Permission"]
    rows = [r for r in await perms.objects.using(db) if f"{r.label}.{r.codename}" in names]
    assert len(rows) == len(names)
    Role = models["Role"]
    await Role.objects.using(db).insert_many([{"name": f"for-{username}"}])
    role = await Role.objects.using(db).filter(Role.name == f"for-{username}").first()
    user = await models["User"].objects.using(db).filter(models["User"].username == username).first()
    await models["RolePermission"].objects.using(db).insert_many([{"role_id": role.id, "permission_id": p.id} for p in rows])
    await models["UserRole"].objects.using(db).insert_many([{"user_id": user.id, "role_id": role.id}])
    return user.id


async def audit(db, models):
    entry = models["AdminLogEntry"]
    return [(r.user_key, r.action, r.resource) for r in await entry.objects.using(db).order_by(entry.id)]


def test_each_strategy_lists_its_names(world):
    site, db, models = world
    assert site.permissions == ["admin.access", "admin.read_role", "admin.manage_role", "admin.read_permission", "admin.manage_permission"]
    registry = site._registry
    model = admin.Admin(registry, permissions="model", label="staff")
    model.register(models["Role"], {"actions": ["export"], "field_permissions": ["see_name"]})
    assert model.permissions == [
        "admin.access",
        "staff.add_role",
        "staff.change_role",
        "staff.delete_role",
        "staff.view_role",
        "staff.see_name",
        "staff.export_role",
    ]
    only = admin.Admin(registry, permissions="access")
    only.register(models["Role"])
    assert only.permissions == ["admin.access"]


def test_perms_with_delete_adds_a_delete_name(world):
    site, db, models = world
    split = admin.Admin(site._registry)
    split.register(models["Role"], {"perms": ["read", "manage", "delete"]})
    assert split.permissions[-1] == "admin.delete_role"


def test_a_wrong_permission_option_is_an_admin_config_error(world):
    site, db, models = world
    bad = [
        (admin.Admin(site._registry, permissions="roles"), None, "page"),
        (admin.Admin(site._registry), {"allow": ["write"]}, "allow"),
        (admin.Admin(site._registry), {"perms": ["read"]}, "perms"),
        (admin.Admin(site._registry, permissions="model"), {"perms": ["read", "manage"]}, "page strategy"),
        (admin.Admin(site._registry), {"actions": ["Bad.Name"]}, "lowercase"),
    ]
    for candidate, config, text in bad:
        candidate.register(models["Role"], config)
        with pytest.raises(admin.AdminConfigError, match=text):
            candidate.check()
    with pytest.raises(admin.AdminConfigError, match="unknown option"):
        site.register(models["User"], {"bogus": 1})  # type: ignore[typeddict-unknown-key]


async def test_sync_creates_the_names_once_and_reports_stale_names_without_deleting_them(world):
    site, db, models = world
    first = await site.sync_permissions(db)
    assert first.created == 5 and first.stale == ()
    perm = models["Permission"]
    await perm.objects.using(db).insert_many([{"label": "admin", "codename": "gone", "name": "Can gone"}, {"label": "other", "codename": "x", "name": "x"}])
    second = await site.sync_permissions(db)
    assert (second.created, second.stale) == (0, ("admin.gone",))
    assert await perm.objects.using(db).count() == 7


async def test_a_role_grant_reads_and_a_missing_grant_is_a_403_with_an_audit_row(world):
    site, db, models = world
    await site.sync_permissions(db)
    ann = await grant(db, models, "ann", ["admin.access", "admin.read_role"])
    bob = await grant(db, models, "bob", ["admin.read_role"])
    assert (await site.test_client(db, user={"key": ann}).get(ROLES)).status == 200
    assert (await site.test_client(db, user={"key": ann}).get("/admin/api/resources/Permission")).status == 403
    assert (await site.test_client(db, user={"key": bob}).get(ROLES)).status == 403
    assert await audit(db, models) == [(str(ann), "permission_denied", "Permission"), (str(bob), "permission_denied", "")]


async def test_manage_includes_read_and_model_view_or_change_reads(world):
    site, db, models = world
    await site.sync_permissions(db)
    ann = await grant(db, models, "ann", ["admin.access", "admin.manage_role"])
    assert (await site.test_client(db, user={"key": ann}).get(ROLES)).status == 200
    model = admin.Admin(site._registry, permissions="model")
    model.register(models["Role"])
    await model.sync_permissions(db)
    bob = await grant(db, models, "bob", ["admin.access", "admin.change_role"])
    assert (await model.test_client(db, user={"key": bob}).get(ROLES)).status == 200
    assert (await model.test_client(db, user={"key": ann}).get(ROLES)).status == 403


async def test_a_superuser_has_everything_but_allow_limits_it_and_an_inactive_user_has_nothing(world):
    site, db, models = world
    su = {"is_superuser": True}
    assert (await site.test_client(db, user=su).get(ROLES)).status == 200
    assert (await site.test_client(db, user={**su, "is_active": False}).get(ROLES)).status == 403
    limited = admin.Admin(site._registry)
    limited.register(models["Role"], {"allow": ["create"]})
    limited.register(models["Permission"])
    assert (await limited.test_client(db, user=su).get(ROLES)).status == 403
    names = [r["name"] for r in (await limited.test_client(db, user=su).get(LIST)).json()["resources"]]
    assert names == ["Permission"]


async def test_the_grants_cost_one_statement_for_a_user_and_none_for_a_superuser(world):
    site, db, models = world
    await site.sync_permissions(db)
    ann = await grant(db, models, "ann", ["admin.access", "admin.read_role"])
    root = await site.test_client(db, user={"is_superuser": True}).get(ROLES)
    plain = await site.test_client(db, user={"key": ann}).get(ROLES)
    assert plain.queries == root.queries + 1


async def test_metadata_has_only_what_the_user_can_use_and_the_filtered_names(world):
    site, db, models = world
    await site.sync_permissions(db)
    ann = await grant(db, models, "ann", ["admin.access", "admin.read_role", "admin.manage_permission"])
    body = (await site.test_client(db, user={"key": ann}).get(LIST)).json()
    assert [r["name"] for r in body["resources"]] == ["Role", "Permission"]
    assert body["permissions"] == ["admin.access", "admin.read_role", "admin.manage_permission"]
    bob = await grant(db, models, "bob", ["admin.access", "admin.read_role"])
    body = (await site.test_client(db, user={"key": bob}).get(LIST)).json()
    assert [r["name"] for r in body["resources"]] == ["Role"]
    assert (await site.test_client(db, user={"key": 999}).get(LIST)).status == 403


async def test_pattern_grants_seed_roles_and_a_second_run_changes_nothing(world):
    site, db, models = world
    seeded = admin.Admin(site._registry, roles={"support": ["admin.*", "-admin.manage_*"], "reader": ["admin.read_role"]})
    seeded.register(models["Role"])
    seeded.register(models["Permission"])
    await seeded.sync_permissions(db)
    first = await seeded.seed_roles(db)
    assert (len(first.roles_created), first.grants) == (2, 4)
    assert (await seeded.seed_roles(db)).grants == 0 and (await seeded.seed_roles(db)).roles_created == ()
    role = models["Role"]
    support = await role.objects.using(db).filter(role.name == "support").first()
    rows = await models["RolePermission"].objects.using(db).filter(models["RolePermission"].role_id == support.id)
    assert len(rows) == 3
    assert await role.objects.using(db).count() == 2


async def test_a_pattern_that_matches_nothing_and_a_missing_sync_are_errors(world):
    site, db, models = world
    typo = admin.Admin(site._registry, roles={"support": ["orders.*"]})
    typo.register(models["Role"])
    with pytest.raises(admin.AdminConfigError, match="matches no permission name"):
        await typo.seed_roles(db)
    names = ["admin.access", "admin.read_role"]
    assert resolve_pattern_grants(["admin.*", "-admin.acce??"], names, "r") == ["admin.read_role"]
    with pytest.raises(ValueError, match="matches no permission name"):
        resolve_pattern_grants(["admin.READ_*"], names, "r")
    with pytest.raises(ValueError, match=r"has \[ or \]"):
        resolve_pattern_grants(["admin.[ar]*"], names, "r")
    unsynced = admin.Admin(site._registry, roles={"support": ["admin.read_role"]})
    unsynced.register(models["Role"])
    with pytest.raises(admin.AdminConfigError, match="sync-permissions"):
        await unsynced.seed_roles(db)
