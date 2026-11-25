"""Admin writes through the bridge: host behavior, the protected opt-in, services, ``ctx``, inputs and ``admin.log``, on PostgreSQL and SQLite."""

import datetime
import decimal
import os
import uuid

import pytest

import monorm
from monorm import admin, param
from monorm.adapters import register_field_adapter
from monorm.errors import ValidationError, VersionConflict

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SCHEMA = """datasource db {
  provider = "%(provider)s"
}
import "app:monorm-admin" (prefix: "admin_")

model SNote {
  id    Int    @id @default(autoincrement())
  owner Int    @default(1)
  title String @db.VarChar(20)
  slug  String @default("")
  @@map("s_notes")
}

model SLocked {
  id Int @id @default(autoincrement())
  x  Int
  @@write_rules.protect(layer: app)
  @@map("s_locked")
}

model SCode {
  code String @id
  note String @default("")
  @@map("s_codes")
}

model SBig {
  id  Int    @id @default(autoincrement())
  big BigInt @default(0)
  @@map("s_bigs")
}
"""

NOTES = "/admin/api/resources/SNote"
LOCKED = "/admin/api/resources/SLocked"
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
    await models["SNote"].objects.using(db).insert_many([{"owner": 1, "title": "First"}, {"owner": 2, "title": "Theirs"}])
    with monorm.allow_writes(models["SLocked"]):
        await models["SLocked"].objects.using(db).insert_many([{"x": 1}])
    yield registry, db, models
    await db.drop_tables()
    await db.close()


async def audit(db, models):
    entry = models["AdminLogEntry"]
    return [r for r in await entry.objects.using(db).order_by(entry.id)]


def site_of(registry, models, note=None, locked=None):
    site = admin.Admin(registry)
    site.register(models["SNote"], note or {})
    site.register(models["SLocked"], locked or {})
    return site


class Lower:
    def validate(self, value):
        return value.lower()

    async def prepare(self, value):
        return value

    def decode(self, stored):
        return stored


# -- the default writer ------------------------------------------------------------------------------------------------


async def test_a_query_set_write_with_the_queryset_and_the_can_rule_for_a_model_with_host_behavior(world):
    registry, db, models = world
    note = models["SNote"]
    register_field_adapter(note, "slug", Lower())
    config = {"queryset": note.objects.filter(note.owner == param("user")).prepare(), "can": {"change": note.title != "Locked"}}
    site = site_of(registry, models, config)
    client = site.test_client(db, user={"key": 1, "is_superuser": True})
    r = await client.patch(f"{NOTES}/1", json={"set": {"title": "Renamed", "slug": "AbC"}, "loaded": {"title": "First", "slug": ""}})
    assert r.status == 200, r.text
    assert (await note.objects.using(db).filter(note.id == 1).first()).title == "Renamed"
    assert (await note.objects.using(db).filter(note.id == 1).first()).slug == "abc"
    assert (await client.patch(f"{NOTES}/2", json={"set": {"title": "Mine"}, "loaded": {"title": "Theirs"}})).status == 404
    assert (await note.objects.using(db).filter(note.id == 2).first()).title == "Theirs"
    rows = await audit(db, models)
    assert [(r.action, r.record_key) for r in rows] == [("update", "1")]
    assert rows[0].changes["service"] is False


# -- the protected opt-in ----------------------------------------------------------------------------------------------


async def test_a_protected_model_stays_closed_until_the_resource_opts_in_and_the_audit_row_records_it(world):
    registry, db, models = world
    locked = models["SLocked"]
    closed = site_of(registry, models).test_client(db, user=SU)
    r = await closed.patch(f"{LOCKED}/1", json={"set": {"x": 2}, "loaded": {"x": 1}})
    assert (r.status, r.json()["error"]["kind"]) == (403, "ReadOnly")
    opened = site_of(registry, models, locked={"allow_protected_writes": True}).test_client(db, user=SU)
    r = await opened.patch(f"{LOCKED}/1", json={"set": {"x": 2}, "loaded": {"x": 1}})
    assert r.status == 200, r.text
    assert (await locked.objects.using(db).filter(locked.id == 1).first()).x == 2
    r = await opened.post(LOCKED, json={"set": {"x": 7}})
    assert r.status == 201, r.text
    rows = await audit(db, models)
    assert [(r.action, r.changes["allow_protected_writes"]) for r in rows] == [("update", True), ("create", True)]


# -- services ----------------------------------------------------------------------------------------------------------


def services(models):
    note = models["SNote"]
    seen = {}

    async def create(ctx, values):
        seen["create"] = (ctx.user["key"], isinstance(ctx.request_id, str), dict(ctx.inputs), ctx.loaded)
        async with ctx.database.transaction():
            made = await note.objects.using(ctx.database).insert(**{**values, "owner": ctx.user["key"]})
            await ctx.audit(made, changes={"reason": ctx.inputs.get("reason")})
        return made

    async def update(ctx, qs, changes):
        async with ctx.database.transaction():
            row = await qs.first()
            if row is None:
                raise monorm.DoesNotExist("gone")
            if row.title != ctx.loaded["title"]:
                raise VersionConflict("stale")
            if changes.get("title") == "bad":
                raise ValidationError({"title": "Not this title."}, non_field_messages=["Fix the form."])
            await qs.update(**changes)
            await ctx.audit(changes={"reason": ctx.inputs.get("reason")})

    async def delete(ctx, qs):
        async with ctx.database.transaction():
            await qs.delete()

    return {"create": create, "update": update, "delete": delete, "inputs": [admin.input("reason", type="textarea", required=True, label="Reason")]}, seen


async def test_async_services_create_update_and_delete_with_ctx_inputs_and_one_audit_row_each(world):
    registry, db, models = world
    note = models["SNote"]
    config, seen = services(models)
    client = site_of(registry, models, config).test_client(db, user={"key": 1, "is_superuser": True})
    r = await client.post(NOTES, json={"set": {"title": "Made"}, "inputs": {"reason": "because"}})
    assert r.status == 201, r.text
    assert r.json()["key"] == "3"
    assert seen["create"] == (1, True, {"reason": "because"}, {})
    assert (await note.objects.using(db).filter(note.id == 3).first()).owner == 1
    r = await client.patch(f"{NOTES}/3", json={"set": {"title": "Edited"}, "loaded": {"title": "Made"}, "inputs": {"reason": "typo"}})
    assert r.status == 200, r.text
    assert (await note.objects.using(db).filter(note.id == 3).first()).title == "Edited"
    r = await client.delete(f"{NOTES}/3", json={"loaded": {"title": "Edited"}, "inputs": {"reason": "gone"}})
    assert r.status == 200, r.text
    assert await note.objects.using(db).filter(note.id == 3).first() is None
    rows = await audit(db, models)
    assert [(r.action, r.record_key) for r in rows] == [("create", "3"), ("update", "3"), ("delete", "3")]
    assert rows[0].changes["reason"] == "because" and rows[0].changes["service"] is True
    assert rows[0].changes["inputs"] == {"reason": "because"}
    assert rows[1].changes["reason"] == "typo"


async def test_a_version_conflict_is_a_409_with_the_current_values_and_a_validation_error_is_a_422(world):
    registry, db, models = world
    note = models["SNote"]
    config, _ = services(models)
    client = site_of(registry, models, config).test_client(db, user=SU)
    r = await client.patch(f"{NOTES}/1", json={"set": {"title": "Mine"}, "loaded": {"title": "Old"}, "inputs": {"reason": "x"}})
    assert r.status == 409, r.text
    assert r.json()["current"]["title"] == "First"
    assert (await note.objects.using(db).filter(note.id == 1).first()).title == "First"
    r = await client.patch(f"{NOTES}/1", json={"set": {"title": "bad"}, "loaded": {"title": "First"}, "inputs": {"reason": "x"}})
    assert r.status == 422, r.text
    assert (r.json()["fields"]["title"], r.json()["form"]) == (["Not this title."], ["Fix the form."])
    assert await audit(db, models) == []


async def test_a_required_input_is_a_422_and_a_service_that_does_not_exist_for_the_row_is_a_404(world):
    registry, db, models = world
    config, _ = services(models)
    client = site_of(registry, models, config).test_client(db, user=SU)
    r = await client.post(NOTES, json={"set": {"title": "Made"}})
    assert r.status == 422, r.text
    assert "reason" in r.json()["fields"]
    assert (await client.patch(f"{NOTES}/99", json={"set": {"title": "x"}, "loaded": {"title": "y"}, "inputs": {"reason": "x"}})).status == 404


async def test_a_service_runs_with_the_queryset_filter_and_the_can_rule_on_its_query_set(world):
    registry, db, models = world
    note = models["SNote"]
    config, _ = services(models)
    config |= {"queryset": note.objects.filter(note.owner == param("user")).prepare(), "can": {"change": note.title != "First"}}
    client = site_of(registry, models, config).test_client(db, user=SU)
    body = {"set": {"title": "Z"}, "inputs": {"reason": "x"}}
    assert (await client.patch(f"{NOTES}/2", json={**body, "loaded": {"title": "Theirs"}})).status == 404
    assert (await client.patch(f"{NOTES}/1", json={**body, "loaded": {"title": "First"}})).status == 403


async def test_a_service_query_set_carries_the_scope_of_the_write(world):
    registry, db, models = world
    note = models["SNote"]
    seen = {}

    async def update(ctx, qs, changes):
        seen["before"] = await qs.count()
        await qs.update(**changes)
        seen["after"] = await qs.count()

    config = {"update": update, "queryset": note.objects.filter(note.owner == param("user")).prepare(), "can": {"change": note.title != "Zed"}}
    client = site_of(registry, models, config).test_client(db, user=SU)
    r = await client.patch(f"{NOTES}/1", json={"set": {"title": "Zed"}, "loaded": {"title": "First"}})
    assert r.status == 200, r.text
    assert seen == {"before": 1, "after": 0}
    assert (await note.objects.using(db).filter(note.id == 1).first()).title == "Zed"


async def test_a_threaded_sync_service_gets_plain_values_and_runs_database_calls_through_ctx_run(world):
    registry, db, models = world
    note = models["SNote"]

    @admin.threaded
    def update(ctx, qs, changes):
        row = ctx.run(qs.first())
        ctx.run(qs.update(title=changes["title"].upper() + row.title))

    client = site_of(registry, models, {"update": update}).test_client(db, user=SU)
    r = await client.patch(f"{NOTES}/1", json={"set": {"title": "ab"}, "loaded": {"title": "First"}})
    assert r.status == 200, r.text
    assert (await note.objects.using(db).filter(note.id == 1).first()).title == "ABFirst"


async def test_a_threaded_service_on_an_opted_in_protected_model_writes_like_an_async_one(world):
    registry, db, models = world
    locked = models["SLocked"]

    @admin.threaded
    def create(ctx, values):
        return ctx.run(locked.objects.using(ctx.database).insert(**values))

    client = site_of(registry, models, locked={"create": create, "allow_protected_writes": True}).test_client(db, user=SU)
    r = await client.post(LOCKED, json={"set": {"x": 9}})
    assert r.status == 201, r.text
    assert sorted(row.x for row in await locked.objects.using(db).all()) == [1, 9]


@pytest.mark.filterwarnings("ignore:.*never awaited")
async def test_a_threaded_service_that_returns_an_async_call_is_a_500_that_names_ctx_run(world):
    registry, db, models = world

    @admin.threaded
    def update(ctx, qs, changes):
        return qs.update(**changes)

    client = site_of(registry, models, {"update": update}).test_client(db, user=SU)
    r = await client.patch(f"{NOTES}/1", json={"set": {"title": "ab"}, "loaded": {"title": "First"}})
    assert r.status == 500, r.text
    assert (await models["SNote"].objects.using(db).filter(models["SNote"].id == 1).first()).title == "First"


async def test_a_service_that_fails_after_ctx_audit_leaves_no_row_and_no_audit_row(world):
    registry, db, models = world
    note = models["SNote"]

    async def create(ctx, values):
        async with ctx.database.transaction():
            made = await note.objects.using(ctx.database).insert(**values)
            await ctx.audit(made)
            raise RuntimeError("after the audit")

    client = site_of(registry, models, {"create": create}).test_client(db, user=SU)
    r = await client.post(NOTES, json={"set": {"title": "Made"}})
    assert r.status == 500, r.text
    assert await note.objects.using(db).count() == 2
    assert await audit(db, models) == []


async def test_a_secret_input_is_masked_in_the_audit_row_and_the_service_row_gets_a_label(world):
    registry, db, models = world
    note = models["SNote"]
    seen = {}

    async def create(ctx, values):
        seen["pw"] = ctx.inputs["pw"]
        async with ctx.database.transaction():
            made = await note.objects.using(ctx.database).insert(**values)
            await ctx.audit(made)
        return made

    config = {"create": create, "inputs": [admin.input("pw", secret=True), admin.input("reason")]}
    client = site_of(registry, models, config).test_client(db, user=SU)
    r = await client.post(NOTES, json={"set": {"title": "Made"}, "inputs": {"pw": "hunter2", "reason": "why"}})
    assert r.status == 201, r.text
    assert seen["pw"] == "hunter2"
    (row,) = await audit(db, models)
    assert row.changes["inputs"] == {"pw": "***", "reason": "why"}
    assert "hunter2" not in str(row.changes)
    assert row.record_label == "Made"


async def test_a_create_with_the_default_writer_needs_the_required_fields(world):
    registry, db, models = world
    note = models["SNote"]
    register_field_adapter(note, "slug", Lower())
    client = site_of(registry, models).test_client(db, user=SU)
    r = await client.post(NOTES, json={"set": {"slug": "A"}})
    assert r.status == 422, r.text
    assert r.json()["fields"]["title"] == ["This field is required."]
    assert await note.objects.using(db).count() == 2


async def test_a_plain_sync_service_and_an_input_without_a_service_are_site_build_errors(world):
    registry, _, models = world

    def update(ctx, qs, changes):
        return None

    with pytest.raises(admin.AdminConfigError, match="admin.threaded"):
        site_of(registry, models, {"update": update}).check()
    with pytest.raises(admin.AdminConfigError, match="service"):
        site_of(registry, models, {"inputs": [admin.input("reason")]}).check()
    with pytest.raises(admin.AdminConfigError, match="select"):
        site_of(registry, models, {"update": services(models)[0]["update"], "inputs": [admin.input("kind", type="select")]}).check()
    site_of(registry, models, {"update": services(models)[0]["update"]}).check()


async def test_a_service_is_a_plain_function_that_a_unit_test_calls_with_no_admin(world):
    _, db, models = world
    config, _ = services(models)
    ctx = admin.Context(user={"key": 7}, request_id="r1", inputs={"reason": "unit"}, database=db)
    made = await config["create"](ctx, {"title": "Unit"})  # no admin: the context is built by hand
    assert (made.owner, made.title) == (7, "Unit")
    rows = await audit(db, models)
    assert [(r.user_key, r.action, r.resource, r.record_key) for r in rows] == [("7", "service", "SNote", str(made.id))]
    assert rows[0].changes["request_id"] == "r1"


async def test_admin_log_lets_host_code_write_an_audit_row_in_the_open_transaction(world):
    _, db, models = world
    await admin.log("export", user={"key": 5}, resource="SNote", record_key=3, label="Q3", changes={"rows": 10}, using=db)
    async with db.transaction():
        await admin.log("purge", user=9, using=db)
    rows = await audit(db, models)
    assert [(r.user_key, r.action, r.resource, r.record_key, r.record_label) for r in rows] == [("5", "export", "SNote", "3", "Q3"), ("9", "purge", "", "", "")]
    assert rows[0].changes == {"rows": 10}
    # A value that is not JSON is stored as its text (the model JSON field refuses it).
    await admin.log("odd", user=1, changes={"at": datetime.datetime(2026, 1, 1, 1, 2, 3), "price": decimal.Decimal("1.50"), "id": uuid.UUID(int=5)}, using=db)
    assert (await audit(db, models))[-1].changes == {"at": "2026-01-01 01:02:03", "price": "1.50", "id": "00000000-0000-0000-0000-000000000005"}


def test_a_service_object_with_an_async_call_is_not_sync_and_the_log_reads_one_key_rule_of_a_user():
    from monorm._admin_write import _actor_key, is_sync

    class Call:
        async def __call__(self, ctx, values):
            return values

    async def plain(ctx, values):
        return values

    assert (is_sync(Call()), is_sync(plain), is_sync(lambda ctx, values: values)) == (False, False, True)

    class Obj:
        id = 5

    class Keyed(Obj):
        key = 7

    assert [_actor_key(u) for u in ({"key": 1}, {"id": 2}, Obj(), Keyed(), 9, "x", None)] == [1, 2, 5, 7, 9, "x", None]


async def test_a_service_audit_on_a_key_that_needs_escaping_gets_the_rust_key_text_and_the_label(world):
    registry, db, models = world
    code = models["SCode"]

    async def create(ctx, values):
        async with ctx.database.transaction():
            made = await code.objects.using(ctx.database).insert(**values)
            await ctx.audit(made)
        return made

    async def update(ctx, qs, changes):
        async with ctx.database.transaction():
            await qs.update(**changes)
            await ctx.audit()

    site = admin.Admin(registry)
    site.register(code, {"create": create, "update": update})
    client = site.test_client(db, user=SU)
    for key in ("a b", "a,b"):
        r = await client.post("/admin/api/resources/SCode", json={"set": {"code": key, "note": "n"}})
        assert r.status == 201, r.text
        r = await client.patch(f"/admin/api/resources/SCode/{r.json()['key']}", json={"set": {"note": "m"}, "loaded": {"note": "n"}})
        assert r.status == 200, r.text
    rows = await audit(db, models)
    assert [(a.action, a.record_key) for a in rows] == [("create", "a%20b"), ("update", "a%20b"), ("create", "a%2Cb"), ("update", "a%2Cb")]
    assert all(a.record_label for a in rows)


async def test_a_bigint_value_sent_as_a_string_reaches_the_service_as_an_int(world):
    registry, db, models = world
    big = models["SBig"]
    seen = {}

    async def create(ctx, values):
        seen["big"] = values["big"]
        return await big.objects.using(ctx.database).insert(**values)

    site = admin.Admin(registry)
    site.register(big, {"create": create})
    client = site.test_client(db, user=SU)
    r = await client.post("/admin/api/resources/SBig", json={"set": {"big": "9007199254740993"}})
    assert r.status == 201, r.text
    assert seen["big"] == 9007199254740993


async def test_a_route_audit_gets_the_rust_key_text_of_the_model_of_its_record(world):
    registry, db, models = world
    code = models["SCode"]

    async def stamp(ctx, request):
        made = await code.objects.using(ctx.database).insert(code=request.body["code"])
        child = await models["SNote"].objects.using(ctx.database).insert(title="child")
        await ctx.audit(made)
        await ctx.audit(child)
        await ctx.audit()
        return {"ok": True}

    site = admin.Admin(registry)
    site.register(code, {})
    site.route("POST", "stamp", stamp)
    client = site.test_client(db, user=SU)
    for key in ("a b", "a,b"):
        r = await client.post("/admin/api/routes/stamp", json={"code": key})
        assert r.status == 200, r.text
    rows = await audit(db, models)
    children = [str(n.id) for n in await models["SNote"].objects.using(db).filter(models["SNote"].title == "child").order_by(models["SNote"].id)]
    assert [(a.action, a.resource, a.record_key) for a in rows] == [
        ("service", "SCode", "a%20b"), ("service", "SNote", children[0]), ("service", "", ""),
        ("service", "SCode", "a%2Cb"), ("service", "SNote", children[1]), ("service", "", ""),
    ]


async def test_a_bigint_value_that_is_not_a_number_is_a_422_that_names_the_field(world):
    registry, db, models = world
    big = models["SBig"]
    calls = []

    async def create(ctx, values):
        calls.append(values)
        return await big.objects.using(ctx.database).insert(**values)

    site = admin.Admin(registry)
    site.register(big, {"create": create})
    r = await site.test_client(db, user=SU).post("/admin/api/resources/SBig", json={"set": {"big": "abc"}})
    assert r.status == 422, r.text
    assert r.json()["fields"]["big"] == ["Must be a whole number"]
    assert calls == []


async def test_ctx_audit_of_a_record_of_another_model_writes_the_key_of_that_model(world):
    registry, db, models = world
    code = models["SCode"]

    async def create(ctx, values):
        async with ctx.database.transaction():
            made = await models["SNote"].objects.using(ctx.database).insert(**values)
            child = await code.objects.using(ctx.database).insert(code="c-1")
            await ctx.audit(child)
        return made

    client = site_of(registry, models, {"create": create}).test_client(db, user=SU)
    r = await client.post(NOTES, json={"set": {"title": "Made"}})
    assert r.status == 201, r.text
    (row,) = await audit(db, models)
    assert (row.resource, row.record_key) == ("SCode", "c-1")
