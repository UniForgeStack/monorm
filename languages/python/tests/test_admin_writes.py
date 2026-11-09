"""Admin plain writes: create, change and delete, the 409 and 422 answers, the read-only reason and the audit row, on PostgreSQL and SQLite."""

import asyncio
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
  @@protected_write
  @@map("w_locked")
}
"""

BOOKS = "/admin/api/resources/WBook"
AUTHORS = "/admin/api/resources/WAuthor"
SU = {"key": 1, "is_superuser": True}


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request, tmp_path):
    (tmp_path / "schema.prisma").write_text(SCHEMA % {"provider": request.param, "storage": "@@storage(text)" if request.param == "sqlite" else ""})
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
    assert "@@protected_write" in r.json()["error"]["message"]
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
