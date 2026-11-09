"""Admin row scope, `can=` rules, the bypass of a read default and a tenant read default that the admin ignores, on PostgreSQL and SQLite."""

import os
from types import SimpleNamespace

import pytest
from artifact_caps import has_extension

import monorm
from monorm import admin, param

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SOURCE = """
datasource db {
  provider = "%(provider)s"
}

model ScopeOwner {
  id    Int         @id
  name  String
  items ScopeItem[]
  @@map("scope_owners")
}

model ScopeItem {
  id       Int        @id
  owner_id Int
  title    String
  rank     Int
  owner    ScopeOwner @relation(fields: [owner_id], references: [id])
  %(extra)s
  @@map("scope_items")
}
"""

ITEMS = "/admin/api/resources/ScopeItem"
SU1 = {"key": 1, "is_superuser": True}
SU2 = {"key": 2, "is_superuser": True}


def titles(response):
    assert response.status == 200, response.json()
    return [row[0] for row in response.json()["rows"]]


async def build(provider, extra=""):
    registry = monorm.Registry()
    models = monorm.loads(SOURCE % {"provider": provider, "extra": extra}, registry=registry)
    url = "sqlite://:memory:" if provider == "sqlite" else PG_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    Owner, Item = models["ScopeOwner"], models["ScopeItem"]
    await Owner.objects.using(db).insert_many([{"id": 1, "name": "Ada"}, {"id": 2, "name": "Bo"}])
    await Item.objects.using(db).insert_many(
        [
            {"id": 1, "owner_id": 1, "title": "one", "rank": 1},
            {"id": 2, "owner_id": 1, "title": "two", "rank": 2},
            {"id": 3, "owner_id": 2, "title": "three", "rank": 3},
            {"id": 4, "owner_id": 1, "title": "four", "rank": 4},
        ]
    )
    return SimpleNamespace(provider=provider, registry=registry, db=db, Owner=Owner, Item=Item)


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request):
    w = await build(request.param)
    yield w
    await w.db.drop_tables()
    await w.db.close()


@pytest.fixture(params=["sqlite", "postgres"])
async def soft(request):
    if not has_extension("monorm.soft-delete"):
        pytest.skip("requires a selected soft-delete native artifact")
    w = await build(request.param, '@@soft_delete.deleted_at("deleted")')
    await w.Item.objects.using(w.db).filter(w.Item.id == 2).delete()
    yield w
    await w.db.drop_tables()
    await w.db.close()


@pytest.fixture(params=["sqlite", "postgres"])
async def tenanted(request):
    w = await build(request.param, '@@query.filter("tenant", "owner_id == scope.tenant")')
    yield w
    await w.db.drop_tables()
    await w.db.close()


def site_of(w, item=None, **options):
    site = admin.Admin(w.registry, **options)
    config = {"list": {"columns": [w.Item.title, w.Item.rank], "order": [w.Item.id], "search": [w.Item.title]}}
    config.update(item or {})
    site.register(w.Item, config)
    site.register(w.Owner, {"list": {"columns": [w.Owner.name], "order": [w.Owner.id]}})
    return site


def mine(w):
    return w.Item.objects.filter(w.Item.owner_id == param("user")).prepare()


async def test_the_queryset_filter_limits_the_list_the_count_the_search_and_the_detail(world):
    client = site_of(world, {"queryset": mine(world)}).test_client(world.db, user=SU1)
    assert titles(await client.get(ITEMS)) == ["one", "two", "four"]
    assert (await client.get("/admin/api/counts/ScopeItem?exact=1")).json()["count"] == 3
    assert titles(await client.get(f"{ITEMS}?q=t")) == ["two"]
    assert titles(await client.get(f"{ITEMS}?q=fo")) == ["four"]
    assert (await client.get(f"{ITEMS}/1")).status == 200
    assert (await client.get(f"{ITEMS}/3")).status == 404
    other = site_of(world, {"queryset": mine(world)}).test_client(world.db, user=SU2)
    assert titles(await other.get(ITEMS)) == ["three"]


async def test_the_queryset_order_is_the_order_of_the_list(world):
    queryset = world.Item.objects.filter(world.Item.owner_id == param("user")).order_by(-world.Item.id).prepare()
    config = {"queryset": queryset, "list": {"columns": [world.Item.title]}}
    client = site_of(world, config).test_client(world.db, user=SU1)
    assert titles(await client.get(ITEMS)) == ["four", "two", "one"]


async def test_a_parent_outside_the_queryset_has_no_tabs(world):
    site = admin.Admin(world.registry)
    own = world.Owner.objects.filter(world.Owner.id == param("user")).prepare()
    site.register(world.Owner, {"queryset": own, "list": {"columns": [world.Owner.name], "order": [world.Owner.id]}})
    site.register(world.Item, {"queryset": mine(world), "list": {"columns": [world.Item.title], "order": [world.Item.id]}})
    client = site.test_client(world.db, user=SU1)
    assert titles(await client.get("/admin/api/resources/ScopeOwner/1/items")) == ["one", "two", "four"]
    assert (await client.get("/admin/api/resources/ScopeOwner/2/items")).status == 404
    assert (await client.get("/admin/api/counts/ScopeOwner/2/items")).status == 404


async def test_can_rules_are_flags_of_every_row_in_the_list_and_the_detail(world):
    Item = world.Item
    client = site_of(world, {"can": {"change": Item.rank > 1, "delete": Item.rank > 3}}).test_client(world.db, user=SU1)
    body = (await client.get(ITEMS)).json()
    assert body["flags"] == [
        {"change": False, "delete": False},
        {"change": True, "delete": False},
        {"change": True, "delete": False},
        {"change": True, "delete": True},
    ]
    assert len(body["rows"][0]) == 3
    assert (await client.get(f"{ITEMS}/4")).json()["flags"] == {"change": True, "delete": True}
    meta = (await client.get("/admin/api/resources")).json()
    assert next(r for r in meta["resources"] if r["name"] == "ScopeItem")["can"] == ["change", "delete"]


async def test_a_callable_queryset_is_one_host_call_for_each_request(world):
    seen = []

    async def scope(user):
        seen.append(user)
        return world.Item.owner_id == 2

    client = site_of(world, {"queryset": scope}).test_client(world.db, user={"key": 5, "is_superuser": True})
    assert titles(await client.get(ITEMS)) == ["three"]
    assert seen == [5]


async def test_a_tenant_read_default_is_off_in_every_admin_read(tenanted):
    client = site_of(tenanted, {"queryset": mine(tenanted)}).test_client(tenanted.db, user=SU1)
    assert titles(await client.get(ITEMS)) == ["one", "two", "four"]
    assert (await client.get(f"{ITEMS}/2")).status == 200
    assert (await client.get("/admin/api/counts/ScopeItem?exact=1")).json()["count"] == 3
    assert titles(await client.get(f"{ITEMS}?q=t")) == ["two"]
    every = site_of(tenanted).test_client(tenanted.db, user=SU1)
    assert titles(await every.get(ITEMS)) == ["one", "two", "three", "four"]
    assert (await every.get(f"{ITEMS}?without=tenant")).status == 400
    meta = (await every.get("/admin/api/resources")).json()
    assert "tenants" not in meta
    assert "tenant" not in next(r for r in meta["resources"] if r["name"] == "ScopeItem")["bypass"]


async def test_a_bypass_permission_switches_a_read_default_off(soft):
    client = site_of(soft).test_client(soft.db, user=SU1)
    assert titles(await client.get(ITEMS)) == ["one", "three", "four"]
    assert (await client.get(f"{ITEMS}/2")).status == 404
    assert titles(await client.get(f"{ITEMS}?without=deleted")) == ["one", "two", "three", "four"]
    assert (await client.get(f"{ITEMS}/2?without=deleted")).status == 200
    assert (await client.get("/admin/api/counts/ScopeItem?without=deleted&exact=1")).json()["count"] == 4
    assert (await client.get(f"{ITEMS}?without=nope")).status == 400
    meta = (await client.get("/admin/api/resources")).json()
    assert next(r for r in meta["resources"] if r["name"] == "ScopeItem")["bypass"] == ["deleted"]
    plain = site_of(soft).test_client(soft.db, user={"key": 9})
    assert (await plain.get(f"{ITEMS}?without=deleted")).status in (401, 403)


async def test_a_bad_queryset_or_can_rule_is_a_config_error(world):
    Item = world.Item
    for bad in [
        {"queryset": Item.objects.prepare()},
        {"queryset": Item.objects.filter(Item.rank > 1).limit(2).prepare()},
        {"queryset": world.Owner.objects.filter(world.Owner.id > 0).prepare()},
        {"queryset": Item.objects.filter(Item.rank > param("rank")).prepare()},
        {"can": {"fly": Item.rank > 1}},
    ]:
        with pytest.raises(admin.AdminConfigError):
            site_of(world, bad).check()
