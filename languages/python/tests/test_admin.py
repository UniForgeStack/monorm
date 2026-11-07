"""The admin tracer: a superuser reads model lists through `admin.test_client`, on PostgreSQL and SQLite."""

import os
from datetime import datetime, timezone
from decimal import Decimal

import pytest

import monorm
from monorm import _native, admin

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SOURCE = """
datasource db {
  provider = "%(provider)s"
}

model AdmAuthor {
  id    BigInt   @id @default(autoincrement())
  name  String
  posts AdmPost[]
  @@map("adm_authors")
}

model AdmPost {
  id         BigInt    @id @default(autoincrement())
  title      String
  views      BigInt
  price      %(decimal)s
  rank       Int
  created_at DateTime
  author_id  BigInt?
  author     AdmAuthor? @relation(fields: [author_id], references: [id])
  @@map("adm_posts")
}
"""

SU = {"is_superuser": True}


def source(provider):
    return SOURCE % {"provider": provider, "decimal": "Decimal" if provider == "postgres" else "Float"}


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request):
    registry = monorm.Registry()
    classes = monorm.loads(source(request.param), registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else PG_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    price = Decimal if request.param == "postgres" else float
    Author, Post = classes["AdmAuthor"], classes["AdmPost"]
    ada, bo = await Author.objects.using(db).insert_many([{"name": "Ada"}, {"name": "Bo"}]).returning()
    at = datetime(2026, 1, 2, 3, 4, 5, tzinfo=timezone.utc)
    await Post.objects.using(db).insert_many(
        [
            {"title": "one", "views": 9007199254740993, "price": price("1.50"), "rank": 1, "created_at": at, "author_id": ada.id},
            {"title": "two", "views": 2, "price": price("2.25"), "rank": 1, "created_at": at, "author_id": None},
            {"title": "three", "views": 3, "price": price("3"), "rank": 2, "created_at": at, "author_id": bo.id},
        ]
    )
    yield admin.Admin(registry), db, Author, Post
    await db.drop_tables()
    await db.close()


async def test_a_to_one_column_is_one_statement_and_keeps_the_row_without_an_author(world):
    site, db, Author, Post = world
    site.register(Post, {"list": {"columns": [Post.title, Post.author.name], "order": [Post.title]}})
    page = await site.test_client(db, user=SU).get("/admin/api/resources/AdmPost")
    assert page.status == 200
    assert page.queries == 1
    body = page.json()
    assert [c["label"] for c in body["columns"]][:2] == ["title", "author.name"]
    assert [row[:2] for row in body["rows"]] == [["one", "Ada"], ["three", "Bo"], ["two", None]]
    assert body["next"] is None


async def test_keyset_pages_with_the_primary_key_as_tie_breaker(world):
    site, db, Author, Post = world
    site.register(Post, {"list": {"columns": [Post.rank, Post.title], "order": [Post.rank.desc()], "page_size": 2}})
    client = site.test_client(db, user=SU)
    first = (await client.get("/admin/api/resources/AdmPost")).json()
    assert len(first["rows"]) == 2
    assert first["next"]
    second = (await client.get(f"/admin/api/resources/AdmPost?cursor={first['next']}")).json()
    seen = [r[1] for r in first["rows"] + second["rows"]]
    assert sorted(seen) == ["one", "three", "two"]
    assert second["next"] is None


async def test_bigint_and_decimal_are_strings_and_dates_carry_an_offset(world):
    site, db, Author, Post = world
    site.register(Post, {"list": {"columns": [Post.views, Post.price, Post.created_at], "order": [Post.views.desc()]}})
    body = (await site.test_client(db, user=SU).get("/admin/api/resources/AdmPost")).json()
    first = body["rows"][0]
    assert first[0] == "9007199254740993"
    assert float(first[1]) == 1.5
    assert isinstance(first[1], str) == (db.url.startswith("postgres"))
    assert datetime.fromisoformat(first[2]) == datetime(2026, 1, 2, 3, 4, 5, tzinfo=timezone.utc)
    assert first[2].endswith("+00:00")


async def test_with_no_config_every_concrete_model_is_a_read_only_resource(world):
    site, db, Author, Post = world
    client = site.test_client(db, user=SU)
    listing = (await client.get("/admin/api/resources")).json()
    assert sorted(r["name"] for r in listing["resources"]) == ["AdmAuthor", "AdmPost"]
    assert all(r["permissions"] == ["read"] for r in listing["resources"])
    assert (await client.get("/admin/api/resources/AdmAuthor")).status == 200


async def test_the_prefix_is_a_config_value(world):
    site, db, Author, Post = world
    site.prefix = "/back/office"
    client = site.test_client(db, user=SU)
    assert (await client.get("/back/office/resources/AdmAuthor")).status == 200
    assert (await client.get("/admin/api/resources/AdmAuthor")).status == 404


async def test_a_request_needs_a_superuser(world):
    site, db, Author, Post = world
    assert (await site.test_client(db).get("/admin/api/resources")).status == 401
    assert (await site.test_client(db, user={"is_superuser": False}).get("/admin/api/resources")).status == 403
    assert (await site.test_client(db, user=SU).get("/admin/api/resources/Nope")).status == 404
    assert (await site.test_client(db, user=SU).get("/admin/api/resources/AdmPost?cursor=zz")).status == 400


async def test_check_raises_one_precise_error_for_a_list_column_through_a_to_many_relation(world):
    site, db, Author, Post = world
    site.register(Author, {"list": {"columns": [Author.posts.title]}})
    with pytest.raises(admin.AdminConfigError, match="AdmAuthor") as raised:
        site.check()
    with pytest.raises(admin.AdminConfigError) as from_client:
        site.test_client(db, user=SU)
    assert str(raised.value) == str(from_client.value)


def test_the_native_site_rejects_an_unknown_field():
    registry = monorm.Registry()
    monorm.loads(source("sqlite"), registry=registry)
    config = '{"resources":[{"model":"AdmPost","list":{"columns":["nope"]}}]}'
    with pytest.raises(ValueError, match="nope"):
        _native.admin_site(registry.prepare(), config)


def test_a_column_of_another_model_is_refused():
    registry = monorm.Registry()
    classes = monorm.loads(source("sqlite"), registry=registry)
    site = admin.Admin(registry)
    with pytest.raises(admin.AdminConfigError, match="not to AdmPost"):
        site.register(classes["AdmPost"], {"list": {"columns": [classes["AdmAuthor"].name]}})
        site.check()


def test_a_model_registers_once():
    registry = monorm.Registry()
    classes = monorm.loads(source("sqlite"), registry=registry)
    site = admin.Admin(registry)
    site.register(classes["AdmPost"])
    with pytest.raises(admin.AdminConfigError, match="already registered"):
        site.register(classes["AdmPost"])
