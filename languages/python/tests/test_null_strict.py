"""The strict null rule: a comparison on a nullable column under ``~`` / ``not_()`` / ``exclude()``,
and ``!=`` / ``not_in`` on one in any ``filter()``, need ``or_null()`` or ``and_not_null()``."""

import pytest
from blog.models import Comment, Post, User

from monorm import QueryError, not_

x = Comment.author_id  # nullable
ALICE = object.__new__(User)
ALICE.__dict__.update(id=1, email="a@b.c")


def where(qs) -> str:
    return qs.sql().split(" WHERE ", 1)[1]


@pytest.mark.parametrize(
    "build",
    [
        lambda: ~(x > 20),
        lambda: ~(x == 1),
        lambda: ~x.in_([1, 2]),
        lambda: ~x.between(1, 5),
        lambda: ~((x > 1) | (Comment.id > 2)),
        lambda: ~((Comment.id > 2) & (x > 1)),
        lambda: not_(x == 1),
        lambda: Comment.objects.exclude(x == 1),
        lambda: Comment.objects.exclude(Comment.id == 1, x == 1),
    ],
)
def test_a_negation_of_a_nullable_comparison_raises(build):
    with pytest.raises(QueryError, match=r"or_null\(\) drops NULL rows under negation, and_not_null\(\) keeps them"):
        build()


@pytest.mark.parametrize(
    "build",
    [
        lambda: Comment.objects.filter(x != 1),
        lambda: Comment.objects.filter(x.not_in([1, 2])),
        lambda: Comment.objects.filter(Comment.id == 1, (x != 1) | (Comment.id == 2)),
        lambda: Comment.objects.get(x != 1),
    ],
)
def test_ne_and_not_in_on_a_nullable_column_raise_in_a_positive_filter(build):
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows, and_not_null\(\) drops them"):
        build()


def test_a_having_condition_follows_the_same_rule():
    from monorm import func

    with pytest.raises(QueryError, match="or_null"):
        Comment.objects.select(x, func.count()).group_by(x).having(x != 1)


@pytest.mark.parametrize("method", ["or_null", "and_not_null"])
def test_the_method_on_the_comparison_is_accepted(method):
    def m(c):
        return getattr(c, method)()

    assert Comment.objects.filter(m(x != 1)).sql()
    assert Comment.objects.filter(m(x.not_in([1]))).sql()
    assert Comment.objects.filter(~m(x > 20)).sql()
    assert Comment.objects.exclude(m(x == 1)).sql()
    assert Comment.objects.exclude(m(x == 1), m(x.between(1, 5))).sql()
    assert Comment.objects.filter(~(m(x > 1) | m(x < 0))).sql()
    assert Comment.objects.filter(m(Comment.author != ALICE)).sql()
    assert Comment.objects.filter(~m(Comment.author == ALICE)).sql()


def test_a_method_on_one_comparison_does_not_cover_another_column():
    with pytest.raises(QueryError, match="or_null"):
        ~((x > 1).or_null() & (x < 9))


def test_the_sql_is_unchanged_by_the_check():
    assert where(Comment.objects.exclude((x > 20).or_null())) == 'NOT ("comments"."author_id" > 20 OR "comments"."author_id" IS NULL)'
    assert where(Comment.objects.filter((x != 1).and_not_null())) == '"comments"."author_id" <> 1 AND "comments"."author_id" IS NOT NULL'


def test_a_not_null_column_and_a_null_test_need_nothing():
    assert Comment.objects.filter(~(Comment.id == 1)).sql()
    assert User.objects.exclude(User.email == "a").sql()
    assert User.objects.filter(User.email != "a", User.email.not_in(["b"])).sql()
    assert Comment.objects.filter(~x.is_null(), x != None).sql()  # noqa: E711
    assert Comment.objects.filter(x > 20, x == 1, x.in_([1])).sql()


def test_a_nullable_column_behind_a_to_many_relation_follows_the_column_rule():
    for build in (
        lambda: Post.objects.exclude(Post.comments.author_id == 1),
        lambda: Post.objects.filter(Post.comments.author_id != 1),
    ):
        with pytest.raises(QueryError):
            build()
    assert Post.objects.filter(Post.comments.id != 1).sql()
    assert Post.objects.exclude(Post.comments.id == 1).sql()


NESTED = """
type Geo {
  lat Float
  lng Float?
}

type Spot {
  name  String
  geo   Geo
  where Geo?
}

model Place {
  id   Int   @id
  spot Spot
  opt  Spot?
  js   Spot  @storage(json)
  @@map("ns_place")
}
"""


def test_a_nested_subfield_is_nullable_only_when_its_declaration_says_so():
    import monorm

    Place = monorm.loads(NESTED, registry=monorm.Registry())["Place"]
    sure = [Place.spot.name, Place.spot.geo.lat, Place.js.name, Place.js.geo.lat]
    maybe = [Place.spot.geo.lng, Place.spot.where.lat, Place.opt.name, Place.opt.geo.lat, Place.js.geo.lng, Place.js.where.lat]
    def value(col):
        return "a" if repr(col).endswith("name") else 1.5

    for col in sure:
        v = value(col)
        assert Place.objects.filter(col != v).sql(), col
        assert Place.objects.exclude(col == v).sql(), col
        with pytest.raises(QueryError, match="NOT NULL"):
            (col != v).or_null()
    for col in maybe:
        v = value(col)
        with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
            Place.objects.filter(col != v)
        with pytest.raises(QueryError, match="under negation"):
            Place.objects.exclude(col == v)
        assert Place.objects.filter((col != v).or_null()).sql(), col


@pytest.mark.parametrize(
    "build",
    [
        lambda: ~((x > 1) | x.is_not_null()),
        lambda: ~((x > 1) & x.is_null()),
        lambda: ~((x != 1) & x.is_null()),
        lambda: Comment.objects.filter((x != 1) | x.is_not_null()),
        lambda: Comment.objects.filter((x != 1) & x.is_null()),
    ],
)
def test_a_null_test_of_the_wrong_direction_does_not_handle_a_comparison(build):
    with pytest.raises(QueryError, match="or_null"):
        build()


def test_a_null_test_of_the_right_direction_handles_a_comparison():
    assert Comment.objects.filter((x != 1) | x.is_null()).sql()
    assert Comment.objects.filter((x != 1) & x.is_not_null()).sql()
    assert Comment.objects.filter(~((x > 1) | x.is_null())).sql()
    assert Comment.objects.filter(~((x > 1) & x.is_not_null())).sql()


@pytest.fixture
def lines():
    import monorm

    return monorm.loads(
        """
model Item {
  shop BigInt
  id   BigInt
  lines Line[]
  @@id([shop, id])
  @@map("ns_item")
}

model Line {
  id      BigInt @id
  shop    BigInt?
  item_id BigInt?
  item    Item?  @relation(fields: [shop, item_id], references: [shop, id])
  @@map("ns_line")
}
""",
        registry=monorm.Registry(),
    )


def test_a_composite_key_ne_keeps_the_rows_with_a_null_key_column(lines):
    Line, Item = lines["Line"], lines["Item"]
    item = object.__new__(Item)
    item.__dict__.update(shop=1, id=2)
    assert Line.objects.filter(Line.item != item).sql().count(" IS NULL") == 2
    assert Line.objects.filter(~(Line.item == item)).sql().count(" IS NOT NULL") == 2


@pytest.mark.parametrize(
    "build",
    [
        lambda: ~User.email.like("a%"),
        lambda: ~Comment.body.contains("a"),
        lambda: ~Post.title.ilike("a%"),
    ],
)
def test_a_negated_like_on_a_not_null_column_needs_nothing(build):
    assert Post.objects.filter(~Post.title.like("a%")).sql()
    build()


def test_a_negated_like_on_a_nullable_column_raises():
    import monorm

    Reading = monorm.loads('model Reading {\n  id Int @id\n  name String?\n  @@map("ns_reading")\n}', registry=monorm.Registry())["Reading"]
    for build in (lambda: ~Reading.name.like("a%"), lambda: ~Reading.name.contains("a"), lambda: ~Reading.name.ilike("a%")):
        with pytest.raises(QueryError, match="under negation"):
            build()
    assert Reading.objects.filter(~Reading.name.like("a%").or_null()).sql()
    assert Reading.objects.filter(~Reading.name.contains("a").and_not_null()).sql()


def test_a_comparison_with_no_null_rule_builds_no_value_key(monkeypatch):
    from monorm import expr

    keys = []
    monkeypatch.setattr(expr, "_ckey", lambda e: keys.append(e) or "")
    Post.objects.get(Post.id == 1)
    Post.objects.filter(Post.published, Post.views > 500)
    assert keys == []


def test_a_nested_column_keeps_its_own_value_key():
    import monorm
    from monorm import expr

    schema = "type Address { city String }\nmodel Shop { id Int @id\n address Address? @storage(json) }"
    shop = monorm.loads(schema, registry=monorm.Registry())["Shop"]
    ref = shop.address.city
    assert expr._ckey(ref) == repr(ref) and expr._ckey(shop.address) == repr(shop.address)
    assert expr._ckey(ref) != expr._ckey(shop.address)
