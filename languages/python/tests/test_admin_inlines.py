"""Admin inlines: a to-many relation edited in place on the page of its parent, through the bridge, on PostgreSQL and SQLite."""

import os

import pytest

import monorm
from monorm import admin
from monorm.adapters import register_field_adapter
from monorm.errors import ValidationError

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SCHEMA = """datasource db {
  provider = "%(provider)s"
}
import "app:monorm-admin" (prefix: "admin_")

model IOrder {
  id     Int          @id @default(autoincrement())
  number String       @unique @db.VarChar(10)
  status String       @default("open") @db.VarChar(10)
  lines  IOrderLine[]
  notes  INote[]
  @@map("i_orders")
}

model IOrderLine {
  id       Int    @id @default(autoincrement())
  order_id Int
  sku      String @db.VarChar(10)
  qty      Int    @default(1)
  kind     String @default("goods") @db.VarChar(10)
  order    IOrder @relation(fields: [order_id], references: [id], onDelete: Cascade)
  @@map("i_order_lines")
}

model INote {
  id       Int    @id @default(autoincrement())
  order_id Int
  body     String @db.VarChar(40)
  order    IOrder @relation(fields: [order_id], references: [id], onDelete: Cascade)
  @@map("i_notes")
}

model IAuthor {
  id      Int     @id @default(autoincrement())
  name    String  @db.VarChar(20)
  written IBook[] @relation("written")
  edited  IBook[] @relation("edited")
  @@map("i_authors")
}

model IBook {
  id        Int      @id @default(autoincrement())
  author_id Int
  editor_id Int?
  title     String   @db.VarChar(20)
  author    IAuthor  @relation("written", fields: [author_id], references: [id])
  editor    IAuthor? @relation("edited", fields: [editor_id], references: [id])
  @@map("i_books")
}
"""

ORDER = "/admin/api/resources/IOrder"
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
    await models["IOrder"].objects.using(db).insert_many([{"number": "A1"}, {"number": "A2"}])
    await models["IOrderLine"].objects.using(db).insert_many(
        [{"order_id": 1, "sku": "S1", "qty": 1}, {"order_id": 1, "sku": "S2", "qty": 2}, {"order_id": 2, "sku": "X", "qty": 9}]
    )
    await models["INote"].objects.using(db).insert_many([{"order_id": 1, "body": "first"}, {"order_id": 2, "body": "other"}])
    yield registry, db, models
    await db.drop_tables()
    await db.close()


async def lines(db, models):
    line = models["IOrderLine"]
    return [(r.id, r.order_id, r.sku, r.qty) for r in await line.objects.using(db).order_by(line.id)]


async def audit(db, models):
    entry = models["AdminLogEntry"]
    return [r for r in await entry.objects.using(db).order_by(entry.id)]


def site_of(registry, models, order=None, line=None, note=None):
    site = admin.Admin(registry)
    site.register(models["IOrder"], order or {"inlines": [admin.inline(models["IOrderLine"]), admin.inline(models["INote"], save="row")]})
    site.register(models["IOrderLine"], line or {})
    site.register(models["INote"], note or {})
    return site


# -- the engine path ---------------------------------------------------------------------------------------------------


async def test_two_relations_to_the_parent_and_no_via_fail_admin_check_and_via_names_one(world):
    registry, _, models = world
    site = admin.Admin(registry)
    site.register(models["IAuthor"], {"inlines": [admin.inline(models["IBook"])]})
    site.register(models["IBook"])
    with pytest.raises(admin.AdminConfigError, match="ambiguous"):
        site.check()
    ok = admin.Admin(registry)
    ok.register(models["IAuthor"], {"inlines": [admin.inline(models["IBook"], via="written")]})
    ok.register(models["IBook"])
    ok.check()


async def test_with_parent_saves_creates_updates_and_deletes_with_the_parent_and_writes_one_audit_row_each(world):
    registry, db, models = world
    client = site_of(registry, models).test_client(db, user=SU)
    body = {
        "set": {"status": "paid"},
        "loaded": {"status": "open"},
        "inlines": {
            "lines": {
                "create": [{"set": {"sku": "N1", "qty": 5}}],
                "update": [{"key": "1", "set": {"qty": 10}, "loaded": {"qty": 1}}],
                "delete": ["2"],
            }
        },
    }
    r = await client.patch(f"{ORDER}/1", json=body)
    assert r.status == 200, r.text
    assert await lines(db, models) == [(1, 1, "S1", 10), (3, 2, "X", 9), (4, 1, "N1", 5)]
    assert (await models["IOrder"].objects.using(db).filter(models["IOrder"].id == 1).first()).status == "paid"
    rows = await audit(db, models)
    assert [(r.action, r.resource) for r in rows] == [("delete", "IOrderLine"), ("update", "IOrderLine"), ("create", "IOrderLine"), ("update", "IOrder")]
    assert rows[1].changes["inline"] == "IOrder.lines"


async def test_one_refused_row_refuses_the_whole_change_set_and_names_the_row(world):
    registry, db, models = world
    client = site_of(registry, models).test_client(db, user=SU)
    body = {"set": {"status": "paid"}, "loaded": {"status": "open"}, "inlines": {"lines": {"create": [{"set": {"sku": "ok"}}, {"set": {"qty": 2}}]}}}
    r = await client.patch(f"{ORDER}/1", json=body)
    assert r.status == 422, r.text
    assert any(key.startswith("lines.new.1") for key in r.json()["fields"]), r.text
    assert await lines(db, models) == [(1, 1, "S1", 1), (2, 1, "S2", 2), (3, 2, "X", 9)]
    assert (await models["IOrder"].objects.using(db).filter(models["IOrder"].id == 1).first()).status == "open"
    assert await audit(db, models) == []


async def test_row_mode_saves_each_row_alone_through_the_page_of_the_child_and_pins_it_to_the_parent(world):
    registry, db, models = world
    note = models["INote"]
    client = site_of(registry, models).test_client(db, user=SU)
    r = await client.post(f"{ORDER}/1/inlines/notes", json={"set": {"body": "second"}})
    assert r.status == 201, r.text
    made = await note.objects.using(db).filter(note.body == "second").first()
    assert made.order_id == 1
    r = await client.patch(f"{ORDER}/1/inlines/notes/{made.id}", json={"set": {"body": "edited"}, "loaded": {"body": "second"}})
    assert r.status == 200, r.text
    assert (await client.patch(f"{ORDER}/1/inlines/notes/2", json={"set": {"body": "x"}, "loaded": {"body": "other"}})).status == 404
    assert (await client.post(f"{ORDER}/1/inlines/lines", json={"set": {"sku": "q"}})).status == 404
    assert (await client.delete(f"{ORDER}/1/inlines/notes/{made.id}")).status == 200
    assert sorted(r.body for r in await note.objects.using(db).all()) == ["first", "other"]
    assert [(r.action, r.resource) for r in await audit(db, models)] == [("create", "INote"), ("update", "INote"), ("delete", "INote")]


async def test_the_row_cap_and_the_new_parent_rules_hold_on_the_server(world):
    registry, db, models = world
    order = {"inlines": [admin.inline(models["IOrderLine"], max=3)]}
    client = site_of(registry, models, order).test_client(db, user=SU)
    r = await client.patch(f"{ORDER}/1", json={"set": {}, "inlines": {"lines": {"create": [{"set": {"sku": "a"}}, {"set": {"sku": "b"}}]}}})
    assert r.status == 422, r.text
    r = await client.post(ORDER, json={"set": {"number": "C3"}, "inlines": {"lines": {"update": [{"key": "1", "set": {"qty": 1}}]}}})
    assert r.status == 400, r.text
    r = await client.post(ORDER, json={"set": {"number": "C3"}, "inlines": {"lines": {"create": [{"set": {"sku": "K", "qty": 2}}]}}})
    assert r.status == 201, r.text
    assert (await lines(db, models))[-1] == (4, 3, "K", 2)


# -- the page keys of a wrapper ----------------------------------------------------------------------------------------


async def test_a_wrapper_queryset_filters_the_rows_on_read_and_on_inline_writes(world):
    registry, db, models = world
    line = models["IOrderLine"]
    await line.objects.using(db).filter(line.id == 2).update(kind="fee")
    order = {"inlines": [admin.inline(line, page={"queryset": line.objects.filter(line.kind != "fee").prepare()})]}
    client = site_of(registry, models, order).test_client(db, user=SU)
    detail = (await client.get(f"{ORDER}/1")).json()
    assert [r["key"] for r in detail["inlines"]["lines"]["rows"]] == ["1"]
    r = await client.patch(f"{ORDER}/1", json={"set": {}, "inlines": {"lines": {"update": [{"key": "2", "set": {"qty": 5}, "loaded": {"qty": 2}}]}}})
    assert r.status == 404, r.text
    assert await lines(db, models) == [(1, 1, "S1", 1), (2, 1, "S2", 2), (3, 2, "X", 9)]


async def test_a_wrapper_overrides_a_page_key_and_a_child_without_a_page_takes_the_config_of_the_wrapper(world):
    registry, db, models = world
    line = models["IOrderLine"]
    order = {"inlines": [admin.inline(line, page={"readonly": ["qty"]})]}
    site = admin.Admin(registry)
    site.register(models["IOrder"], order)
    client = site.test_client(db, user=SU)
    form = {f["name"]: f["readonly"] for f in (await client.get("/admin/api/resources")).json()["resources"][0]["inlines"][0]["form"]}
    assert form == {"id": False, "sku": False, "qty": True, "kind": False}
    r = await client.patch(f"{ORDER}/1", json={"set": {}, "inlines": {"lines": {"update": [{"key": "1", "set": {"qty": 4}, "loaded": {"qty": 1}}]}}})
    assert r.status == 422, r.text
    assert await lines(db, models) == [(1, 1, "S1", 1), (2, 1, "S2", 2), (3, 2, "X", 9)]


def test_a_page_key_that_cannot_be_shared_and_a_wrong_wrapper_are_config_errors():
    for key in ("create", "inlines", "files", "title", "perms", "actions", "allow_protected_writes"):
        with pytest.raises(admin.AdminConfigError, match=key):
            admin.Admin(monorm.Registry())._inline_json("P", admin.inline(object, page={key: 1}), admin._Fns())  # type: ignore[arg-type]
    with pytest.raises(admin.AdminConfigError, match="save"):
        admin.inline(object, save="later")  # type: ignore[arg-type]
    with pytest.raises(admin.AdminConfigError, match="style"):
        admin.inline(object, style="grid")  # type: ignore[arg-type]


# -- permissions -------------------------------------------------------------------------------------------------------


async def test_an_own_permission_names_the_write_and_an_inline_row_links_to_its_page_when_the_user_can_view_it(world):
    registry, db, models = world
    order = {"inlines": [admin.inline(models["IOrderLine"], permission="edit_lines")]}
    site = site_of(registry, models, order)
    assert "admin.edit_lines" in site.permissions
    detail = (await site.test_client(db, user=SU).get(f"{ORDER}/1")).json()
    assert detail["inlines"]["lines"]["link"]
    assert detail["inlines"]["lines"]["rows"][0]["flags"] is not None


async def test_a_row_write_needs_the_parent_to_take_an_update_its_allow_and_its_change_rule(world):
    registry, db, models = world
    note, order = models["INote"], models["IOrder"]
    inlines = [admin.inline(note, save="row")]
    client = site_of(registry, models, {"allow": ["read"], "inlines": inlines}).test_client(db, user=SU)
    assert (await client.post(f"{ORDER}/1/inlines/notes", json={"set": {"body": "x"}})).status == 403
    assert (await client.patch(f"{ORDER}/1/inlines/notes/1", json={"set": {"body": "x"}, "loaded": {"body": "first"}})).status == 403
    assert (await client.delete(f"{ORDER}/1/inlines/notes/1")).status == 403
    assert sorted(r.body for r in await note.objects.using(db).all()) == ["first", "other"]
    meta = next(r for r in (await client.get("/admin/api/resources")).json()["resources"] if r["name"] == "IOrder")
    assert meta["inlines"][0]["ops"] == []
    await order.objects.using(db).filter(order.id == 1).update(status="shipped")
    locked = {"can": {"change": order.status != "shipped"}, "inlines": inlines}
    client = site_of(registry, models, locked).test_client(db, user=SU)
    assert (await client.post(f"{ORDER}/1/inlines/notes", json={"set": {"body": "x"}})).status == 403
    assert (await client.post(f"{ORDER}/2/inlines/notes", json={"set": {"body": "ok"}})).status == 201
    # Each refused write leaves one audit row: three for the read-only parent, one for the locked order.
    assert [r.action for r in await audit(db, models)] == ["permission_denied"] * 3 + ["permission_denied", "create"]


# -- host behavior and services ----------------------------------------------------------------------------------------


class Upper:
    def validate(self, value):
        return value.upper()

    async def prepare(self, value):
        return value

    def decode(self, stored):
        return stored


async def test_a_parent_with_host_behavior_needs_row_mode_and_a_child_with_host_behavior_saves_through_the_bridge(world):
    registry, db, models = world
    register_field_adapter(models["IOrder"], "status", Upper())
    with pytest.raises(admin.AdminConfigError, match="host behavior and no service"):
        site_of(registry, models).check()
    register_field_adapter(models["INote"], "body", Upper())
    order = {"inlines": [admin.inline(models["INote"], save="row")]}
    note = models["INote"]
    client = site_of(registry, models, order).test_client(db, user=SU)
    r = await client.post(f"{ORDER}/1/inlines/notes", json={"set": {"body": "loud"}})
    assert r.status == 201, r.text
    made = await note.objects.using(db).filter(note.order_id == 1, note.body == "LOUD").first()
    assert made is not None
    assert (await client.patch(f"{ORDER}/1/inlines/notes/{made.id}", json={"set": {"body": "quiet"}, "loaded": {"body": "LOUD"}})).status == 200
    assert (await note.objects.using(db).filter(note.id == made.id).first()).body == "QUIET"
    assert (await client.patch(f"{ORDER}/1/inlines/notes/2", json={"set": {"body": "x"}, "loaded": {"body": "other"}})).status == 404


async def test_a_host_child_row_write_keeps_the_registered_queryset_when_the_wrapper_queryset_is_wider(world):
    registry, db, models = world
    note = models["INote"]
    register_field_adapter(note, "body", Upper())
    await note.objects.using(db).insert_many([{"order_id": 1, "body": "HIDDEN"}])
    hidden = await note.objects.using(db).filter(note.body == "HIDDEN").first()
    wide = note.objects.filter(note.id > 0).prepare()
    order = {"inlines": [admin.inline(note, save="row", page={"queryset": wide})]}
    client = site_of(registry, models, order, note={"queryset": note.objects.filter(note.body != "HIDDEN").prepare()}).test_client(db, user=SU)
    r = await client.patch(f"{ORDER}/1/inlines/notes/{hidden.id}", json={"set": {"body": "seen"}, "loaded": {"body": "HIDDEN"}})
    assert r.status == 404, r.text
    assert (await client.delete(f"{ORDER}/1/inlines/notes/{hidden.id}")).status == 404
    assert (await note.objects.using(db).filter(note.id == hidden.id).first()).body == "HIDDEN"


async def test_a_host_child_row_write_keeps_the_registered_can_rule_when_the_wrapper_overrides_it(world):
    registry, db, models = world
    note = models["INote"]
    register_field_adapter(note, "body", Upper())
    await note.objects.using(db).insert_many([{"order_id": 1, "body": "LOCKED"}])
    locked = await note.objects.using(db).filter(note.body == "LOCKED").first()
    order = {"inlines": [admin.inline(note, save="row", page={"can": {"change": note.id > 0, "delete": note.id > 0}})]}
    client = site_of(registry, models, order, note={"can": {"change": note.body != "LOCKED", "delete": note.body != "LOCKED"}}).test_client(db, user=SU)
    r = await client.patch(f"{ORDER}/1/inlines/notes/{locked.id}", json={"set": {"body": "seen"}, "loaded": {"body": "LOCKED"}})
    assert r.status == 404, r.text
    assert (await client.delete(f"{ORDER}/1/inlines/notes/{locked.id}")).status == 404
    assert (await note.objects.using(db).filter(note.id == locked.id).first()).body == "LOCKED"


async def test_a_wrapper_computed_and_search_override_get_their_own_tagged_host_functions_and_leave_the_page_of_the_child_alone(world):
    registry, db, models = world
    line = models["IOrderLine"]
    calls: dict[str, int] = {"page": 0, "inline": 0, "page_search": 0, "inline_search": 0}

    def computed(name):
        async def function(keys):
            calls[name] += 1
            return [1 for _ in keys]

        return [{"name": "doubled", "type": "int", "nullable": False, "function": function}]

    def search(name):
        async def function(q, limit):
            calls[name] += 1
            return [1, 2, 3]

        return function

    page = {"list": {"columns": ["sku", "doubled"], "computed": computed("page"), "search_with": search("page_search")}}
    wrapper = {"list": {"columns": ["sku", "doubled"], "computed": computed("inline"), "search_with": search("inline_search")}}
    site = site_of(registry, models, {"inlines": [admin.inline(line, page=wrapper)]}, line=page)
    host = site._host()
    assert sorted(n for n in host if n.startswith("column.IOrderLine")) == ["column.IOrderLine.doubled", "column.IOrderLine@IOrder.IOrderLine.doubled"]
    assert sorted(n for n in host if n.startswith("search.IOrderLine")) == ["search.IOrderLine", "search.IOrderLine@IOrder.IOrderLine"]
    client = site.test_client(db, user=SU)
    assert (await client.get("/admin/api/resources/IOrderLine?q=S")).status == 200
    assert (calls["page"], calls["page_search"]) == (1, 1)
    assert (await client.get(f"{ORDER}/1")).status == 200
    assert (calls["inline"], calls["inline_search"]) == (0, 0)


async def test_a_service_of_the_parent_reads_the_checked_rows_from_ctx_inlines(world):
    registry, db, models = world
    seen = {}

    async def update(ctx, qs, changes):
        seen["inlines"] = {name: {"model": rows["model"].__name__, "create": rows["create"], "update": [(u["key"], u["set"]) for u in rows["update"]], "delete": [d["key"] for d in rows["delete"]]} for name, rows in ctx.inlines.items()}
        if any(c.get("sku") == "bad" for c in ctx.inlines["lines"]["create"]):
            raise ValidationError({"status": "No."})
        async with ctx.database.transaction():
            for created in ctx.inlines["lines"]["create"]:
                await models["IOrderLine"].objects.using(ctx.database).insert(order_id=1, **created)
            await ctx.audit(changes={"rows": len(ctx.inlines["lines"]["create"])})

    client = site_of(registry, models, {"update": update, "inlines": [admin.inline(models["IOrderLine"])]}).test_client(db, user=SU)
    body = {"set": {}, "inlines": {"lines": {"create": [{"set": {"sku": "N"}}], "update": [{"key": "1", "set": {"qty": 4}, "loaded": {"qty": 1}}], "delete": ["2"]}}}
    r = await client.patch(f"{ORDER}/1", json=body)
    assert r.status == 200, r.text
    assert seen["inlines"]["lines"] == {"model": "IOrderLine", "create": [{"sku": "N"}], "update": [({"id": 1}, {"qty": 4})], "delete": [{"id": 2}]}
    assert [row[2] for row in await lines(db, models)] == ["S1", "S2", "X", "N"]
    assert [(r.action, r.resource) for r in await audit(db, models)] == [("update", "IOrder")]
