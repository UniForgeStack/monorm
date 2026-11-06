"""Prepared reads held in Rust: a call sends what the unprepared query sends."""

import datetime as dt
import decimal
import os
import uuid

import pytest
from blog.models import Post, User

import monorm
from monorm import QueryError, func, param

SOURCES = {
    "postgresql": 'datasource db {\n provider = "postgresql"\n}\n',
    "sqlite": 'datasource db {\n provider = "sqlite"\n}\n',
}

KINDS = """
model Kind {
 id Int @id @default(autoincrement())
 big BigInt
 small Int
 real Float
 name String
 body String @db.Text
 at DateTime
 day DateTime @db.Date
 uid String @db.Uuid
 price Decimal
 @@map("pr_kind")
}
model Item {
 id Int @id @default(autoincrement())
 shop Int
 name String
 views Int @default(0)
 @@query.filter("tenant", "shop == scope.shop")
 @@map("pr_item")
}
"""

N = 5
STAMP = dt.datetime(2024, 1, 1, 12, 0, 0, tzinfo=dt.timezone.utc)
UIDS = [uuid.UUID(int=i + 1) for i in range(N)]
VALUES = {
    "big": [2**40 + i for i in range(N)],
    "small": [i * 7 for i in range(N)],
    "real": [i * 1.5 for i in range(N)],
    "name": [f"n{i}" for i in range(N)],
    "body": [f"b{i}" for i in range(N)],
    "at": [STAMP + dt.timedelta(days=i) for i in range(N)],
    "day": [dt.date(2024, 1, 1 + i) for i in range(N)],
    "uid": [str(u) for u in UIDS],
    "price": [decimal.Decimal(f"{i}.25") for i in range(N)],
}
# A value of the column that no row holds.
MISSING = {
    "big": 2**62,
    "small": 9999,
    "real": 123.5,
    "name": "zz",
    "body": "zz",
    "at": STAMP + dt.timedelta(days=99),
    "day": dt.date(2030, 1, 1),
    "uid": str(uuid.UUID(int=999)),
    "price": decimal.Decimal("99.5"),
}


def url(provider):
    if provider == "sqlite":
        return "sqlite://:memory:"
    u = os.environ.get("ORM_TEST_DATABASE_URL")
    if not u:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    return str(u)


async def open_db(provider, source=KINDS):
    if provider == "sqlite":
        source = source.replace(" price Decimal\n", "")
    registry = monorm.Registry()
    models = monorm.loads(SOURCES[provider] + source, registry=registry)
    db = await monorm.connect(url(provider), registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    return db, models


def same_rows(a, b):
    return [r.id for r in a] == [r.id for r in b]


# -- in_ of a param: a list of any length ----------------------------------------------------


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
@pytest.mark.parametrize("column", list(VALUES))
async def test_in_of_a_param_matches_the_plain_in_for_each_column_type(provider, column):
    if provider == "sqlite" and column == "price":
        pytest.skip("the SQLite driver has no decimal")
    db, models = await open_db(provider)
    Kind = models["Kind"]
    try:
        await Kind.objects.using(db).insert_many(
            [{"id": i + 1, **{c: v[i] for c, v in VALUES.items() if provider != "sqlite" or c != "price"}} for i in range(N)]
        )
        col = getattr(Kind, column)
        values = VALUES[column]
        inside = Kind.objects.using(db).filter(col.in_(param("v"))).order_by(Kind.id).prepare()
        outside = Kind.objects.using(db).filter(col.not_in(param("v"))).order_by(Kind.id).prepare()
        for chosen in ([], [values[1]], [values[0], values[3], MISSING[column]], values, [MISSING[column]]):
            plain_in = Kind.objects.using(db).filter(col.in_(chosen)).order_by(Kind.id)
            plain_out = Kind.objects.using(db).filter(col.not_in(chosen)).order_by(Kind.id)
            assert same_rows(await inside(v=chosen), await plain_in), (column, chosen)
            assert same_rows(await outside(v=chosen), await plain_out), (column, chosen)
        assert await inside.count(v=[values[2]]) == 1
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_in_of_a_param_takes_null_elements_and_an_empty_list(provider):
    db, models = await open_db(provider, "model Maybe {\n id Int @id\n n Int?\n @@map(\"pr_maybe\")\n}\n")
    Maybe = models["Maybe"]
    try:
        await Maybe.objects.using(db).insert_many([{"id": 1, "n": 1}, {"id": 2, "n": 2}, {"id": 3, "n": None}])
        inside = Maybe.objects.using(db).filter(Maybe.n.in_(param("v"))).order_by(Maybe.id).prepare()
        outside = Maybe.objects.using(db).filter(Maybe.n.not_in(param("v")).and_not_null()).order_by(Maybe.id).prepare()
        for chosen in ([], [1], [1, None], [None]):
            plain_in = Maybe.objects.using(db).filter(Maybe.n.in_(chosen)).order_by(Maybe.id)
            plain_out = Maybe.objects.using(db).filter(Maybe.n.not_in(chosen).and_not_null()).order_by(Maybe.id)
            assert same_rows(await inside(v=chosen), await plain_in), chosen
            assert same_rows(await outside(v=chosen), await plain_out), chosen
        with pytest.raises(ValueError, match="can't be None"):
            await inside(v=None)
    finally:
        await db.drop_tables()
        await db.close()


# -- the same statement and the same rows as the unprepared query -----------------------------


async def seed():
    alice, bob = await User.objects.insert_many(
        [{"email": "alice@example.com", "name": "Alice"}, {"email": "bob@example.com", "name": "Bob"}]
    ).returning()
    await Post.objects.insert_many(
        [
            {"author": alice, "title": "a1", "body": "", "views": 5},
            {"author": alice, "title": "A_2%", "body": "", "views": 50},
            {"author": alice, "title": "a3", "body": "", "views": 20},
            {"author": bob, "title": "b1", "body": "", "views": 100},
        ]
    )
    return alice, bob


def cases(alice, bob):
    """(prepared query set, the values of a call, the same query set with the values inlined)."""
    base = Post.objects.order_by(Post.id)
    return [
        (base.filter(Post.author_id == param("a")), {"a": alice.id}, base.filter(Post.author_id == alice.id)),
        (
            base.filter(Post.views >= param("lo"), Post.views < param("hi")),
            {"lo": 10, "hi": 60},
            base.filter(Post.views >= 10, Post.views < 60),
        ),
        (base.filter(Post.title.icontains(param("t"))), {"t": "_2%"}, base.filter(Post.title.icontains("_2%"))),
        (base.filter(Post.title.startswith(param("t"))), {"t": "a"}, base.filter(Post.title.startswith("a"))),
        (base.filter(Post.id.in_(param("ids"))), {"ids": [1, 3, 99]}, base.filter(Post.id.in_([1, 3, 99]))),
        (base.filter(Post.id.not_in(param("ids"))), {"ids": [1, 3]}, base.filter(Post.id.not_in([1, 3]))),
        (base.limit(param("n")).offset(param("o")), {"n": 2, "o": 1}, base[1:3]),
        (
            base.filter(Post.views + param("bump") > param("min")),
            {"bump": 10, "min": 55},
            base.filter(Post.views + 10 > 55),
        ),
        (base.load(Post.author).filter(Post.id == param("id")), {"id": 2}, base.load(Post.author).filter(Post.id == 2)),
        (
            User.objects.order_by(User.id).load(User.posts.objects.filter(Post.views >= param("min")).order_by(Post.id)),
            {"min": 10},
            User.objects.order_by(User.id).load(User.posts.objects.filter(Post.views >= 10).order_by(Post.id)),
        ),
        (
            User.objects.order_by(User.id).load(User.posts.objects.filter(Post.views >= param("min")).limit(param("k"))),
            {"min": 10, "k": 1},
            User.objects.order_by(User.id).load(User.posts.objects.filter(Post.views >= 10)[:1]),
        ),
    ]


async def test_a_call_sends_the_statement_of_the_plain_query(clean):
    alice, bob = await seed()
    for prepared_qs, values, plain in cases(alice, bob):
        prepared = prepared_qs.prepare()
        if "IN (" not in plain.sql():  # a param list becomes one array, so the text differs on purpose
            assert prepared.sql(**values) == plain.sql(), values
            assert prepared.inspect(**values) == plain.inspect(), values
        got, expect = await prepared(**values), await plain
        assert [(r.id, getattr(r, "title", None)) for r in got] == [(r.id, getattr(r, "title", None)) for r in expect], values
        assert await prepared.count(**values) == await plain.count(), values
        assert await prepared.exists(**values) == await plain.exists(), values


async def test_a_prefetch_of_one_key_uses_one_array_on_postgres(clean):
    await seed()
    q = User.objects.order_by(User.id).load(User.posts.objects.filter(Post.views >= param("min"))).prepare()
    seen = []
    remove = clean.on_query(seen.append)
    try:
        users = await q(min=10)
    finally:
        remove()
    assert len(seen) == 2
    assert "ANY(" in seen[1].sql, seen[1].sql
    assert sorted(len(u.posts.cached) for u in users) == [1, 2]


async def test_a_param_in_a_case_needs_a_declared_type():
    case = func.case((Post.views > 10, param("big")), default=param("small"))
    with pytest.raises(QueryError, match="declare its type"):
        Post.objects.select(case).prepare()


async def test_a_declared_type_lets_a_case_of_params_prepare(clean):
    await seed()
    case = func.case((Post.views > 10, param("big", int)), default=param("small", int))
    q = Post.objects.select(Post.id, case).order_by(Post.id).prepare()
    assert [tuple(r) for r in await q(big=1, small=0)] == [(1, 0), (2, 1), (3, 1), (4, 1)]


async def test_a_param_none_is_refused_for_in_too():
    q = Post.objects.filter(Post.id.in_(param("ids"))).prepare()
    with pytest.raises(ValueError, match="can't be None"):
        await q(ids=None)


# -- scope: per-call values --------------------------------------------------------------------


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_scope_comes_from_the_context_or_the_keyword(provider):
    db, models = await open_db(provider)
    Item = models["Item"]
    try:
        await Item.objects.using(db).without_defaults().insert_many(
            [
                {"shop": 1, "name": "a", "views": 1},
                {"shop": 1, "name": "b", "views": 20},
                {"shop": 2, "name": "c", "views": 30},
            ]
        )
        # Prepared with no scope active.
        q = Item.objects.using(db).filter(Item.views >= param("lo")).order_by(Item.id).prepare()
        names = lambda rows: [r.name for r in rows]  # noqa: E731
        with monorm.scope(shop=1):
            assert names(await q(lo=0)) == ["a", "b"]
            assert await q.count(lo=0) == 2
        with monorm.scope(shop=2):
            assert names(await q(lo=0)) == ["c"]
            assert (await q.first(lo=0)).name == "c"
        assert names(await q(lo=0, scope={"shop": 2})) == ["c"]
        assert names(await q(lo=10, scope={"shop": 1})) == ["b"]
        assert await q.exists(lo=25, scope={"shop": 2})
        assert not await q.exists(lo=25, scope={"shop": 1})
        assert (await q.get(lo=20, scope={"shop": 1})).name == "b"
        # The keyword wins over the context.
        with monorm.scope(shop=1):
            assert names(await q(lo=0, scope={"shop": 2})) == ["c"]
        # The same call and the plain query agree on the SQL.
        with monorm.scope(shop=2):
            plain = Item.objects.using(db).filter(Item.views >= 5).order_by(Item.id)
            assert q.sql(lo=5) == plain.sql()
            assert q.inspect(lo=5) == plain.inspect()
            assert q.sql(lo=5) == q.sql(lo=5, scope={"shop": 2})
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_scope_errors(provider):
    db, models = await open_db(provider)
    Item = models["Item"]
    try:
        q = Item.objects.using(db).filter(Item.views >= param("lo")).prepare()
        with pytest.raises(QueryError, match=r"reads scope\.shop, which is not set"):
            await q(lo=0)
        with pytest.raises(QueryError, match=r"scope\.shop is None"):
            await q(lo=0, scope={"shop": None})
        with pytest.raises(QueryError, match=r"scope\.shop is None"):
            with monorm.scope(shop=None):
                await q(lo=0)
        with pytest.raises(ValueError, match="scope"):
            param("scope")
    finally:
        await db.drop_tables()
        await db.close()


# -- prepare-time errors -----------------------------------------------------------------------


async def test_a_shape_that_cannot_be_a_slot_raises_at_prepare():
    source = (
        "model Owner {\n id Int @id\n things Thing[]\n @@map(\"pr_owner\")\n}\n"
        "model Thing {\n id Int @id\n owner_id Int\n owner Owner @relation(fields: [owner_id], references: [id])\n @@map(\"pr_thing\")\n}\n"
    )
    registry = monorm.Registry()
    models = monorm.loads(SOURCES["sqlite"] + source, registry=registry)
    Owner = models["Owner"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        # SQLite builds the prefetch text per call, so a param in its limit has no slot.
        with pytest.raises(QueryError, match="limit or offset cannot be prepared"):
            Owner.objects.using(db).load(Owner.things.objects.limit(param("k"))).prepare()
    finally:
        await db.close()


async def test_prepared_calls_run_in_the_current_transaction(clean):
    await seed()
    q = Post.objects.filter(Post.views >= param("lo")).order_by(Post.id).prepare()
    with pytest.raises(RuntimeError, match="rolled back"):
        async with monorm.get_database().transaction():
            await Post.objects.filter(Post.id == 1).update(views=500)
            assert [p.views for p in await q(lo=400)] == [500]
            raise RuntimeError("rolled back")
    assert await q(lo=400) == []


async def test_hooks_run_for_every_call(clean):
    await seed()
    q = Post.objects.filter(Post.id == param("id")).prepare()
    seen = []
    remove = clean.on_query(seen.append)
    try:
        await q(id=1)
        await q(id=2)
    finally:
        remove()
    assert len(seen) == 2
