"""`Select` count, exists, prepare, explain, CTEs, lazy one-row reads, `[n]`, `last()` and paging."""

import pytest
from blog.models import Post, User
from pathlib import Path

import monorm
from monorm import DoesNotExist, MultipleObjectsReturned, QueryError, func

SQLITE_SOURCE = Path(__file__).resolve().parents[3].joinpath("examples/sqlite/schema.prisma").read_text()


async def seed():
    alice, bob, carol = await User.objects.insert_many(
        [{"email": "a@example.com", "name": "Alice"}, {"email": "b@example.com", "name": "Bob"}, {"email": "c@example.com", "name": "Carol"}]
    ).returning()
    await Post.objects.insert_many(
        [
            {"author": alice, "title": "a1", "body": "", "views": 80},
            {"author": alice, "title": "a2", "body": "", "views": 40},
            {"author": bob, "title": "b1", "body": "", "views": 5},
        ]
    )
    return alice, bob, carol


def per_author():
    return Post.objects.select(Post.author_id, func.count().label("n")).group_by(Post.author_id)


async def test_count_counts_the_rows_of_a_plain_select(clean):
    await seed()
    assert await Post.objects.select(Post.title).count() == 3
    assert await Post.objects.select(Post.title).filter(Post.views > 10).count() == 2


async def test_count_counts_groups_not_rows(clean):
    await seed()
    assert await per_author().count() == 2
    assert await per_author().having(func.count() > 1).count() == 1


async def test_count_follows_distinct_and_slices(clean):
    await seed()
    assert await Post.objects.select(Post.author_id).distinct().count() == 2
    assert await Post.objects.select(Post.title).order_by(Post.title)[:2].count() == 2


async def test_count_of_an_aggregate_without_groups_is_one(clean):
    await seed()
    assert await Post.objects.select(func.sum(Post.views)).count() == 1


async def test_exists_follows_the_groups(clean):
    await seed()
    assert await per_author().exists() is True
    assert await per_author().having(func.count() > 5).exists() is False
    assert await Post.objects.select(Post.title).filter(Post.views > 1000).exists() is False


async def test_count_and_exists_sql_shape(clean, observe_queries):
    await seed()
    with observe_queries(clean) as events:
        await per_author().having(func.count() > 1).count()
        await per_author().exists()
        await Post.objects.select(Post.title).count()
    grouped_count, grouped_exists, plain_count = (e.sql.lower() for e in events)
    assert grouped_count.startswith("select count(*) from (select") and "group by" in grouped_count and "having" in grouped_count
    assert grouped_exists.startswith("select exists(select") and "group by" in grouped_exists
    assert plain_count.startswith("select count(*) from (select")


async def test_explain_returns_a_plan(clean):
    await seed()
    plan = await per_author().explain()
    assert "Aggregate" in plan or "Scan" in plan


async def test_prepare_runs_with_values(clean):
    await seed()
    q = Post.objects.filter(Post.views > monorm.param("min")).select(Post.title, Post.views).order_by(Post.title).prepare()
    assert [tuple(r) for r in await q(min=10)] == [("a1", 80), ("a2", 40)]
    assert [tuple(r) for r in await q(min=50)] == [("a1", 80)]
    assert tuple(await q.first(min=10)) == ("a1", 80)
    assert await q.first(min=1000) is None
    assert await q.count(min=10) == 2
    assert await q.exists(min=1000) is False
    assert q.params == frozenset({"min"})


async def test_prepare_scalars_and_scalar(clean):
    await seed()
    q = Post.objects.filter(Post.views > monorm.param("min")).select(Post.views).order_by(Post.views).prepare()
    assert await q.scalars(min=10) == [40, 80]
    assert await q.scalar(min=10) == 40
    assert await q.scalar(min=1000) is None


async def test_prepare_counts_groups(clean):
    await seed()
    q = Post.objects.filter(Post.views > monorm.param("min")).select(Post.author_id, func.count().label("n")).group_by(Post.author_id).prepare()
    assert await q.count(min=0) == 2
    assert await q.count(min=50) == 1


async def test_prepare_require_raises_on_no_row(clean):
    await seed()
    q = Post.objects.filter(Post.views > monorm.param("min")).select(Post.title).require().prepare()
    with pytest.raises(DoesNotExist):
        await q.first(min=1000)
    with pytest.raises(DoesNotExist):
        await q(min=1000)


async def test_prepared_one_row_queries_of_a_select(clean):
    await seed()
    by_title = Post.objects.select(Post.title, Post.views).filter(Post.title == monorm.param("t"))
    assert tuple(await by_title.first().prepare()(t="a1")) == ("a1", 80)
    assert await by_title.first().prepare()(t="zz") is None
    assert tuple(await by_title.get().prepare()(t="b1")) == ("b1", 5)
    many = Post.objects.select(Post.title).filter(Post.views > monorm.param("min")).get().prepare()
    with pytest.raises(MultipleObjectsReturned):
        await many(min=1)


async def test_first_and_get_are_lazy(clean):
    await seed()
    first = Post.objects.select(Post.title).order_by(Post.title).first()
    assert tuple(await first) == ("a1",)
    assert tuple(await first) == ("a1",)
    assert await Post.objects.select(Post.title).get(Post.title == "zz") is None
    with pytest.raises(MultipleObjectsReturned):
        await Post.objects.select(Post.title).get(Post.views > 1)
    with pytest.raises(DoesNotExist):
        await Post.objects.select(Post.title).require().get(Post.title == "zz")
    assert first.sql().lower().count("limit") == 1


async def test_index_reads_the_row_at_a_position(clean):
    await seed()
    q = Post.objects.select(Post.title).order_by(Post.title)
    assert tuple(await q[0]) == ("a1",)
    assert tuple(await q[2]) == ("b1",)
    assert await q[3] is None
    assert tuple(await q[-1]) == ("b1",)
    with pytest.raises(ValueError):
        q[-2]


async def test_last_reverses_the_order(clean):
    await seed()
    assert tuple(await Post.objects.select(Post.title).order_by(Post.title).last()) == ("b1",)
    assert tuple(await Post.objects.select(Post.title).order_by(Post.title.desc()).last()) == ("a1",)
    assert tuple(await per_author().order_by(Post.author_id).last())[1] == 1


async def test_last_and_index_need_an_order_on_grouped_and_distinct_selects():
    for q in (per_author(), Post.objects.select(Post.author_id).distinct(), Post.objects.select(func.sum(Post.views))):
        with pytest.raises(TypeError, match="order_by"):
            q.last()
        with pytest.raises(TypeError, match="order_by"):
            q[0]
        with pytest.raises(TypeError, match="order_by"):
            q[monorm.param("n")]


async def test_last_refuses_an_order_past_the_distinct_on_columns(clean):
    q = Post.objects.select(Post.author_id, Post.title).distinct(Post.author_id)
    with pytest.raises(TypeError, match="distinct"):
        q.order_by(Post.author_id, Post.views.desc()).last()
    with pytest.raises(TypeError, match="distinct"):
        q.order_by(Post.author_id, Post.views.desc())[-1]
    await seed()
    assert (await q.order_by(Post.author_id).last()) is not None


async def test_index_takes_a_param(clean):
    await seed()
    q = Post.objects.select(Post.title).order_by(Post.title)[monorm.param("n")].prepare()
    assert tuple(await q(n=1)) == ("a2",)


async def test_join_and_from_a_cte_in_a_select(clean):
    await seed()
    totals = per_author().cte("totals")
    rows = await (
        User.objects.select(User.name, totals.c.n)
        .join(totals, totals.c.author_id == User.id)
        .order_by(totals.c.n.desc(), User.name)
    )
    assert [tuple(r) for r in rows] == [("Alice", 2), ("Bob", 1)]
    grouped = await (
        User.objects.select(totals.c.n, func.count().label("users"))
        .join(totals, totals.c.author_id == User.id)
        .group_by(totals.c.n)
        .having(totals.c.n > 0)
        .order_by(totals.c.n)
    )
    assert [tuple(r) for r in grouped] == [(1, 1), (2, 1)]
    ranked = Post.objects.select(Post, func.row_number().over(order_by=Post.views.desc()).label("rank")).cte("ranked")
    top = Post.objects.select(Post.title, ranked.c.rank).from_(ranked).filter(ranked.c.rank <= 2).order_by(ranked.c.rank)
    assert [tuple(r) for r in await top] == [("a1", 1), ("a2", 2)]
    assert await top.count() == 2


async def test_paginate_a_grouped_select(clean):
    await seed()
    q = per_author().order_by(Post.author_id)
    page = await q.paginate(first=1)
    assert [r.n for r in page.items] == [2]
    assert page.has_next and not page.has_previous
    nxt = await q.paginate(first=1, after=page.next_cursor)
    assert [r.n for r in nxt.items] == [1]
    assert not nxt.has_next and nxt.has_previous
    back = await q.paginate(last=1, before=nxt.previous_cursor)
    assert [r.n for r in back.items] == [2]


async def test_paginate_walks_a_plain_select_with_the_key_in_the_list(clean):
    await seed()
    q = Post.objects.select(Post.id, Post.title).order_by(Post.title)
    seen = []
    cursor = None
    while True:
        page = await q.paginate(first=2, after=cursor)
        seen += [r.title for r in page.items]
        if not page.has_next:
            break
        cursor = page.next_cursor
    assert seen == ["a1", "a2", "b1"]


async def test_paginate_a_distinct_on_select(clean):
    await seed()
    q = Post.objects.select(Post.author_id, Post.title).distinct(Post.author_id).order_by(Post.author_id, Post.title)
    page = await q.paginate(first=1)
    assert [r.title for r in page.items] == ["a1"]
    nxt = await q.paginate(first=1, after=page.next_cursor)
    assert [r.title for r in nxt.items] == ["b1"]
    assert not nxt.has_next


async def test_paginate_rejects_what_it_cannot_page(clean):
    await seed()
    with pytest.raises(QueryError, match="distinct"):
        await Post.objects.select(Post.author_id).distinct().paginate(first=1)
    with pytest.raises(QueryError, match="select"):
        await Post.objects.select(Post.title).order_by(Post.title).paginate(first=1)
    with pytest.raises(QueryError, match="sliced"):
        await Post.objects.select(Post.id).order_by(Post.id)[:2].paginate(first=1)


async def test_paginate_sql_shape(clean, observe_queries):
    await seed()
    q = per_author().order_by(Post.author_id)
    page = await q.paginate(first=1)
    with observe_queries(clean) as events:
        await q.paginate(first=1, after=page.next_cursor)
    (sql,) = (e.sql.lower() for e in events)
    assert "group by" in sql and "having" not in sql
    assert "author_id" in sql.split("order by")[1] and "limit" in sql
    assert "where" in sql


async def test_batches_and_iterate_read_every_row(clean):
    await seed()
    q = Post.objects.select(Post.id, Post.title).order_by(Post.title)
    assert [[r.title for r in b] async for b in q.batches(2)] == [["a1", "a2"], ["b1"]]
    assert [r.title async for r in q.iterate(2)] == ["a1", "a2", "b1"]
    with pytest.raises(ValueError):
        [b async for b in q.batches(0)]
    grouped = per_author().order_by(Post.author_id)
    assert sorted([r.n async for r in grouped.iterate(1)]) == [1, 2]


def test_select_has_no_lock():
    assert not hasattr(Post.objects.select(Post.id), "lock")


async def test_sqlite_select_reads():
    registry = monorm.Registry()
    m = monorm.loads(SQLITE_SOURCE, registry=registry)
    Author, Book = m["Author"], m["Book"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    try:
        a, b = (
            await Author.objects.using(db)
            .insert_many([{"email": "a@example.com", "name": "Alice"}, {"email": "b@example.com", "name": "Bob"}])
            .returning()
        )
        await Book.objects.using(db).insert_many(
            [{"author_id": a.id, "title": "one"}, {"author_id": a.id, "title": "two"}, {"author_id": b.id, "title": "three"}]
        )
        grouped = Book.objects.using(db).select(Book.author_id, func.count().label("n")).group_by(Book.author_id)
        assert await grouped.count() == 2
        assert await grouped.having(func.count() > 1).exists() is True
        assert await grouped.having(func.count() > 5).exists() is False
        ordered = grouped.order_by(Book.author_id)
        assert [r.n for r in (await ordered.paginate(first=1)).items] == [2]
        assert tuple(await ordered.last())[1] == 1
        q = Book.objects.using(db).filter(Book.id > monorm.param("min")).select(Book.title).order_by(Book.id).prepare()
        assert await q.scalars(min=1) == ["two", "three"]
        assert await q.count(min=0) == 3
        assert [r.title async for r in Book.objects.using(db).select(Book.id, Book.title).order_by(Book.id).iterate(2)] == ["one", "two", "three"]
    finally:
        await db.drop_tables()
        await db.close()
