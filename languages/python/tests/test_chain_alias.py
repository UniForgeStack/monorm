"""One to-many path is one related row for the whole query; `alias()` is another, on PostgreSQL and SQLite."""

import pytest

import monorm
from conftest import DATABASE_URL

SCHEMA = """
model Author {
  id    BigInt @id @default(autoincrement())
  name  String
  posts Post[]
  @@map("ca_author")
}

model Post {
  id        BigInt  @id @default(autoincrement())
  author_id BigInt
  author    Author  @relation(fields: [author_id], references: [id])
  views     Int
  published Boolean
  notes     Note[]
  @@map("ca_post")
}

model Note {
  id      BigInt @id @default(autoincrement())
  post_id BigInt
  post    Post   @relation(fields: [post_id], references: [id])
  body    String
  @@map("ca_note")
}
"""


@pytest.fixture(params=["sqlite", "postgres"])
async def data(request):
    registry = monorm.Registry()
    source = SCHEMA if request.param == "postgres" else 'datasource db {\n  provider = "sqlite"\n}\n' + SCHEMA
    m = monorm.loads(source, registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else DATABASE_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    Author, Post, Note = m["Author"], m["Post"], m["Note"]
    a1, a2, a3 = await Author.objects.using(db).insert_many([{"name": "a1"}, {"name": "a2"}, {"name": "a3"}]).returning()
    # a1: a popular published post and an unpopular unpublished one; a2: a popular unpublished post; a3: none
    p1, p2, p3 = await Post.objects.using(db).insert_many(
        [
            {"author_id": a1.id, "views": 20, "published": True},
            {"author_id": a1.id, "views": 5, "published": False},
            {"author_id": a2.id, "views": 20, "published": False},
        ]
    ).returning()
    await Note.objects.using(db).insert_many(
        [{"post_id": p1.id, "body": "x"}, {"post_id": p2.id, "body": "y"}, {"post_id": p3.id, "body": "x"}, {"post_id": p3.id, "body": "y"}]
    ).returning()
    yield db, Author
    await db.drop_tables()
    await db.close()


async def names(db, qs):
    return {a.name for a in await qs.using(db)}


async def test_one_path_is_one_row_in_one_call_or_across_calls(data):
    db, Author = data
    popular_unpublished = {"a2"}
    assert await names(db, Author.objects.filter(Author.posts.views > 10, Author.posts.published == False)) == popular_unpublished  # noqa: E712
    assert await names(db, Author.objects.filter(Author.posts.views > 10).filter(Author.posts.published == False)) == popular_unpublished  # noqa: E712


async def test_alias_is_another_row(data):
    db, Author = data
    assert await names(db, Author.objects.filter(Author.posts.views > 10, Author.posts.alias().published == False)) == {"a1", "a2"}  # noqa: E712
    other = Author.posts.alias()
    assert await names(db, Author.objects.filter(other.views > 10).filter(other.published == False)) == {"a2"}  # noqa: E712


async def test_or_and_not_are_barriers(data):
    db, Author = data
    # a popular post, and no published post at all
    assert await names(db, Author.objects.filter(Author.posts.views > 10).exclude(Author.posts.published == True)) == {"a2"}  # noqa: E712
    # a popular post, and a post that is published or unpopular: two different rows may do
    either = (Author.posts.published == False) | (Author.posts.views < 10)  # noqa: E712
    assert await names(db, Author.objects.filter(Author.posts.views > 10, either)) == {"a1", "a2"}


async def test_the_shared_row_holds_inside_a_deeper_path(data):
    db, Author = data
    # both notes on one post, through one post and one note: a note cannot read both x and y
    assert await names(db, Author.objects.filter(Author.posts.notes.body == "x", Author.posts.notes.body == "y")) == set()
    # the same post, another note: only a2's post has both notes
    assert await names(db, Author.objects.filter(Author.posts.notes.body == "x", Author.posts.notes.alias().body == "y")) == {"a2"}
    # another post (an alias may match the same row): a1 has x on one post and y on the other, a2 has both on one
    assert await names(db, Author.objects.filter(Author.posts.notes.body == "x", Author.posts.alias().notes.body == "y")) == {"a1", "a2"}


NESTED = """
type Geo {
  city String
  zip  String?
}

model Shop {
  id     BigInt @id @default(autoincrement())
  geo    Geo?   @storage(json)
  @@map("ca_shop")
}

model Stall {
  id      BigInt @id @default(autoincrement())
  shop_id BigInt?
  shop    Shop?  @relation(fields: [shop_id], references: [id])
  @@map("ca_stall")
}
"""


def test_a_nested_column_behind_a_path_keeps_the_alias_the_rule_and_the_to_one_state():
    m = monorm.loads('datasource db {\n  provider = "sqlite"\n}\n' + NESTED, registry=monorm.Registry())
    Stall = m["Stall"]
    same = Stall.objects.filter(Stall.shop.geo.city == "a", Stall.shop.geo.zip == "b").sql()
    other = Stall.objects.filter(Stall.shop.geo.city == "a", Stall.shop.alias().geo.zip == "b").sql()
    assert same.count("EXISTS") == other.count("EXISTS") - 1
    ruled = Stall.objects.filter(Stall.shop.or_null().geo.zip != "b").sql()
    assert 'NOT EXISTS(SELECT 1 FROM "ca_shop"' in ruled


def test_the_alias_message_names_the_only_places_it_works():
    from blog.models import Post

    from monorm import outer

    with pytest.raises(monorm.QueryError, match=r"it works only in filter\(\) and exclude\(\)"):
        outer(Post.comments.alias().id)
