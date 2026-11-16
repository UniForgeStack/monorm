"""Admin actions: the placements, the query set, applies_to, the second confirmation, bulk delete, the results and the grouped audit,
through the bridge on PostgreSQL and SQLite."""

import base64
import os

import pytest

import monorm
from monorm import admin
from monorm.errors import ValidationError

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SCHEMA = """datasource db {
  provider = "%(provider)s"
}
import "app:monorm-admin" (prefix: "admin_")

model AShop {
  id    Int     @id @default(autoincrement())
  name  String  @db.VarChar(20)
  items AItem[]
  @@map("a_shops")
}

model AItem {
  id      Int      @id @default(autoincrement())
  shop_id Int
  name    String   @db.VarChar(20)
  status  String   @default("new") @db.VarChar(10)
  qty     Int      @default(0)
  created DateTime @default(now())
  shop    AShop    @relation(fields: [shop_id], references: [id], onDelete: Cascade)
  @@map("a_items")
}

model ABig {
  id  BigInt @id
  tag String @db.VarChar(10)
  @@map("a_bigs")
}

model AVault {
  id   Int    @id @default(autoincrement())
  note String @db.VarChar(10)
  @@protected_write
  @@map("a_vaults")
}
"""

ITEMS = "/admin/api/actions/AItem"
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
    await models["AShop"].objects.using(db).insert_many([{"name": "one"}, {"name": "two"}])
    await models["AItem"].objects.using(db).insert_many(
        [
            {"shop_id": 1, "name": "a", "qty": 1},
            {"shop_id": 1, "name": "b", "qty": 2},
            {"shop_id": 1, "name": "c", "status": "locked", "qty": 3},
            {"shop_id": 2, "name": "d", "qty": 4},
            {"shop_id": 2, "name": "e", "status": "done", "qty": 5},
        ]
    )
    yield registry, db, models
    await db.drop_tables()
    await db.close()


async def names(db, models, *where):
    item = models["AItem"]
    qs = item.objects.using(db).filter(*where) if where else item.objects.using(db)
    return [r.name for r in await qs.order_by(item.id)]


async def qtys(db, models):
    item = models["AItem"]
    return [(r.name, r.qty) for r in await item.objects.using(db).order_by(item.id)]


async def audit(db, models):
    entry = models["AdminLogEntry"]
    return [(r.action, r.resource, r.record_key, r.changes) for r in await entry.objects.using(db).order_by(entry.id)]


def site_of(registry, models, *actions, item=None):
    site = admin.Admin(registry)
    site.register(models["AShop"])
    site.register(
        models["AItem"],
        {"list": {"columns": [models["AItem"].name], "filters": [models["AItem"].status, models["AItem"].created]}, "actions": list(actions), **(item or {})},
    )
    return site


async def post(client, action, body, path=ITEMS):
    return await client.post(f"{path}/{action}", json=body)


# -- the query set and the placements ----------------------------------------------------------------------------------


async def test_a_bulk_action_gets_the_selected_rows_with_the_can_rule_and_the_result_is_a_message(world):
    registry, db, models = world
    item = models["AItem"]
    seen = []

    @admin.action(placement="bulk", confirm="Double these?")
    async def double(ctx, qs):
        seen.append((ctx.count, ctx.capped, ctx.selection, sorted(r.name for r in await qs)))
        await qs.update(qty=item.qty * 2)
        return admin.message(f"Doubled {ctx.count}", "success")

    site = site_of(registry, models, double, item={"can": {"change": item.status != "locked"}})
    client = site.test_client(db, user=SU)
    r = await post(client, "double", {"keys": ["1", "3"], "inputs": {}})
    assert r.status == 200, r.text
    body = r.json()
    assert body["result"] == {"kind": "message", "message": "Doubled 1", "level": "success"}
    assert seen == [(1, False, "keys", ["a"])]
    assert await qtys(db, models) == [("a", 2), ("b", 2), ("c", 3), ("d", 4), ("e", 5)]
    rows = await audit(db, models)
    assert [(a, res, key) for a, res, key, _ in rows] == [("action:double", "AItem", "")]
    summary = rows[0][3]
    assert (summary["placement"], summary["count"], summary["selection"], summary["keys"]) == ("bulk", 1, "keys", ["1", "3"])
    meta = (await client.get("/admin/api/resources")).json()
    actions = next(r for r in meta["resources"] if r["name"] == "AItem")["actions"]
    assert actions == [{"name": "double", "label": "Double", "placement": "bulk", "confirm": "Double these?", "inputs": [], "applies": False}]


async def test_all_rows_that_match_the_list_filters_run_in_batches_with_a_date_filter_and_a_search_free_query_set(world):
    registry, db, models = world
    item = models["AItem"]
    await item.objects.using(db).insert_many([{"shop_id": 1, "name": f"m{i}", "qty": 0} for i in range(1200)])
    seen = []

    @admin.action(placement="bulk")
    async def bump(ctx, qs):
        batches = 0
        async for batch in qs.batches(500):
            batches += 1
            await item.objects.using(ctx.database).filter(item.id.in_([r.id for r in batch])).update(qty=1)
        seen.append((ctx.count, ctx.selection, batches))

    client = site_of(registry, models, bump).test_client(db, user=SU)
    r = await client.post(f"{ITEMS}/bump?f.status=new&f.created:gte=2020-01-01", json={"all": True, "inputs": {}})
    assert r.status == 200, r.text
    assert seen == [(1203, "all", 3)]
    assert (await item.objects.using(db).filter(item.qty == 1).count()) == 1203
    assert await item.objects.using(db).filter(item.status != "new").count() == 2


async def test_an_action_on_all_rows_with_no_filter_may_update_them_with_qs_update(world):
    registry, db, models = world
    item = models["AItem"]

    @admin.action(placement="bulk")
    async def zero(ctx, qs):
        await qs.update(qty=0)

    client = site_of(registry, models, zero).test_client(db, user=SU)
    r = await client.post(f"{ITEMS}/zero", json={"all": True, "inputs": {}})
    assert r.status == 200, r.text
    assert await item.objects.using(db).filter(item.qty == 0).count() == 5


async def test_row_detail_submit_and_list_placements_each_run_on_the_rows_they_name(world):
    registry, db, models = world
    seen = []

    def make(placement):
        @admin.action(name=placement, placement=placement)
        async def run(ctx, qs):
            seen.append((placement, ctx.selection, ctx.count, sorted(r.name for r in await qs)))

        return run

    client = site_of(registry, models, *[make(p) for p in ("row", "detail", "submit", "list")]).test_client(db, user=SU)
    for placement in ("row", "detail", "submit"):
        r = await post(client, placement, {"keys": ["2"], "inputs": {}})
        assert r.status == 200 and r.json()["result"] == {"kind": "refresh"}, r.text
    assert (await post(client, "row", {"keys": ["1", "2"], "inputs": {}})).status == 400
    assert (await post(client, "list", {"keys": ["1"], "inputs": {}})).status == 400
    r = await client.post(f"{ITEMS}/list?f.status=new", json={"inputs": {}})
    assert r.status == 200, r.text
    assert seen == [("row", "keys", 1, ["b"]), ("detail", "keys", 1, ["b"]), ("submit", "keys", 1, ["b"]), ("list", "filter", 3, ["a", "b", "d"])]


async def test_applies_to_flags_the_rows_and_the_query_set_leaves_out_the_rows_it_does_not_cover(world):
    registry, db, models = world
    item = models["AItem"]
    seen = []

    @admin.action(placement="row", applies_to=item.status == "new")
    async def ship(ctx, qs):
        seen.append(sorted(r.name for r in await qs))

    client = site_of(registry, models, ship).test_client(db, user=SU)
    page = (await client.get("/admin/api/resources/AItem")).json()
    assert page["rows"] and "applies:ship" in str(page), page
    assert (await post(client, "ship", {"keys": ["1"], "inputs": {}})).status == 200
    assert (await post(client, "ship", {"keys": ["3"], "inputs": {}})).status == 404
    assert seen == [["a"]]


async def test_above_ten_thousand_rows_the_action_needs_a_second_confirmation(world):
    registry, db, models = world
    item = models["AItem"]
    await item.objects.using(db).insert_many([{"shop_id": 1, "name": "x"} for _ in range(10_000)])
    seen = []

    @admin.action(placement="bulk")
    async def touch(ctx, qs):
        seen.append((ctx.count, ctx.capped))

    client = site_of(registry, models, touch).test_client(db, user=SU)
    r = await post(client, "touch", {"all": True, "inputs": {}})
    assert r.status == 409 and r.json()["error"]["kind"] == "ConfirmLarge" and r.json()["capped"] is True, r.text
    assert seen == []
    r = await post(client, "touch", {"all": True, "inputs": {}, "confirm_large": True})
    assert r.status == 200, r.text
    assert seen == [(10_000, True)]


# -- inputs and results -------------------------------------------------------------------------------------------------


async def test_inputs_are_checked_and_a_file_input_arrives_as_a_file_and_is_masked_in_the_audit(world):
    registry, db, models = world
    seen = []

    @admin.action(
        placement="bulk",
        inputs=[admin.input("reason", required=True), admin.input("token", secret=True), admin.input("sheet", type="file")],
    )
    async def load(ctx, qs):
        sheet = ctx.inputs["sheet"]
        with open(sheet["path"], "rb") as f:
            seen.append((ctx.inputs["reason"], sheet["filename"], sheet["content_type"], f.read()))

    client = site_of(registry, models, load).test_client(db, user=SU)
    assert (await post(client, "load", {"keys": ["1"], "inputs": {"token": "t"}})).status == 422
    sheet = {"name": "rows.csv", "content_type": "text/csv", "base64": base64.b64encode(b"a,b\n").decode()}
    r = await post(client, "load", {"keys": ["1"], "inputs": {"reason": "why", "token": "s3cret", "sheet": sheet}})
    assert r.status == 200, r.text
    assert seen == [("why", "rows.csv", "text/csv", b"a,b\n")]
    inputs = (await audit(db, models))[0][3]["inputs"]
    assert inputs["token"] == "***" and inputs["sheet"] == {"name": "rows.csv", "content_type": "text/csv", "size": 4}


async def test_a_result_is_a_message_a_redirect_a_download_or_a_refresh_and_a_bad_one_is_a_500(world):
    registry, db, models = world

    @admin.action(placement="detail", inputs=[admin.input("kind")])
    async def show(ctx, qs):
        kind = ctx.inputs.get("kind")
        return {
            "message": admin.message("done", "warning"),
            "redirect": admin.redirect("/resources/AShop"),
            "download": admin.download("out.txt", "hello", "text/plain"),
            "text": "plain text",
            "bad": 42,
        }.get(kind)

    client = site_of(registry, models, show).test_client(db, user=SU)

    async def run(kind):
        return await post(client, "show", {"keys": ["1"], "inputs": {"kind": kind} if kind else {}})

    assert (await run("message")).json()["result"] == {"kind": "message", "message": "done", "level": "warning"}
    assert (await run("redirect")).json()["result"] == {"kind": "redirect", "to": "/resources/AShop"}
    assert (await run("download")).json()["result"] == {"kind": "download", "filename": "out.txt", "content_type": "text/plain", "base64": base64.b64encode(b"hello").decode()}
    assert (await run("text")).json()["result"] == {"kind": "message", "message": "plain text", "level": "info"}
    assert (await run(None)).json()["result"] == {"kind": "refresh"}
    assert (await run("bad")).status == 500


async def test_validation_errors_and_missing_rows_of_an_action_map_to_the_status_of_a_write(world):
    registry, db, models = world

    @admin.action(placement="row", inputs=[admin.input("mode")])
    async def fail(ctx, qs):
        if ctx.inputs.get("mode") == "invalid":
            raise ValidationError({"qty": ["too big"]}, non_field_messages=["not now"])
        if ctx.inputs.get("mode") == "missing":
            raise monorm.DoesNotExist("gone")
        raise monorm.VersionConflict("stale")

    client = site_of(registry, models, fail).test_client(db, user=SU)
    r = await post(client, "fail", {"keys": ["1"], "inputs": {"mode": "invalid"}})
    assert r.status == 422 and r.json()["fields"] == {"qty": ["too big"]}, r.text
    assert (await post(client, "fail", {"keys": ["1"], "inputs": {"mode": "missing"}})).status == 404
    assert (await post(client, "fail", {"keys": ["1"], "inputs": {}})).status == 409
    assert await audit(db, models) == []


# -- bulk delete ----------------------------------------------------------------------------------------------------------


async def test_bulk_delete_is_one_delete_of_the_query_set_and_bulk_delete_false_removes_it(world):
    registry, db, models = world
    item = models["AItem"]
    client = site_of(registry, models, item={"can": {"delete": item.status != "locked"}}).test_client(db, user=SU)
    r = await post(client, "bulk_delete", {"keys": ["1", "3", "4"], "inputs": {}})
    assert r.status == 200 and r.json()["affected"] == 2, r.text
    assert await names(db, models) == ["b", "c", "e"]
    r = await client.post(f"{ITEMS}/bulk_delete?f.status=done", json={"all": True, "inputs": {}})
    assert r.status == 200 and r.json()["affected"] == 1, r.text
    assert [(a, key) for a, _, key, _ in await audit(db, models)] == [("bulk_delete", ""), ("bulk_delete", "")]
    off = site_of(registry, models, item={"bulk_delete": False}).test_client(db, user=SU)
    assert (await post(off, "bulk_delete", {"keys": ["2"], "inputs": {}})).status == 404
    assert await names(db, models) == ["b", "c"]


async def test_bulk_delete_with_a_delete_service_gets_the_query_set_and_a_single_delete_keeps_working(world):
    registry, db, models = world
    seen = []

    async def delete(ctx, qs):
        seen.append((ctx.key, ctx.count, sorted(r.name for r in await qs)))
        await qs.delete()
        await ctx.audit(action="trash", changes={"note": "by service"})

    client = site_of(registry, models, item={"delete": delete}).test_client(db, user=SU)
    r = await post(client, "bulk_delete", {"keys": ["1", "2"], "inputs": {}})
    assert r.status == 200, r.text
    assert seen == [(None, 2, ["a", "b"])]
    assert await names(db, models) == ["c", "d", "e"]
    assert (await client.delete("/admin/api/resources/AItem/3")).status == 200
    assert seen[1][0] == {"id": 3}
    kinds = [a for a, *_ in await audit(db, models)]
    assert kinds == ["trash", "bulk_delete", "trash"]


# -- tabs, permissions, audit, config -----------------------------------------------------------------------------------


async def test_row_and_bulk_actions_of_the_child_run_on_a_tab_with_the_parent_pinned(world):
    registry, db, models = world
    item = models["AItem"]
    seen = []

    @admin.action(placement="bulk")
    async def double(ctx, qs):
        seen.append(sorted(r.name for r in await qs))
        await qs.update(qty=item.qty * 2)

    @admin.action(placement="detail")
    async def only_detail(ctx, qs):
        raise AssertionError("a tab runs bulk and row actions only")

    client = site_of(registry, models, double, only_detail).test_client(db, user=SU)
    tab = "/admin/api/tab-actions/AShop/1/items"
    r = await post(client, "double", {"keys": ["1", "4"], "inputs": {}}, tab)
    assert r.status == 200, r.text
    assert seen == [["a"]]
    assert (await post(client, "only_detail", {"keys": ["1"], "inputs": {}}, tab)).status == 404
    r = await post(client, "double", {"all": True, "inputs": {}}, tab)
    assert r.status == 200 and seen[1] == ["a", "b", "c"], r.text


async def test_an_action_has_a_permission_name_and_an_action_without_one_needs_manage(world):
    registry, db, models = world

    @admin.action(placement="bulk", permission="refund")
    async def refund(ctx, qs):
        pass

    @admin.action(placement="bulk")
    async def plain(ctx, qs):
        pass

    site = site_of(registry, models, refund, plain, "export")
    assert "admin.refund_aitem" in site.permissions and "admin.export_aitem" in site.permissions
    assert not any("plain" in p for p in site.permissions)
    stranger = site.test_client(db, user={"key": 99})
    assert (await post(stranger, "refund", {"keys": ["1"], "inputs": {}})).status == 403
    assert (await post(stranger, "plain", {"keys": ["1"], "inputs": {}})).status == 403
    assert (await post(site.test_client(db, user=SU), "export", {"keys": ["1"], "inputs": {}})).status == 404


async def test_the_rows_of_one_request_share_one_group_id_and_a_bulk_action_writes_a_summary_row(world):
    registry, db, models = world

    @admin.action(placement="bulk")
    async def audited(ctx, qs):
        for row in await qs:
            await ctx.audit(row, action="audited_row", changes={"name": row.name})

    client = site_of(registry, models, audited).test_client(db, user=SU)
    r = await post(client, "audited", {"keys": ["1", "2", "4"], "inputs": {}})
    assert r.status == 200, r.text
    rows = await audit(db, models)
    assert [a for a, *_ in rows] == ["audited_row", "audited_row", "audited_row", "action:audited"]
    ids = {changes["request_id"] for *_, changes in rows}
    assert len(ids) == 1
    assert rows[-1][3]["count"] == 3


async def test_a_bigint_key_and_a_date_filter_reach_the_query_set_with_their_types(world):
    registry, db, models = world
    big = models["ABig"]
    await big.objects.using(db).insert_many([{"id": 2**40, "tag": "a"}, {"id": 2**40 + 1, "tag": "b"}])
    seen = []

    @admin.action(placement="bulk")
    async def tag(ctx, qs):
        seen.append(sorted(r.id for r in await qs))

    site = admin.Admin(registry)
    site.register(big, {"actions": [tag]})
    client = site.test_client(db, user=SU)
    r = await client.post("/admin/api/actions/ABig/tag", json={"keys": [str(2**40 + 1)], "inputs": {}})
    assert r.status == 200, r.text
    assert seen == [[2**40 + 1]]


async def test_an_action_is_a_plain_function_that_a_unit_test_calls_with_no_admin(world):
    registry, db, models = world
    item = models["AItem"]

    @admin.action(placement="bulk", inputs=[admin.input("by", type="number")])
    async def add(ctx, qs):
        await qs.update(qty=item.qty + ctx.inputs["by"])
        return f"added {ctx.inputs['by']}"

    ctx = admin.Context(inputs={"by": 10}, database=db)
    assert await add(ctx, item.objects.using(db).filter(item.shop_id == 2)) == "added 10"
    assert await qtys(db, models) == [("a", 1), ("b", 2), ("c", 3), ("d", 14), ("e", 15)]


def test_a_wrong_action_config_is_an_admin_config_error(world):
    registry, db, models = world

    @admin.action(placement="middle")
    async def bad_place(ctx, qs):
        pass

    @admin.action(placement="bulk", confirm="x")
    async def twice(ctx, qs):
        pass

    cases = [
        ({"actions": [bad_place]}, "placement"),
        ({"actions": [twice, twice]}, "listed twice"),
        ({"actions": [7]}, "an action is a name"),
        ({"bulk_delete": "no"}, "bulk_delete"),
    ]
    for config, text in cases:
        site = admin.Admin(registry)
        site.register(models["AItem"], config)
        with pytest.raises(admin.AdminConfigError, match=text):
            site.check()


async def test_an_action_writes_a_protected_model_only_when_the_resource_opts_in(world):
    registry, db, models = world
    vault = models["AVault"]
    with monorm.allow_writes(vault):
        await vault.objects.using(db).insert_many([{"note": "open"}])

    @admin.action(placement="bulk")
    async def seal(ctx, qs):
        await qs.update(note="sealed")
        return admin.message("Sealed")

    async def run(opt_in):
        site = admin.Admin(registry)
        site.register(vault, {"allow_protected_writes": opt_in, "actions": [seal]})
        return await post(site.test_client(db, user=SU), "seal", {"keys": ["1"]}, "/admin/api/actions/AVault")

    refused = await run(False)
    assert refused.status != 200, refused.text
    assert [r.note for r in await vault.objects.using(db).all()] == ["open"]
    done = await run(True)
    assert done.status == 200, done.text
    assert [r.note for r in await vault.objects.using(db).all()] == ["sealed"]
