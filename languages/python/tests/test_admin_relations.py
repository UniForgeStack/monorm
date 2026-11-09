"""Admin relation fields and files: autocomplete, `choices`, many-to-many chips, upload and download, through `admin.test_client`, on PostgreSQL and SQLite."""

import os

import pytest

import monorm
from artifact_caps import has_extension
from monorm import admin
from monorm_auth import UserMethods, UserQueries, make_password

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
SECRET = "s" * 40
SU = {"key": 1, "is_superuser": True}
API = "/admin/api"

SCHEMA = """datasource db {
  provider = "%s"
}
import "app:monorm-auth" (prefix: "auth_")
import "app:monorm-admin" (prefix: "admin_")

model RAuthor {
  id     Int      @id @default(autoincrement())
  name   String
  active Boolean  @default(true)
  books  RBook[]
  @@map("r_authors")
}

model RTag {
  id    Int         @id @default(autoincrement())
  name  String
  books RBook[]     @relation(through: RBookTag)
  links RBookTag[]
  @@map("r_tags")
}

model RBook {
  id         Int         @id @default(autoincrement())
  author_id  Int
  title      String
  cover      Json?
  author     RAuthor     @relation(fields: [author_id], references: [id])
  tags       RTag[]      @relation(through: RBookTag)
  links      RBookTag[]
  @@map("r_books")
}

model RBookTag {
  id      Int   @id @default(autoincrement())
  book_id Int
  tag_id  Int
  book    RBook @relation(fields: [book_id], references: [id], onDelete: Cascade)
  tag     RTag  @relation(fields: [tag_id], references: [id], onDelete: Cascade)
  @@map("r_book_tags")
}
"""


class Media:
    """A storage registry stand-in: OpenDAL's memory service holds the bytes."""

    @staticmethod
    def make():
        from monorm_file_storage import Registry
        from opendal import AsyncOperator

        return Registry({"media": AsyncOperator("memory")})


class World:
    def __init__(self, registry, models, db):
        self.registry, self.models, self.db = registry, models, db

    def site(self, book=None, author=None, **options):
        site = admin.Admin(self.registry, **options)
        a, t, b = self.models["RAuthor"], self.models["RTag"], self.models["RBook"]
        site.register(a, {"list": {"columns": [a.name], "order": [a.id], "search": [a.name]}, **(author or {})})
        site.register(t, {"list": {"columns": [t.name], "order": [t.id], "search": [t.name]}})
        site.register(b, {"list": {"columns": [b.title], "order": [b.id]}, "relations": {"author": {}, "tags": {}}, **(book or {})})
        return site

    async def links(self, book):
        rows = await self.models["RBookTag"].objects.using(self.db).filter(self.models["RBookTag"].book_id == book).order_by(self.models["RBookTag"].tag_id)
        return [r.tag_id for r in rows]

    async def audit(self):
        return await self.models["AdminLogEntry"].objects.using(self.db).order_by(self.models["AdminLogEntry"].id)


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
    using = lambda name: models[name].objects.using(db)  # noqa: E731
    await using("User").insert_many([{"username": "ann", "email": "ann@example.com", "password": make_password("pw"), "is_superuser": True}])
    await using("RAuthor").insert_many([{"name": "Ada"}, {"name": "Bo", "active": False}, *({"name": f"Author {i:02}"} for i in range(3, 31))])
    await using("RTag").insert_many([{"name": n} for n in ("red", "green", "blue", "grey")])
    await using("RBook").insert_many([{"author_id": 1, "title": "First"}])
    await using("RBookTag").insert_many([{"book_id": 1, "tag_id": 1}, {"book_id": 1, "tag_id": 2}])
    yield World(registry, models, db)
    await db.drop_tables()
    await db.close()


def labels(reply):
    return [i["label"] for i in reply.json()["items"]]


async def test_the_autocomplete_pages_by_keyset_and_searches(world):
    client = world.site().test_client(world.db, user=SU)
    first = (await client.get(f"{API}/autocomplete/RBook/author?limit=10")).json()
    assert first["items"][0] == {"value": 1, "label": "Ada"} and len(first["items"]) == 10
    seen, cursor = [i["value"] for i in first["items"]], first["next"]
    while cursor:
        page = (await client.get(f"{API}/autocomplete/RBook/author?limit=10&cursor={cursor}")).json()
        seen += [i["value"] for i in page["items"]]
        cursor = page["next"]
    assert seen == list(range(1, 31)), "every row once, in key order"
    assert labels(await client.get(f"{API}/autocomplete/RBook/author?q=Author%2007")) == ["Author 07"]
    assert (await client.get(f"{API}/autocomplete/RBook/title")).status == 404, "a column is not a relation field"
    assert (await world.site().test_client(world.db).get(f"{API}/autocomplete/RBook/author")).status == 401


async def test_the_autocomplete_applies_the_queryset_of_the_target(world):
    a = world.models["RAuthor"]
    client = world.site(author={"queryset": a.objects.filter(a.active == True).prepare()}).test_client(world.db, user=SU)  # noqa: E712
    assert labels(await client.get(f"{API}/autocomplete/RBook/author?limit=3")) == ["Ada", "Author 03", "Author 04"]


@pytest.mark.parametrize(
    ("config", "match"),
    [
        ({"author": {"label": "id"}}, "not a text column"),
        ({"nope": {}}, "not a relation"),
        ({"links": {}}, "not a resource"),
        ({"author": {"colour": "red"}}, "unknown option"),
    ],
)
async def test_a_bad_relation_config_fails_the_build(world, config, match):
    with pytest.raises(admin.AdminConfigError, match=match):
        world.site(book={"relations": config}).check()


async def test_a_target_with_no_search_fails_the_build(world):
    site = admin.Admin(world.registry)
    a, b = world.models["RAuthor"], world.models["RBook"]
    site.register(a, {"list": {"columns": [a.name]}})
    site.register(b, {"list": {"columns": [b.title]}, "relations": {"author": {}}})
    with pytest.raises(admin.AdminConfigError, match="has no search"):
        site.check()


async def test_choices_limit_the_picker_and_a_write_outside_them_is_a_422(world):
    a = world.models["RAuthor"]
    book = {"relations": {"author": {"choices": a.objects.filter(a.active == True).prepare()}}}  # noqa: E712
    client = world.site(book=book).test_client(world.db, user=SU)
    assert labels(await client.get(f"{API}/autocomplete/RBook/author?limit=3")) == ["Ada", "Author 03", "Author 04"]
    bad = await client.post(f"{API}/resources/RBook", json={"set": {"author_id": 2, "title": "Bad"}})
    assert bad.status == 422 and "Choose" in bad.json()["fields"]["author_id"][0], bad.text
    assert (await client.post(f"{API}/resources/RBook", json={"set": {"author_id": 3, "title": "Good"}})).status == 201
    moved = await client.patch(f"{API}/resources/RBook/2", json={"set": {"author_id": 2}, "loaded": {"author_id": 3}})
    assert moved.status == 422, "a changed value is checked on update"
    same = await client.patch(f"{API}/resources/RBook/2", json={"set": {"title": "Renamed"}, "loaded": {"title": "Good"}})
    assert same.status == 200, "an unchanged value stays"


async def test_the_chips_add_and_remove_links_in_the_save_with_one_audit_row(world):
    client = world.site().test_client(world.db, user=SU)
    assert sorted(labels(await client.get(f"{API}/links/RBook/1/tags"))) == ["green", "red"]
    body = {"set": {"title": "Renamed"}, "loaded": {"title": "First"}, "links": {"tags": {"add": [3, 4], "remove": [1]}}}
    assert (await client.patch(f"{API}/resources/RBook/1", json=body)).status == 200
    assert await world.links(1) == [2, 3, 4]
    entries = await world.audit()
    assert len(entries) == 1, "one audit row for the field and the links"
    assert entries[0].changes["diff"]["links.tags"]["removed"] == [1]
    only = await client.patch(f"{API}/resources/RBook/1", json={"links": {"tags": {"add": [3], "remove": [2]}}})
    assert only.status == 200 and await world.links(1) == [3, 4]
    missing = await client.patch(f"{API}/resources/RBook/1", json={"links": {"tags": {"add": [99]}}})
    assert missing.status == 422 and await world.links(1) == [3, 4], "a failed save leaves the links"


async def test_a_create_takes_chips_in_the_same_transaction(world):
    client = world.site().test_client(world.db, user=SU)
    made = await client.post(f"{API}/resources/RBook", json={"set": {"author_id": 1, "title": "New"}, "links": {"tags": {"add": [1, 3]}}})
    assert made.status == 201 and await world.links(2) == [1, 3]
    bad = await client.post(f"{API}/resources/RBook", json={"set": {"author_id": 1, "title": "Bad"}, "links": {"tags": {"add": [99]}}})
    assert bad.status == 422
    assert await world.models["RBook"].objects.using(world.db).count() == 2, "the book of the failed save is not stored"
    assert len(await world.audit()) == 1


async def test_the_modal_creates_a_related_record_without_inlines(world):
    book = {"relations": {"author": {"create": True}, "tags": {}}}
    client = world.site(book=book).test_client(world.db, user=SU)
    meta = (await client.get(f"{API}/resources")).json()
    form = {f["name"]: f for f in next(r for r in meta["resources"] if r["name"] == "RBook")["form"]}
    assert form["author_id"]["relation"]["create"] is True
    assert (await client.post(f"{API}/resources/RAuthor", json={"set": {"name": "Cy"}})).status == 201


def storage_world(world):
    return world.site(book={"files": {"cover": "media"}}, storage=Media.make())


@pytest.mark.skipif(not has_extension("monorm.file-storage"), reason="requires file-storage")
async def test_an_upload_stores_the_file_and_a_download_sends_it_as_an_attachment(world):
    client = storage_world(world).test_client(world.db, user=SU)
    up = await client.post(f"{API}/files/RBook/1/cover", file=("dir/cover 1.png", b"\x89PNG data", "image/png"))
    assert up.status == 200, up.text
    value = up.json()["value"]
    assert (value["storage"], value["filename"], value["size"]) == ("media", "cover 1.png", 9)
    book = world.models["RBook"]
    stored = await book.objects.using(world.db).get(book.id == 1)
    assert stored.cover["key"] == value["key"]
    assert [e.action for e in await world.audit()] == ["update"]
    down = await client.get(f"{API}/files/RBook/1/cover")
    assert down.status == 200 and down.content == b"\x89PNG data"
    headers = {k.lower(): v for k, v in down.headers}
    assert headers["content-type"] == "image/png" and headers["content-disposition"].startswith("attachment;")
    assert "http" not in " ".join(headers.values()), "no storage URL reaches the browser"
    assert (await client.get(f"{API}/files/RBook/1/title")).status == 404, "a field that is not a file field"
    assert (await client.post(f"{API}/files/RBook/9/cover", file=("a.png", b"x", "image/png"))).status == 404


@pytest.mark.skipif(not has_extension("monorm.file-storage"), reason="requires file-storage")
async def test_a_plain_set_of_a_file_field_is_a_422_and_an_upload_over_the_cap_is_a_413(world):
    client = storage_world(world).test_client(world.db, user=SU)
    plain = await client.patch(f"{API}/resources/RBook/1", json={"set": {"cover": {"key": "x"}}, "loaded": {"cover": None}})
    assert plain.status == 422 and "upload" in plain.json()["fields"]["cover"][0]
    big = await client.post(f"{API}/files/RBook/1/cover", file=("big.bin", b"a" * (26 * 1024 * 1024), "application/octet-stream"))
    assert big.status == 413


@pytest.mark.skipif(not has_extension("monorm.file-storage"), reason="requires file-storage")
async def test_an_upload_needs_the_csrf_token_and_the_origin(world):
    client = storage_world(world).test_client(world.db, secret=SECRET)
    assert (await client.login("ann", "pw")).status == 200
    path, file = f"{API}/files/RBook/1/cover", ("a.txt", "text", "text/plain")
    assert (await client.post(path, file=file, csrf=None)).status == 403, "no CSRF token"
    assert (await client.post(path, file=file, origin="http://evil.example")).status == 403, "a foreign Origin"
    assert (await client.post(path, file=file)).status == 200
    assert (await client.get(path)).content == b"text"


@pytest.mark.skipif(not has_extension("monorm.file-storage"), reason="requires file-storage")
async def test_a_read_only_file_field_takes_no_upload(world):
    client = world.site(book={"files": {"cover": "media"}, "readonly": ["cover"]}, storage=Media.make()).test_client(world.db, user=SU)
    up = await client.post(f"{API}/files/RBook/1/cover", file=("a.png", b"x", "image/png"))
    assert up.status == 422 and "read-only" in up.json()["fields"]["cover"][0]
    book = world.models["RBook"]
    assert (await book.objects.using(world.db).get(book.id == 1)).cover is None
    assert await world.audit() == []
