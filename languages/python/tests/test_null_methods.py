"""`.or_null()` / `.and_not_null()`: a NULL row kept or dropped, on PostgreSQL and SQLite."""

import pytest
from blog.models import Comment, Post, User

import orm
from conftest import DATABASE_URL
from orm import QueryError

SCHEMA = """
model Reading {
  id    BigInt  @id @default(autoincrement())
  score Int?
  name  String?
  tag   String
  @@map("nm_reading")
}

model Item {
  shop  BigInt
  id    BigInt
  lines Line[]
  @@id([shop, id])
  @@map("nm_item")
}

model Line {
  id      BigInt  @id @default(autoincrement())
  shop    BigInt?
  item_id BigInt?
  item    Item?   @relation(fields: [shop, item_id], references: [shop, id])
  @@map("nm_line")
}
"""


def where(qs) -> str:
    return qs.sql().split(" WHERE ", 1)[1]


def test_method_binds_before_not():
    c = ~(Comment.author_id > 20).or_null()
    assert where(Comment.objects.filter(c)) == 'NOT ("comments"."author_id" > 20 OR "comments"."author_id" IS NULL)'
    c = ~(Comment.author_id > 20).and_not_null()
    assert where(Comment.objects.filter(c)) == 'NOT ("comments"."author_id" > 20 AND "comments"."author_id" IS NOT NULL)'


def test_sql_of_each_form():
    col = Comment.author_id
    assert where(Comment.objects.filter((col == 1).or_null())) == (
        '"comments"."author_id" = 1 OR "comments"."author_id" IS NULL'
    )
    assert where(Comment.objects.filter(col.in_([1, 2]).and_not_null())) == (
        '"comments"."author_id" IN (1, 2) AND "comments"."author_id" IS NOT NULL'
    )
    assert where(Comment.objects.filter(col.between(1, 5).or_null())) == (
        '("comments"."author_id" >= 1 AND "comments"."author_id" <= 5) OR "comments"."author_id" IS NULL'
    )


def test_a_precedence_trap_raises_instead_of_guessing():
    with pytest.raises(TypeError, match="applies to one comparison"):
        (~(Comment.author_id > 20)).or_null()
    with pytest.raises(TypeError, match="applies to one comparison"):
        ((Comment.author_id > 1) | (Comment.author_id < 0)).or_null()
    with pytest.raises(TypeError, match="applies to one comparison"):
        Comment.author_id.is_null().and_not_null()


def test_a_not_null_value_raises():
    with pytest.raises(QueryError, match="NOT NULL"):
        (User.email == "a").or_null()
    with pytest.raises(QueryError, match="NOT NULL"):
        User.email.like("a%").and_not_null()


def test_a_column_reached_through_a_relation_raises_for_now():
    with pytest.raises(QueryError, match="relation"):
        (Post.comments.author_id == 1).or_null()


def test_a_computed_value_raises():
    with pytest.raises(TypeError, match="applies to one comparison"):
        (Comment.author_id + 1 > 5).or_null()


@pytest.fixture(params=["sqlite", "postgres"])
async def models(request):
    registry = orm.Registry()
    source = SCHEMA if request.param == "postgres" else 'datasource db {\n  provider = "sqlite"\n}\n' + SCHEMA
    m = orm.loads(source, registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else DATABASE_URL
    try:
        db = await orm.connect(url, registry=registry, default=False)
    except orm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    yield db, m["Reading"], m["Item"], m["Line"]
    await db.drop_tables()
    await db.close()


async def test_a_null_row_is_kept_or_dropped(models):
    db, Reading, _, _ = models
    a, b, null = await Reading.objects.using(db).insert_many(
        [
            {"score": 10, "name": "ab", "tag": "t"},
            {"score": 30, "name": "cd", "tag": "t"},
            {"score": None, "name": None, "tag": "t"},
        ]
    ).returning()
    s, n = Reading.score, Reading.name
    # (condition, the non-NULL rows it matches)
    cases = [
        (s == 10, {a}),
        (s != 10, {b}),
        (s < 20, {a}),
        (s > 20, {b}),
        (s.in_([10]), {a}),
        (s.not_in([10]), {b}),
        (s.between(5, 15), {a}),
        (n.like("a%"), {a}),
        (n.ilike("A%"), {a}),
        (n.contains("d"), {b}),
    ]
    both = {a, b}

    async def ids(condition):
        return {r.id for r in await Reading.objects.using(db).filter(condition)}

    for c, matched in cases:
        m = {r.id for r in matched}
        rest = {r.id for r in both - matched}
        nid = {null.id}
        label = repr(c)
        assert await ids(c) == m, label
        assert await ids(c.or_null()) == m | nid, label
        assert await ids(c.and_not_null()) == m, label
        assert await ids(~c) == rest, label
        assert await ids(~c.or_null()) == rest, label
        assert await ids(~c.and_not_null()) == rest | nid, label
    assert {r.id for r in await Reading.objects.using(db).exclude((s > 20).and_not_null())} == {a.id, null.id}
    assert {r.id for r in await Reading.objects.using(db).exclude((s > 20).or_null())} == {a.id}


async def test_a_composite_key_has_one_is_null_per_column(models):
    db, _, Item, Line = models
    a, _ = await Item.objects.using(db).insert_many([{"shop": 1, "id": 1}, {"shop": 2, "id": 1}]).returning()
    one, two, null, half = await Line.objects.using(db).insert_many(
        [
            {"item": a},
            {"shop": 2, "item_id": 1},
            {"shop": None, "item_id": None},
            {"shop": 1, "item_id": None},
        ]
    ).returning()

    async def ids(condition):
        return {r.id for r in await Line.objects.using(db).filter(condition)}

    eq = Line.item == a
    ne = Line.item != a
    assert where(Line.objects.filter(eq.or_null())).count(" IS NULL") == 2
    assert await ids(eq) == {one.id}
    assert await ids(eq.or_null()) == {one.id, null.id, half.id}
    assert await ids(eq.and_not_null()) == {one.id}
    assert await ids(ne) == {two.id}
    assert await ids(ne.or_null()) == {two.id, null.id, half.id}
    assert await ids(ne.and_not_null()) == {two.id}
    assert await ids(~eq.or_null()) == {two.id}
    assert await ids(~eq.and_not_null()) == {two.id, null.id, half.id}
