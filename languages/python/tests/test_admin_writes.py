"""Admin plain writes: create, change and delete, the 409 and 422 answers, the read-only reason and the audit row, on PostgreSQL and SQLite."""

import asyncio
import datetime
import os

import pytest

import monorm
from artifact_caps import has_extension
from monorm import admin, param

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SCHEMA = """datasource db {
  provider = "%(provider)s"
}
import "app:monorm-admin" (prefix: "admin_")

enum WStatus {
  draft
  published
  %(storage)s
}

model WAuthor {
  id    Int     @id @default(autoincrement())
  email String  @unique
  name  String  @db.VarChar(10)
  books WBook[]
  @@map("w_authors")
}

model WBook {
  id        Int      @id @default(autoincrement())
  author_id Int
  title     String   @db.VarChar(20)
  pages     Int      @default(0) @check(raw("pages >= 0"))
  status    WStatus  @default(draft)
  meta      Json?
  author    WAuthor  @relation(fields: [author_id], references: [id], onDelete: Restrict)
  @@check(raw("title <> 'banned'"), name: "no_banned_title")
  @@map("w_books")
}

model WLocked {
  id Int @id @default(autoincrement())
  x  Int
  @@write_rules.protect(layer: app)
  @@map("w_locked")
}

model WCoded {
  code String @id
  note String @default("")
  @@write_rules.protect(layer: app)
  @@map("w_coded")
}

model WChore {
  code  String    @id
  every Duration?
  @@write_rules.protect(layer: app)
  @@map("w_chores")
}
%(money)s"""

# SQLite has no exact decimal, so the decimal model exists on PostgreSQL only.
MONEY = """
model WMoney {
  code  String  @id
  price Decimal @db.Decimal(10, 2)
  @@write_rules.protect(layer: app)
  @@map("w_money")
}
"""

BOOKS = "/admin/api/resources/WBook"
AUTHORS = "/admin/api/resources/WAuthor"
SU = {"key": 1, "is_superuser": True}


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request, tmp_path):
    (tmp_path / "schema.prisma").write_text(SCHEMA % {"provider": request.param, "storage": "@@storage(text)" if request.param == "sqlite" else "", "money": "" if request.param == "sqlite" else MONEY})
    registry = monorm.Registry()
    models = monorm.load(tmp_path / "schema.prisma", registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else PG_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    await models["WAuthor"].objects.using(db).insert_many([{"email": "a@x.io", "name": "Ada"}, {"email": "b@x.io", "name": "Bo"}])
    await models["WBook"].objects.using(db).insert_many([{"author_id": 1, "title": "First", "pages": 10}])
    site = admin.Admin(registry)
    site.register(models["WAuthor"])
    site.register(models["WBook"], {"check_messages": {"no_banned_title": "That title is not allowed."}})
    site.register(models["WLocked"])
    yield site, db, models
    await db.drop_tables()
    await db.close()


async def audit(db, models):
    entry = models["AdminLogEntry"]
    return [r for r in await entry.objects.using(db).order_by(entry.id)]


async def test_a_create_stores_the_row_and_writes_one_audit_row_with_the_diff(world):
    site, db, models = world
    client = site.test_client(db, user=SU)
    body = {"set": {"author_id": 2, "title": "Second", "status": "published", "meta": {"a": [1]}}}
    response = await client.post(BOOKS, json=body)
    assert response.status == 201, response.text
    assert response.json()["key"] == "2"
    book = await models["WBook"].objects.using(db).filter(models["WBook"].id == 2).first()
    assert (book.author_id, book.title, book.pages, str(book.status.value if hasattr(book.status, "value") else book.status), book.meta) == (2, "Second", 0, "published", {"a": [1]})
    rows = await audit(db, models)
    assert len(rows) == 1
    assert (rows[0].action, rows[0].resource, rows[0].record_key, rows[0].record_label) == ("create", "WBook", "2", "Second")
    changes = rows[0].changes
    assert changes["diff"]["title"] == {"from": None, "to": "Second"}
    assert all(isinstance(changes[k], str) for k in ("request_id", "ip", "user_agent"))


async def test_a_bad_create_is_a_422_with_one_message_per_field_and_stores_nothing(world):
    site, db, models = world
    client = site.test_client(db, user=SU)
    response = await client.post(BOOKS, json={"set": {"title": "x" * 21, "pages": 1.5, "status": "gone", "meta": 1, "nope": 1}})
    assert response.status == 422
    fields = response.json()["fields"]
    assert fields["title"] == ["Must have at most 20 characters"]
    assert fields["pages"] == ["Must be a whole number"]
    assert fields["status"] == ["Must be one of the listed values"]
    assert fields["author_id"] == ["This field is required."]
    assert fields["nope"] == ["This field cannot be written."]
    assert await models["WBook"].objects.using(db).count() == 1
    assert await audit(db, models) == []


async def test_database_rules_come_back_as_field_errors_and_a_named_check_as_a_form_message(world):
    site, db, models = world
    client = site.test_client(db, user=SU)
    r = await client.post(AUTHORS, json={"set": {"email": "a@x.io", "name": "Cy"}})
    assert (r.status, r.json()["fields"]["email"]) == (422, ["A record with this value already exists."])
    r = await client.post(BOOKS, json={"set": {"author_id": 99, "title": "T"}})
    assert (r.status, r.json()["fields"]["author_id"]) == (422, ["No such record."])
    r = await client.post(BOOKS, json={"set": {"author_id": 1, "title": "T", "pages": -1}})
    assert (r.status, r.json()["fields"]["pages"]) == (422, ["This value is not allowed."])
    r = await client.post(BOOKS, json={"set": {"author_id": 1, "title": "banned"}})
    assert (r.status, r.json()["form"]) == (422, ["That title is not allowed."])
    assert await audit(db, models) == []


async def test_an_update_sends_only_the_changed_fields_and_the_audit_row_holds_the_diff(world):
    site, db, models = world
    client = site.test_client(db, user=SU)
    r = await client.patch(f"{BOOKS}/1", json={"set": {"title": "Renamed", "pages": 10}, "loaded": {"title": "First", "pages": 10}})
    assert (r.status, r.json()["changed"]) == (200, ["title"]), r.text
    assert r.queries <= 8, r.sql
    book = await models["WBook"].objects.using(db).filter(models["WBook"].id == 1).first()
    assert book.title == "Renamed"
    assert (await audit(db, models))[0].changes["diff"] == {"title": {"from": "First", "to": "Renamed"}}
    again = await client.patch(f"{BOOKS}/1", json={"set": {"title": "Renamed"}, "loaded": {"title": "Renamed"}})
    assert (again.status, again.json()["changed"]) == (200, [])
    assert len(await audit(db, models)) == 1


async def test_a_stale_loaded_value_is_a_409_with_the_current_value_and_writes_nothing(world):
    site, db, models = world
    client = site.test_client(db, user=SU)
    r = await client.patch(f"{BOOKS}/1", json={"set": {"title": "Mine"}, "loaded": {"title": "Old"}})
    assert r.status == 409, r.text
    assert r.json()["conflicts"]["title"] == {"loaded": "Old", "current": "First"}
    book = await models["WBook"].objects.using(db).filter(models["WBook"].id == 1).first()
    assert book.title == "First"
    assert await audit(db, models) == []


async def opened(world):
    """A site whose protected model is open: its writes go through the host function `admin.write`."""
    base, db, models = world
    with monorm.allow_writes(models["WLocked"]):
        await models["WLocked"].objects.using(db).insert_many([{"x": 1}])
    site = admin.Admin(base._registry)
    site.register(models["WLocked"], {"allow_protected_writes": True})
    return site.test_client(db, user=SU), db, models


LOCKED = "/admin/api/resources/WLocked"


async def test_a_failed_host_write_leaves_no_audit_row_and_a_good_one_leaves_exactly_one(world, monkeypatch):
    from monorm import _admin_write

    client, db, models = await opened(world)
    write_audit = _admin_write._write_audit

    async def fails_after_the_audit_row(*args, **kwargs):
        await write_audit(*args, **kwargs)
        raise RuntimeError("the host fails after the audit row")

    monkeypatch.setattr(_admin_write, "_write_audit", fails_after_the_audit_row)
    r = await client.patch(f"{LOCKED}/1", json={"set": {"x": 2}, "loaded": {"x": 1}})
    assert r.status >= 500, r.text
    assert (await models["WLocked"].objects.using(db).first()).x == 1
    assert await audit(db, models) == []
    monkeypatch.setattr(_admin_write, "_write_audit", write_audit)
    r = await client.patch(f"{LOCKED}/1", json={"set": {"x": 3}, "loaded": {"x": 1}})
    assert r.status == 200, r.text
    rows = await audit(db, models)
    assert [(a.action, a.record_key, a.record_label) for a in rows] == [("update", "1", "1")]
    assert rows[0].changes["diff"] == {"x": {"from": 1, "to": 3}}


async def test_a_stale_loaded_value_is_a_409_with_the_default_writer_and_the_other_edit_stays(world):
    client, db, models = await opened(world)
    with monorm.allow_writes(models["WLocked"]):
        await models["WLocked"].objects.using(db).filter(models["WLocked"].id == 1).update(x=9)  # the other edit, after the form loaded x=1
    r = await client.patch(f"{LOCKED}/1", json={"set": {"x": 2}, "loaded": {"x": 1}})
    assert r.status == 409, r.text
    assert r.json()["conflicts"]["x"] == {"loaded": 1, "current": 9}
    assert (await models["WLocked"].objects.using(db).first()).x == 9, "the other edit stays"
    assert await audit(db, models) == []


CODED = "/admin/api/resources/WCoded"
MONEY_URL = "/admin/api/resources/WMoney"
CHORE_URL = "/admin/api/resources/WChore"


async def test_a_host_create_with_a_key_that_needs_escaping_writes_the_audit_row_under_the_rust_key_text(world):
    base, db, models = world
    site = admin.Admin(base._registry)
    site.register(models["WCoded"], {"allow_protected_writes": True})
    client = site.test_client(db, user=SU)
    for code in ("a b", "a,b", "é", "a/b", "a\\b", "plain"):
        r = await client.post(CODED, json={"set": {"code": code}})
        assert r.status == 201, (code, r.text)
    rows = await audit(db, models)
    keys = [a.record_key for a in rows]
    assert keys == ["a%20b", "a%2Cb", "%C3%A9", "a%2Fb", "a%5Cb", "plain"]
    assert all(a.record_label for a in rows)
    assert await models["WCoded"].objects.using(db).count() == 6


async def test_a_host_write_compares_a_decimal_by_value_not_by_text(world):
    base, db, models = world
    if "WMoney" not in models:
        pytest.skip("SQLite has no exact decimal")
    Money = models["WMoney"]
    with monorm.allow_writes(Money):
        await Money.objects.using(db).insert_many([{"code": "k", "price": "5.00"}])
    site = admin.Admin(base._registry)
    site.register(Money, {"allow_protected_writes": True})
    client = site.test_client(db, user=SU)
    for loaded in ("5", "5.0", "5.00"):
        r = await client.patch(f"{MONEY_URL}/k", json={"set": {"price": "7.50"}, "loaded": {"price": loaded}})
        assert r.status == 200, (loaded, r.text)
        with monorm.allow_writes(Money):
            await Money.objects.using(db).filter(Money.code == "k").update(price="5.00")
    r = await client.patch(f"{MONEY_URL}/k", json={"set": {"price": "7.50"}, "loaded": {"price": "5.01"}})
    assert (r.status, list(r.json()["conflicts"])) == (409, ["price"])


async def test_a_host_write_compares_a_duration_by_value_and_writes_it_as_text(world):
    base, db, models = world
    Chore = models["WChore"]
    td = datetime.timedelta
    with monorm.allow_writes(Chore):
        await Chore.objects.using(db).insert_many([{"code": "k", "every": td(hours=1, minutes=30)}])
    site = admin.Admin(base._registry)
    site.register(Chore, {"allow_protected_writes": True})
    client = site.test_client(db, user=SU)
    r = await client.patch(f"{CHORE_URL}/k", json={"set": {"every": "1 02:00:00"}, "loaded": {"every": "01:30:00"}})
    assert r.status == 200, r.text
    assert (await Chore.objects.using(db).filter(Chore.code == "k").first()).every == td(days=1, hours=2)
    r = await client.patch(f"{CHORE_URL}/k", json={"set": {"every": "00:00:05"}, "loaded": {"every": "01:30:00"}})
    assert (r.status, list(r.json()["conflicts"])) == (409, ["every"])


async def test_an_action_filter_can_bind_a_duration_column(world):
    from monorm._admin_write import _query_set

    base, db, models = world
    Chore = models["WChore"]
    with monorm.allow_writes(Chore):
        await Chore.objects.using(db).insert_many([{"code": "a", "every": datetime.timedelta(hours=1, minutes=30)}, {"code": "b", "every": datetime.timedelta(days=1)}])
    bound = {"filters": [{"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "every"}, "r": {"t": "param", "i": 0}}], "values": [{"$": "duration", "v": "5400000000"}]}
    assert [c.code for c in await _query_set(Chore, db, bound)] == ["a"]


async def test_a_bulk_delete_through_the_host_writer_deletes_the_query_set_and_writes_one_summary_row(world):
    client, db, models = await opened(world)
    with monorm.allow_writes(models["WLocked"]):
        await models["WLocked"].objects.using(db).insert_many([{"x": 2}, {"x": 3}])
    r = await client.post("/admin/api/actions/WLocked/bulk_delete", json={"keys": ["1", "2"], "inputs": {}})
    assert r.status == 200, r.text
    assert [row.x for row in await models["WLocked"].objects.using(db).order_by(models["WLocked"].id)] == [3]
    assert [a.action for a in await audit(db, models)] == ["bulk_delete"]


async def test_an_update_checks_the_request_before_it_writes(world):
    site, db, _ = world
    client = site.test_client(db, user=SU)
    assert (await client.patch(f"{BOOKS}/1", json={"set": {"title": "x"}})).status == 400
    r = await client.patch(f"{BOOKS}/1", json={"set": {"id": 5}, "loaded": {"id": 1}})
    assert (r.status, r.json()["fields"]["id"]) == (422, ["The key of a record cannot change."])
    assert (await client.patch(f"{BOOKS}/9", json={"set": {}})).status == 404
    assert (await client.patch("/admin/api/resources/Nope/1", json={"set": {}})).status == 404
    assert (await client.patch(f"{BOOKS}/1", json=[1])).status == 400


async def test_a_delete_removes_the_row_and_a_referenced_row_is_a_form_message(world):
    site, db, models = world
    client = site.test_client(db, user=SU)
    r = await client.delete(f"{AUTHORS}/1")
    assert r.status == 422, r.text
    assert "still refer" in r.json()["form"][0]
    assert await models["WAuthor"].objects.using(db).count() == 2
    r = await client.delete(f"{BOOKS}/1", json={"loaded": {"title": "Stale"}})
    assert (r.status, r.json()["conflicts"]["title"]["current"]) == (409, "First")
    assert (await client.delete(f"{BOOKS}/1", json={"loaded": {"title": "First"}})).status == 200
    assert (await client.delete(f"{BOOKS}/1")).status == 404
    rows = await audit(db, models)
    assert [(r.action, r.record_key, r.record_label) for r in rows] == [("delete", "1", "First")]


async def test_a_protected_model_is_read_only_with_the_reason_and_the_metadata_says_so(world):
    site, db, _ = world
    client = site.test_client(db, user=SU)
    r = await client.post("/admin/api/resources/WLocked", json={"set": {"x": 1}})
    assert (r.status, r.json()["error"]["kind"]) == (403, "ReadOnly")
    assert "write-protected" in r.json()["error"]["message"]
    resources = {m["name"]: m for m in (await client.get("/admin/api/resources")).json()["resources"]}
    assert resources["WLocked"]["writable"] is False
    assert resources["WBook"]["writable"] is True
    assert isinstance(resources["WLocked"]["read_only_reason"], str)


async def test_an_operation_that_the_resource_does_not_allow_is_a_403_with_an_audit_row(world):
    base, db, models = world
    site = admin.Admin(base._registry)
    site.register(models["WBook"], {"allow": ["read", "create"]})
    client = site.test_client(db, user=SU)
    assert (await client.patch(f"{BOOKS}/1", json={"set": {}})).status == 403
    assert (await client.delete(f"{BOOKS}/1")).status == 403
    assert [r.action for r in await audit(db, models)] == ["permission_denied", "permission_denied"]


async def test_a_write_stays_inside_the_queryset_and_the_can_rule(world):
    base, db, models = world
    Book = models["WBook"]
    await Book.objects.using(db).insert_many([{"id": 4, "author_id": 1, "title": "Four", "pages": 4}, {"id": 5, "author_id": 2, "title": "Five", "pages": 5}])
    site = admin.Admin(base._registry)
    site.register(
        Book,
        {
            "queryset": Book.objects.filter(Book.author_id == param("user")).prepare(),
            "can": {"change": Book.pages > 4, "delete": Book.pages > 3},
        },
    )
    client = site.test_client(db, user=SU)
    edit = lambda title, was: {"set": {"title": title}, "loaded": {"title": was}}  # noqa: E731
    assert (await client.patch(f"{BOOKS}/5", json=edit("x", "Five"))).status == 404, "outside the queryset"
    assert (await client.patch(f"{BOOKS}/4", json=edit("x", "Four"))).status == 403, "the can rule"
    assert (await client.delete(f"{BOOKS}/4")).status == 200


async def test_a_raw_json_value_counts_by_what_it_stores(world):
    site, db, models = world
    client = site.test_client(db, user=SU)
    edit = lambda to, was: client.patch(f"{BOOKS}/1", json={"set": {"pages": to}, "loaded": {"pages": was}})  # noqa: E731
    r = await edit("10", "10")
    assert (r.status, r.json()["changed"]) == (200, []), r.text
    assert await audit(db, models) == []
    r = await edit("11", "10")
    assert (r.status, r.json()["changed"]) == (200, ["pages"]), r.text
    assert (await audit(db, models))[0].changes["diff"] == {"pages": {"from": 10, "to": 11}}
    r = await edit(12, "10")
    assert r.status == 409, r.text
    assert r.json()["conflicts"]["pages"] == {"loaded": "10", "current": 11}


async def test_a_delete_ignores_a_loaded_key_that_is_not_a_field(world):
    site, db, _ = world
    client = site.test_client(db, user=SU)
    r = await client.delete(f"{AUTHORS}/2", json={"loaded": {"nope": 1}})
    assert r.status == 200, r.text


async def test_a_check_message_for_a_missing_check_fails_the_build(world):
    base, _, models = world
    site = admin.Admin(base._registry)
    site.register(models["WBook"], {"check_messages": {"no_such_check": "x"}})
    with pytest.raises(admin.AdminConfigError, match="no_such_check"):
        site.check()


GUARDED = """datasource db {
  provider = "%(provider)s"
}
import "app:monorm-admin" (prefix: "admin_")

model GBook {
  id    Int    @id @default(autoincrement())
  title String @db.VarChar(20)
  @@timestamps.updated_at("changed")
  @@locking.version
  @@map("g_books")
}
"""


@pytest.mark.skipif(not has_extension("monorm.locking", "monorm.timestamps"), reason="requires locking and timestamps")
@pytest.mark.parametrize("provider", ["sqlite", "postgres"])
async def test_an_update_of_a_locked_model_needs_its_version_and_bumps_it(provider, tmp_path):
    (tmp_path / "schema.prisma").write_text(GUARDED % {"provider": provider})
    registry = monorm.Registry()
    models = monorm.load(tmp_path / "schema.prisma", registry=registry)
    try:
        db = await monorm.connect("sqlite://:memory:" if provider == "sqlite" else PG_URL, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    try:
        await models["GBook"].objects.using(db).insert_many([{"title": "One"}])
        site = admin.Admin(registry)
        site.register(models["GBook"])
        client = site.test_client(db, user=SU)
        url = "/admin/api/resources/GBook/1"
        r = await client.patch(url, json={"set": {"title": "Two"}, "loaded": {"title": "One"}})
        assert r.status == 400 and 'needs the guard field "version"' in r.json()["error"]["message"], r.text
        r = await client.patch(url, json={"set": {"title": "Two"}, "loaded": {"title": "One", "version": 5}})
        assert r.status == 409, r.text
        assert r.json()["conflicts"]["version"] == {"loaded": 5, "current": 0}
        before = (await models["GBook"].objects.using(db).first()).changed
        await asyncio.sleep(0.02)
        r = await client.patch(url, json={"set": {"title": "Two"}, "loaded": {"title": "One", "version": 0}})
        assert (r.status, r.json()["changed"]) == (200, ["title"]), r.text
        row = await models["GBook"].objects.using(db).first()
        assert (row.title, row.version) == ("Two", 1)
        assert row.changed > before, "the admin update sets the updated_at field"
    finally:
        await db.drop_tables()
        await db.close()
