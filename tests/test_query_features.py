"""End-to-end tests: in_bulk, exists() / scalar subqueries with outer(), window functions,
CTEs, nested and filtered prefetches, instances built in Rust."""

from datetime import datetime, timedelta, timezone
from decimal import Decimal

import pytest
from blog.models import Comment, Post, Profile, Role, User

import orm
from conftest import DATABASE_URL
from orm import QueryError, exists, func, outer, window
from orm.expr import Func

NOW = datetime.now(timezone.utc)


async def seed():
    alice, bob, carol = await User.objects.insert_many(
        [
            {"email": "alice@example.com", "name": "Alice"},
            {"email": "bob@example.com", "name": "Bob"},
            {"email": "carol@example.com", "name": "Carol"},
        ]
    ).returning()
    posts = await Post.objects.insert_many(
        [
            {"author": alice, "title": "a1", "body": "", "views": 5, "created_at": NOW - timedelta(days=3)},
            {"author": alice, "title": "a2", "body": "", "views": 50, "created_at": NOW - timedelta(days=2)},
            {"author": alice, "title": "a3", "body": "", "views": 20, "created_at": NOW - timedelta(days=1)},
            {"author": bob, "title": "b1", "body": "", "views": 100, "created_at": NOW - timedelta(days=5)},
        ]
    ).returning()
    a1, a2, a3, b1 = posts
    await Comment.objects.insert_many(
        [
            {"post": a2, "author": bob, "body": "c1"},
            {"post": a2, "author": None, "body": "c2"},
            {"post": b1, "author": alice, "body": "c3"},
        ]
    )
    return (alice, bob, carol), posts


# -- in_bulk ---------------------------------------------------------------------------------


async def test_in_bulk(clean):
    (alice, bob, carol), _ = await seed()
    got = await User.objects.in_bulk([alice.id, carol.id, 999, alice.id])
    assert got == {alice.id: alice, carol.id: carol}
    assert got[alice.id].name == "Alice"
    assert await User.objects.in_bulk([]) == {}
    by_email = await User.objects.in_bulk(["bob@example.com"], field=User.email)
    assert by_email == {"bob@example.com": bob}
    assert set(await User.objects.filter(User.name != "Bob").in_bulk()) == {alice.id, carol.id}
    with pytest.raises(ValueError, match="unique"):
        await User.objects.in_bulk(["Bob"], field=User.name)
    with pytest.raises(QueryError, match="sliced"):
        await User.objects.all()[:1].in_bulk([1])


async def test_in_bulk_chunks(clean, monkeypatch, observe_queries):
    (alice, bob, carol), _ = await seed()
    monkeypatch.setattr(orm.query, "IN_BULK_CHUNK", 2)
    with observe_queries(clean) as events:
        assert set(await User.objects.in_bulk([alice.id, bob.id, carol.id])) == {alice.id, bob.id, carol.id}
    assert len(events) == 2


# -- exists() and scalar subqueries ------------------------------------------------------


async def test_exists_expression(clean):
    (alice, bob, carol), _ = await seed()
    popular = exists(Post.objects.filter(Post.author_id == outer(User.id), Post.views >= 50))
    query = User.objects.filter(popular)
    assert query.inspect().statements[0].correlated_count == 1
    assert query.inspect().statements[0].uncorrelated_count == 0
    assert {u.name for u in await query} == {"Alice", "Bob"}
    assert {u.name for u in await User.objects.filter(~popular)} == {"Carol"}
    # In select(): a boolean per row.
    rows = await User.objects.select(User.name, popular.label("popular")).order_by(User.name)
    assert [tuple(r) for r in rows] == [("Alice", True), ("Bob", True), ("Carol", False)]
    # Uncorrelated.
    query = User.objects.filter(exists(Post.objects.filter(Post.views > 1000)))
    assert query.inspect().statements[0].correlated_count == 0
    assert query.inspect().statements[0].uncorrelated_count == 1
    assert len(await query) == 0
    # A select() with group_by / having.
    many = exists(
        Post.objects.filter(Post.author_id == outer(User.id)).select(Post.author_id).group_by(Post.author_id).having(func.count() > 2)
    )
    assert [u.name for u in await User.objects.filter(many)] == ["Alice"]


async def test_scalar_subquery(clean):
    (alice, bob, carol), _ = await seed()
    latest = (
        Post.objects.filter(Post.author_id == outer(User.id))
        .order_by(Post.created_at.desc())
        .select(Post.title)[:1]
        .as_scalar()
    )
    query = User.objects.select(User, latest.label("latest")).order_by(User.id)
    result = query.inspect()
    assert result.statement_count == 1
    assert not result.has_batching
    assert result.statements[0].joins_count == 0
    assert result.statements[0].correlated_count == 1
    assert result.statements[0].uncorrelated_count == 0
    rows = await query
    assert [(u.name, t) for u, t in rows] == [("Alice", "a3"), ("Bob", "b1"), ("Carol", None)]
    assert rows[0].latest == "a3" and rows[0].user == alice
    # In filters and ordering.
    best = Post.objects.filter(Post.author_id == outer(User.id)).select(func.max(Post.views)).as_scalar()
    assert [u.name for u in await User.objects.filter(best > 60)] == ["Bob"]
    assert [u.name for u in await User.objects.filter(best.is_not_null()).order_by(best.desc())] == ["Bob", "Alice"]
    # Correlated to the same model: posts above their author's average.
    avg = Post.objects.filter(Post.author_id == outer(Post.author_id)).select(func.avg(Post.views)).as_scalar()
    assert sorted(p.title for p in await Post.objects.filter(Post.views > avg)) == ["a2"]


async def test_scalar_subquery_in_update(clean):
    (alice, bob, carol), _ = await seed()
    n_comments = Comment.objects.filter(Comment.post_id == outer(Post.id)).select(func.count()).as_scalar()
    statement = Post.objects.update(views=n_comments)
    assert statement.inspect() == orm.QueryInspection(1, False, (
        orm.StatementInspection(False, 0, 1, 0, 0),
    ))
    await statement
    views = {p.title: p.views for p in await Post.objects.all()}
    assert views == {"a1": 0, "a2": 2, "a3": 0, "b1": 1}


async def test_string_functions_and_concatenation(clean):
    await seed()
    rows = await Comment.objects.order_by(Comment.id).select(
        Comment.body, func.concat(Comment.author.name, "!").label("c"), Comment.author.name.concat("!").label("p")
    )
    assert [tuple(r) for r in rows] == [("c1", "Bob!", "Bob!"), ("c2", "!", None), ("c3", "Alice!", "Alice!")]
    row = await Post.objects.filter(Post.title == "b1").select(
        func.trim(func.concat("  ", Post.title, " ")).label("t"), func.ltrim(func.concat("  ", Post.title)).label("l"),
        func.rtrim(func.concat(Post.title, "  ")).label("r"), func.replace(Post.title, "b", "B").label("x"),
        func.substr(Post.title, 2).label("s"), func.strpos(Post.title, "1").label("i"),
    ).first()
    assert row is not None and tuple(row) == ("b1", "b1", "b1", "B1", "1", 2)
    assert [p.title for p in await Post.objects.filter(Post.title.concat("!") == "a2!")] == ["a2"]
    # A literal after a non-text column still binds as text.
    assert await Post.objects.filter(Post.title == "b1").select(func.concat(Post.views, "!")).scalar() == "100!"


async def test_case_null_first_branch(clean):
    await seed()
    rows = await Post.objects.order_by(Post.title).select(func.case((Post.views >= 50, None), default=1)).scalars()
    assert rows == [1, None, 1, None]


async def seed_profiles():
    seeded = await seed()
    (alice, bob, _), _ = seeded
    await Profile.objects.insert_many([{"user": alice, "role": "admin"}, {"user": bob}])
    return seeded


async def test_case_enum_branch(clean):
    await seed_profiles()
    roles = await Profile.objects.order_by(Profile.id).select(func.case((Profile.balance > 5, Profile.role), default="editor")).scalars()
    assert roles == [Role.editor, Role.editor]
    roles = await Profile.objects.order_by(Profile.id).select(func.case((Profile.balance <= 5, Profile.role), default="editor")).scalars()
    assert roles == [Role.admin, Role.member]


async def test_case_value_types(clean):
    await seed_profiles()
    # a decimal among integers makes the case a decimal
    value = await Post.objects.filter(Post.title == "a1").select(func.case((Post.published, 1), default=Decimal("0.5"))).scalar()
    assert value == Decimal("0.5") and isinstance(value, Decimal)
    # the type the context expects wins over the case's own: an integer branch assigned to a decimal field
    await Profile.objects.update(balance=func.case((Profile.role == Role.admin, 7), default=2))
    assert sorted(await Profile.objects.select(Profile.balance).scalars()) == [Decimal("2"), Decimal("7")]
    assert await Profile.objects.filter(Profile.balance > func.case((Profile.role == Role.admin, 1), default=9)).count() == 1


async def test_case_expression(clean):
    (alice, bob, carol), _ = await seed()
    heat = func.case((Post.views >= 50, "hot"), (Post.views >= 20, "warm"), default="cold")
    rows = await Post.objects.order_by(Post.title).select(Post.title, heat.label("heat"))
    assert [tuple(r) for r in rows] == [("a1", "cold"), ("a2", "hot"), ("a3", "warm"), ("b1", "hot")]
    # Several conditional counts in one grouped query (Django's Sum(Case(When(...)))).
    counts = await Post.objects.order_by(Post.author_id).select(
        Post.author_id,
        func.sum(func.case((Post.views >= 50, 1), default=0)).label("hot"),
        func.sum(func.case((Post.views < 50, Post.views), default=0)).label("rest"),
    ).group_by(Post.author_id)
    assert [tuple(r) for r in counts] == [(alice.id, 1, 25), (bob.id, 1, 0)]
    assert await Post.objects.filter(heat == "hot").count() == 2
    by_heat = await Post.objects.order_by(func.case((Post.views >= 50, 0), default=1), Post.title).select(Post.title).scalars()
    assert by_heat == ["a2", "b1", "a1", "a3"]
    # annotate: the model with a computed column
    users = await User.objects.order_by(User.id).select(User, func.case((func.count(User.posts) > 1, "many"), default="few"))
    assert [(u.name, n) for u, n in users] == [("Alice", "many"), ("Bob", "few"), ("Carol", "few")]
    # in an update, values bind with the field's type
    await Post.objects.update(views=func.case((Post.views >= 50, Post.views - 50), (Post.title == "a1", 1), default=Post.views))
    views = {p.title: p.views for p in await Post.objects.all()}
    assert views == {"a1": 1, "a2": 0, "a3": 20, "b1": 50}
    # a float among integer values makes the case a float
    assert await Post.objects.filter(Post.title == "a1").select(func.case((Post.published, 1), default=0.5)).scalar() == 0.5


async def test_aggregate_filter(clean):
    (alice, bob, carol), _ = await seed()
    rows = await Post.objects.order_by(Post.author_id).select(
        Post.author_id,
        func.count().label("all"),
        func.count(filter=Post.views >= 20).label("popular"),
        func.sum(Post.views, filter=Post.views < 50).label("small"),
        func.max(Post.title, filter=Post.views < 50).label("last_small"),
    ).group_by(Post.author_id)
    assert [tuple(r) for r in rows] == [(alice.id, 3, 2, 25, "a3"), (bob.id, 1, 1, None, None)]
    mins = await Post.objects.order_by(Post.author_id).select(Post.author_id, func.min(Post.views, filter=Post.views >= 20)).group_by(Post.author_id)
    assert [tuple(r) for r in mins] == [(alice.id, 20), (bob.id, 100)]
    users = await User.objects.order_by(User.id).select(User.name, func.count(User.posts, filter=User.posts.views > 10))
    assert [tuple(u) for u in users] == [("Alice", 2), ("Bob", 1), ("Carol", 0)]
    running = await Post.objects.order_by(Post.created_at).select(
        Post.title, func.count(filter=Post.views >= 50).over(order_by=Post.created_at)
    )
    assert [tuple(r) for r in running] == [("b1", 1), ("a1", 1), ("a2", 2), ("a3", 2)]
    # a filter that reads a to-one relation, in a window
    by_author = await Post.objects.order_by(Post.created_at).select(
        Post.title, func.sum(Post.views, filter=Post.author.name == "Alice").over(order_by=Post.created_at)
    )
    assert [tuple(r) for r in by_author] == [("b1", None), ("a1", 5), ("a2", 55), ("a3", 75)]


async def test_aggregate_filter_through_a_to_one_hop(clean):
    _, _ = await seed()
    posts = Post.objects
    # The filter follows to-one hops only: a plain aggregate over the posts, filtered by an EXISTS.
    assert await posts.select(func.sum(Post.views, filter=Post.author.name == "Alice")).scalar() == 75
    assert await posts.select(func.count(filter=Post.author.name == "Alice")).scalar() == 3
    rows = await posts.order_by(Post.author_id).select(
        Post.author_id, func.sum(Post.views, filter=Post.author.name == "Alice")
    ).group_by(Post.author_id)
    assert [r[1] for r in rows] == [75, None]


async def test_relation_count_with_an_outer_filter(clean):
    await seed()
    users = User.objects.order_by(User.id)
    rows = await users.select(User.name, func.count(User.posts, filter=User.name == "Alice"))
    assert [tuple(r) for r in rows] == [("Alice", 3), ("Bob", 0), ("Carol", 0)]
    # beside a second aggregate of the same relation, which shares a LATERAL join
    rows = await users.select(
        User.name, func.count(User.posts, filter=User.name == "Alice"), func.max(User.posts.views)
    )
    assert [tuple(r) for r in rows] == [("Alice", 3, 50), ("Bob", 0, 100), ("Carol", 0, None)]
    # a to-one hop under the relation is planned inside the subquery
    rows = await users.select(User.name, func.count(User.posts, filter=User.posts.author.name == "Alice"))
    assert [tuple(r) for r in rows] == [("Alice", 3), ("Bob", 0), ("Carol", 0)]


async def test_relation_count_with_a_to_one_filter_under_another_relation(clean):
    await seed_profiles()
    users = User.objects.order_by(User.id)
    # Django: Count("posts", filter=Q(profile__role="admin")) joins the profile once per user,
    # so the filter keeps all the posts of an admin and none of the others.
    rows = await users.select(User.name, func.count(User.posts, filter=User.profile.role == Role.admin))
    assert [tuple(r) for r in rows] == [("Alice", 3), ("Bob", 0), ("Carol", 0)]
    rows = await users.select(User.name, func.count(User.posts, filter=User.profile.role == Role.member))
    assert [tuple(r) for r in rows] == [("Alice", 0), ("Bob", 1), ("Carol", 0)]


async def test_array_agg(clean):
    (alice, bob, carol), _ = await seed()
    # Grouped: the elements in the order given; the empty group does not exist.
    rows = await Post.objects.order_by(Post.author_id).select(
        Post.author_id,
        func.array_agg(Post.title, order_by=Post.views.desc()).label("by_views"),
        func.array_agg(Post.views, order_by=[Post.views]).label("views"),
        func.array_agg(Post.title, filter=Post.views >= 20, order_by=Post.title).label("popular"),
    ).group_by(Post.author_id)
    assert [tuple(r) for r in rows] == [
        (alice.id, ["a2", "a3", "a1"], [5, 20, 50], ["a2", "a3"]),
        (bob.id, ["b1"], [100], ["b1"]),
    ]
    assert await Post.objects.select(func.array_agg(Post.views, distinct=True, order_by=Post.views)).scalar() == [5, 20, 50, 100]
    # NULL elements are kept; no rows gives None, not [].
    comments = await Comment.objects.select(func.array_agg(Comment.author_id, order_by=Comment.body)).scalar()
    assert comments == [bob.id, None, alice.id]
    assert await Post.objects.filter(Post.views > 1000).select(func.array_agg(Post.title)).scalar() is None
    # Over a relation: one correlated subquery per user, None for a user without posts.
    users = await User.objects.order_by(User.id).select(
        User.name,
        func.array_agg(User.posts.title, order_by=User.posts.views.desc()).label("titles"),
        func.count(User.posts).label("n"),
    )
    assert [tuple(u) for u in users] == [("Alice", ["a2", "a3", "a1"], 3), ("Bob", ["b1"], 1), ("Carol", None, 0)]
    assert users[0].titles == ["a2", "a3", "a1"]
    filtered = await User.objects.order_by(User.id).select(
        func.array_agg(User.posts.title, filter=User.posts.views < 50, order_by=User.posts.title)
    ).scalars()
    assert filtered == [["a1", "a3"], None, None]


async def test_array_agg_errors(clean):
    await seed()
    with pytest.raises(QueryError, match="takes no order_by"):
        await Post.objects.select(Func("sum", (Post.views,), order_by=Post.views))
    with pytest.raises(QueryError, match="can't be a window function"):
        await Post.objects.select(func.array_agg(Post.title, order_by=Post.views).over(partition_by=Post.author_id))
    with pytest.raises(QueryError, match="over relations needs all its columns on one relation path"):
        await User.objects.select(func.array_agg(User.posts.title, order_by=User.comments.body))


async def test_outer_through_relation_paths(clean):
    await seed()
    others = (
        Comment.objects.filter(Comment.post_id == outer(Post.id), Comment.author.name != outer(Post.author.name))
        .select(func.count())
        .as_scalar()
    )
    rows = await Post.objects.order_by(Post.title).select(Post.title, others.label("n"))
    assert [tuple(r) for r in rows] == [("a1", 0), ("a2", 1), ("a3", 0), ("b1", 1)]
    email = User.objects.filter(User.name == outer(Comment.post.author.name)).select(User.email).as_scalar()
    rows = await Comment.objects.order_by(Comment.id).select(Comment.body, email.label("email"))
    assert [tuple(r) for r in rows] == [("c1", "alice@example.com"), ("c2", "alice@example.com"), ("c3", "bob@example.com")]


async def test_outer_errors(clean):
    with pytest.raises(ValueError, match=r"use outer\(User.id\)"):
        User.objects.filter(exists(Post.objects.filter(Post.author_id == User.id))).sql()
    with pytest.raises(ValueError, match="not a column of an enclosing query"):
        User.objects.filter(User.id == outer(User.id)).sql()
    with pytest.raises(QueryError, match="needs a to-one relation"):
        User.objects.filter(exists(Post.objects.filter(Post.views == outer(User.posts.views)))).sql()
    with pytest.raises(QueryError, match="one column"):
        Post.objects.select(Post.id, Post.title).as_scalar()


# -- window functions ------------------------------------------------------------------------


async def test_window_functions(clean):
    await seed()
    rank = func.row_number().over(partition_by=Post.author_id, order_by=Post.views.desc())
    rows = await Post.objects.select(Post.title, rank.label("rank")).order_by(Post.title)
    assert [tuple(r) for r in rows] == [("a1", 3), ("a2", 1), ("a3", 2), ("b1", 1)]

    running = func.sum(Post.views).over(partition_by=Post.author_id, order_by=Post.created_at, rows=(None, 0))
    rows = await Post.objects.select(Post.title, running.label("total")).order_by(Post.created_at)
    assert [tuple(r) for r in rows] == [("b1", 100), ("a1", 5), ("a2", 55), ("a3", 75)]

    prev = func.lag(Post.views, 1, -1).over(partition_by=Post.author_id, order_by=Post.created_at)
    nxt = func.lead(Post.title).over(order_by=Post.created_at)
    rows = await Post.objects.filter(Post.author.name == "Alice").select(Post.title, prev.label("prev"), nxt.label("next")).order_by(Post.created_at)
    assert [tuple(r) for r in rows] == [("a1", -1, "a2"), ("a2", 5, "a3"), ("a3", 50, None)]

    rows = await Post.objects.select(
        Post.title,
        func.rank().over(order_by=Post.author_id).label("rank"),
        func.dense_rank().over(order_by=Post.author_id).label("dense"),
        func.ntile(2).over(order_by=Post.id).label("half"),
        func.count().over().label("n"),
        func.first_value(Post.title).over(partition_by=Post.author_id, order_by=Post.views).label("least"),
        func.percent_rank().over(order_by=Post.views).label("pct"),
    ).order_by(Post.id)
    assert [(r.rank, r.dense, r.half, r.n, r.least) for r in rows] == [
        (1, 1, 1, 4, "a1"), (1, 1, 1, 4, "a1"), (1, 1, 2, 4, "a1"), (4, 2, 2, 4, "b1")
    ]
    assert rows[3].pct == 1.0

    # Ordering by a window function; a frame by range.
    titles = await Post.objects.order_by(func.row_number().over(order_by=Post.views.desc())).select(Post.title).scalars()
    assert titles == ["b1", "a2", "a3", "a1"]
    rows = await Post.objects.select(Post.title, func.sum(Post.views).over(order_by=Post.views, range=(-20, 0)).label("s")).order_by(Post.views)
    assert [r.s for r in rows] == [5, 25, 50, 100]


async def test_window_function_errors(clean):
    rank = func.row_number().over(order_by=Post.views)
    with pytest.raises(QueryError, match="window functions can only be used in select"):
        await Post.objects.filter(rank <= 3)
    with pytest.raises(QueryError, match="window function: add .over"):
        await Post.objects.select(func.row_number())
    with pytest.raises(QueryError, match="window functions can only be used in select"):
        await Post.objects.update(views=rank)
    with pytest.raises(ValueError, match="rows or range"):
        func.count().over(rows=(None, 0), range=(None, 0))
    async with (await _db()).transaction():
        with pytest.raises(QueryError, match="window functions"):
            await Post.objects.lock().order_by(rank)


async def test_named_windows(clean):
    await seed()
    w = window(partition_by=Post.author_id, order_by=Post.created_at)
    q = Post.objects.select(
        Post.title,
        func.sum(Post.views).over(w).label("total"),
        func.avg(Post.views).over(w).label("avg"),
        func.row_number().over(w).label("n"),
    )
    assert q.sql().count("WINDOW") == 1 and 'OVER w1' in q.sql()
    rows = sorted(await q)
    assert [(r.title, r.total, r.n) for r in rows] == [("a1", 5, 1), ("a2", 55, 2), ("a3", 75, 3), ("b1", 100, 1)]
    assert rows[1].avg == 27.5
    # Extending a window with a frame: OVER (w1 ROWS ...).
    last2 = func.sum(Post.views).over(w, rows=(-1, 0))
    sql = Post.objects.select(Post.title, last2.label("s")).sql()
    assert "OVER (w1 ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)" in sql
    assert sorted(tuple(r) for r in await Post.objects.select(Post.title, last2.label("s"))) == [
        ("a1", 5), ("a2", 55), ("a3", 70), ("b1", 100)
    ]
    # ... and a window without order_by with one.
    part = window(partition_by=Post.author_id)
    rows = await Post.objects.select(Post.title, func.rank().over(part, order_by=Post.views.desc()).label("r"))
    assert sorted(tuple(r) for r in rows) == [("a1", 3), ("a2", 1), ("a3", 2), ("b1", 1)]
    # The same window in a subquery is declared there, not in the outer query.
    ranked = Post.objects.select(Post, func.row_number().over(window(Post.author_id, Post.views.desc())).label("rank")).cte("ranked")
    assert [p.title for p in await Post.objects.from_(ranked).filter(ranked.c.rank == 1)] in (["a2", "b1"], ["b1", "a2"])


async def test_named_window_limits(clean):
    w = window(partition_by=Post.author_id, order_by=Post.created_at)
    with pytest.raises(ValueError, match="partition_by"):
        func.sum(Post.views).over(w, partition_by=Post.id)
    with pytest.raises(ValueError, match="without its own order_by"):
        func.sum(Post.views).over(w, order_by=Post.id)
    with pytest.raises(ValueError, match="without its own frame"):
        func.sum(Post.views).over(window(rows=(None, 0)), rows=(None, 0))
    # Limits of the current SQL builder: one WINDOW per query, none with ORDER BY / LIMIT.
    other = window(order_by=Post.id)
    with pytest.raises(QueryError, match="only one named window"):
        await Post.objects.select(func.sum(Post.views).over(w), func.sum(Post.views).over(other))
    with pytest.raises(QueryError, match="order_by"):
        await Post.objects.select(func.sum(Post.views).over(w)).order_by(Post.id)
    with pytest.raises(QueryError, match="slicing"):
        await Post.objects.select(func.sum(Post.views).over(w))[:3]


async def test_join_cte(clean):
    (alice, bob, carol), _ = await seed()
    totals = (
        Post.objects.select(Post.author_id, func.sum(Post.views).label("views"), func.count().label("n"))
        .group_by(Post.author_id)
        .cte("totals")
    )
    rows = await User.objects.join(totals, totals.c.author_id == User.id).select(User, totals.c.views).order_by(totals.c.views.desc())
    assert [(u.name, v) for u, v in rows] == [("Bob", 100), ("Alice", 75)]
    rows = await User.objects.join(totals, totals.c.author_id == User.id, outer=True).select(User.name, totals.c.n).order_by(User.id)
    assert [tuple(r) for r in rows] == [("Alice", 3), ("Bob", 1), ("Carol", None)]
    # Filters, instances, count, prefetch.
    busy = User.objects.join(totals, totals.c.author_id == User.id).filter(totals.c.n > 1)
    assert [u.name for u in await busy] == ["Alice"] and await busy.count() == 1
    assert [len(u.posts.cached) for u in await busy.load(User.posts)] == [3]
    # Two CTEs joined.
    commenters = Comment.objects.select(Comment.author_id, func.count().label("c")).group_by(Comment.author_id).cte("commenters")
    rows = await (
        User.objects.join(totals, totals.c.author_id == User.id)
        .join(commenters, commenters.c.author_id == User.id)
        .select(User.name, totals.c.views, commenters.c.c)
        .order_by(User.name)
    )
    assert [tuple(r) for r in rows] == [("Alice", 75, 1), ("Bob", 100, 1)]
    with pytest.raises(QueryError, match="can't run on a query set with from_"):
        User.objects.join(totals, totals.c.author_id == User.id).update(name="x")
    with pytest.raises(ValueError, match="already read"):
        User.objects.join(totals, totals.c.author_id == User.id).join(totals, totals.c.n > 0)


async def test_recursive_cte_with_join(clean):
    users = await User.objects.insert_many([{"email": f"u{i}@x.io", "name": f"u{i}"} for i in range(5)]).returning()
    first = users[0].id
    walk = User.objects.filter(User.id == first).select(User.id, User.name, func.abs(User.id - User.id).label("depth")).cte(
        "walk",
        recursive=lambda w: User.objects.join(w, User.id == w.c.id + 1).filter(w.c.depth < 2).select(User.id, User.name, w.c.depth + 1),
    )
    assert "JOIN \"walk\" ON" in walk.select(walk.c.name).sql()
    rows = await walk.select(walk.c.name, walk.c.depth).order_by(walk.c.depth)
    assert [tuple(r) for r in rows] == [("u0", 0), ("u1", 1), ("u2", 2)]


async def _db():
    return orm.get_database()


# -- CTEs --------------------------------------------------------------------------------------


async def test_cte_from_filters_window(clean):
    await seed()
    rank = func.row_number().over(partition_by=Post.author_id, order_by=Post.views.desc())
    ranked = Post.objects.select(Post, rank.label("rank")).cte("ranked")
    top = await Post.objects.from_(ranked).filter(ranked.c.rank <= 2).order_by(Post.title)
    assert [p.title for p in top] == ["a2", "a3", "b1"]
    assert isinstance(top[0], Post) and top[0].author_id is not None
    # Relation filters, joined and prefetched loads still work on rows read from a CTE.
    top = await Post.objects.from_(ranked).filter(ranked.c.rank == 1, Post.author.name == "Alice").load(Post.author)
    assert [(p.title, p.author.name) for p in top] == [("a2", "Alice")]
    top = await Post.objects.from_(ranked).filter(ranked.c.rank == 1).load(Post.comments).order_by(Post.title)
    assert [len(p.comments.cached) for p in top] == [2, 1]
    assert await Post.objects.from_(ranked).filter(ranked.c.rank == 1).count() == 2
    assert await Post.objects.from_(ranked).filter(ranked.c.rank == 9).exists() is False
    rows = await Post.objects.from_(ranked).select(Post.title, ranked.c.rank).order_by(Post.title)
    assert [tuple(r) for r in rows] == [("a1", 3), ("a2", 1), ("a3", 2), ("b1", 1)]


async def test_cte_select_and_subqueries(clean):
    (alice, bob, carol), _ = await seed()
    totals = (
        Post.objects.select(Post.author_id, func.sum(Post.views).label("views"), func.count().label("n"))
        .group_by(Post.author_id)
        .cte("totals")
    )
    rows = await totals.select(totals.c.author_id, totals.c.views).filter(totals.c.n > 1)
    assert [tuple(r) for r in rows] == [(alice.id, 75)]
    assert [tuple(r) for r in await totals.select(func.max(totals.c.views).label("best"))] == [(100,)]
    # A CTE read from a subquery: declared on the statement, once.
    heavy = User.objects.filter(User.id.in_(totals.select(totals.c.author_id).filter(totals.c.views > 80)))
    result = heavy.inspect()
    assert result.statement_count == 1
    assert not result.has_batching
    assert result.statements[0].joins_count == 0
    assert result.statements[0].correlated_count == 0
    assert result.statements[0].uncorrelated_count == 1
    assert result.statements[0].cte_count == 1
    assert [u.name for u in await heavy] == ["Bob"]
    assert heavy.sql().count("WITH") == 1
    big = exists(totals.select(totals.c.n).filter(totals.c.author_id == outer(User.id), totals.c.views > 50))
    assert [u.name for u in await User.objects.filter(big).order_by(User.id)] == ["Alice", "Bob"]
    # ... in writes too.
    n = await User.objects.filter(User.id.in_(totals.select(totals.c.author_id))).update(name="writer")
    assert n == 2
    assert await User.objects.filter(User.name == "writer").count() == 2
    # CTEs reading CTEs, materialized.
    top = totals.select(totals.c.author_id).filter(totals.c.views > 80).cte("top", materialized=True)
    assert "MATERIALIZED" in User.objects.filter(User.id.in_(top.select(top.c.author_id))).sql()
    assert [u.id for u in await User.objects.filter(User.id.in_(top.select(top.c.author_id)))] == [bob.id]
    assert await Post.objects.filter(Post.author_id.in_(totals.select(totals.c.author_id))).delete() == 4


async def test_recursive_cte(clean):
    users = await User.objects.insert_many([{"email": f"u{i}@x.io", "name": f"u{i}"} for i in range(5)]).returning()
    first = users[0].id
    chain = User.objects.filter(User.id == first).cte(
        "chain", recursive=lambda c: User.objects.filter(User.id == c.c.id + 1, User.id < first + 3)
    )
    assert [u.name for u in await User.objects.from_(chain).order_by(User.id)] == ["u0", "u1", "u2"]
    # A select() CTE with a depth column.
    walk = User.objects.filter(User.id == first).select(User.id, func.abs(User.id - User.id).label("depth")).cte(
        "walk",
        recursive=lambda w: User.objects.filter(User.id == w.c.id + 1).select(User.id, w.c.depth + 1),
    )
    assert await walk.select(walk.c.depth).order_by(walk.c.depth.desc()).limit(1).scalars() == [4]


async def test_cte_errors(clean):
    totals = Post.objects.select(Post.author_id, func.count().label("n")).group_by(Post.author_id).cte("totals")
    with pytest.raises(TypeError, match="columns of Post"):
        Post.objects.from_(totals)
    with pytest.raises(AttributeError, match="no column 'nope'"):
        totals.c.nope
    with pytest.raises(QueryError, match="only available in queries reading totals"):
        await Post.objects.filter(totals.c.n > 1)
    with pytest.raises(QueryError, match="can't run on a query set with from_"):
        await Post.objects.from_(Post.objects.cte("p")).delete()
    other = Post.objects.select(Post.author_id).cte("totals")
    with pytest.raises(ValueError, match="two different CTEs"):
        User.objects.filter(User.id.in_(totals.select(totals.c.author_id)), User.id.in_(other.select(other.c.author_id))).sql()
    with pytest.raises(ValueError, match="several columns named id"):
        Post.objects.select(Post, Post.id.label("id")).cte("x")


# -- prefetch -----------------------------------------------------------------------------------


async def test_nested_prefetch(clean, observe_queries):
    (alice, bob, carol), _ = await seed()
    query = User.objects.load(User.posts.comments, User.comments).order_by(User.id)
    result = query.inspect()
    assert result.statement_count == 4  # root, posts, posts' comments, users' comments
    assert result.has_batching
    assert [s.has_batching for s in result.statements] == [False, True, True, True]
    assert all(s.joins_count == 0 for s in result.statements)
    assert all((s.correlated_count, s.uncorrelated_count) == (0, 0) for s in result.statements)
    with observe_queries(clean) as events:
        users = await query
        assert len(events) == 4  # independent of the number of parents
    a, b, c = users
    assert [p.title for p in a.posts.cached] == ["a1", "a2", "a3"]
    assert [[x.body for x in p.comments.cached] for p in a.posts.cached] == [[], ["c1", "c2"], []]
    assert a.posts.cached[1].comments.cached[0].post is a.posts.cached[1]  # back reference
    assert a.posts.cached[0].author is a
    assert [x.body for x in b.comments.cached] == ["c1"] and c.posts.cached == []


async def test_to_one_prefetch(clean, observe_queries):
    await seed()
    to_one = Comment.post.objects.as_prefetch().load(Post.author.objects.as_prefetch())
    query = Comment.objects.load(to_one, Comment.author.objects.as_prefetch()).order_by(Comment.id)
    result = query.inspect()
    assert result.statement_count == 4
    assert result.has_batching
    assert [s.has_batching for s in result.statements] == [False, True, True, True]
    assert all(s.joins_count == 0 for s in result.statements)
    assert all((s.correlated_count, s.uncorrelated_count) == (0, 0) for s in result.statements)
    with observe_queries(clean) as events:
        comments = await query
        assert len(events) == 4
    assert [(x.post.title, x.post.author.name) for x in comments] == [("a2", "Alice"), ("a2", "Alice"), ("b1", "Bob")]
    assert comments[0].post is comments[1].post  # one object per related row
    assert [x.author.name if x.author else None for x in comments] == ["Bob", None, "Alice"]


async def test_filtered_prefetch(clean, observe_queries):
    (alice, bob, carol), _ = await seed()
    query = User.objects.load(
        User.posts.objects.filter(Post.views >= 20).order_by(Post.views.desc())
    ).order_by(User.id)
    assert query.inspect() == orm.QueryInspection(2, True, (
        orm.StatementInspection(False, 0, 0, 0, 0),
        orm.StatementInspection(True, 0, 0, 0, 0),
    ))
    with observe_queries(clean) as events:
        users = await query
    assert len(events) == 2
    # Like Django: the filtered rows are what user.posts holds now.
    assert [p.title for p in users[0].posts.cached] == ["a2", "a3"]
    assert [p.title for p in await users[0].posts] == ["a2", "a3"]
    assert await users[0].posts.count() == 3  # a new query sees every post
    users = await User.objects.load(
        User.posts.objects.filter(Post.views >= 20).label("popular"),
    ).order_by(User.id)
    assert [p.title for p in users[0].popular] == ["a2", "a3"] and users[2].popular == []
    with pytest.raises(orm.NotLoaded):
        users[0].posts.cached


async def test_sliced_prefetch_is_per_parent(clean, observe_queries):
    await seed()
    top = User.posts.objects.order_by(Post.views.desc()).load(Post.comments)[:2]
    query = User.objects.load(top.label("top")).order_by(User.id)
    result = query.inspect()
    assert result.statement_count == 3
    assert result.has_batching
    assert [s.has_batching for s in result.statements] == [False, True, True]
    assert all(s.joins_count == 0 for s in result.statements)
    # Per-parent slicing uses one windowed subquery, not a query for each user.
    assert [(s.correlated_count, s.uncorrelated_count) for s in result.statements] == [
        (0, 0),
        (0, 1),
        (0, 0),
    ]
    with observe_queries(clean) as events:
        users = await query
        assert len(events) == 3
    assert [[p.title for p in u.top] for u in users] == [["a2", "a3"], ["b1"], []]
    assert [len(p.comments.cached) for p in users[0].top] == [2, 0]
    second = User.posts.objects.order_by(Post.created_at, Post.author.name)[1:2]
    users = await User.objects.load(second).order_by(User.id)
    assert [[p.title for p in u.posts.cached] for u in users] == [["a2"], [], []]


async def test_relation_query_set_with_joins_and_nested_query_set(clean, observe_queries):
    await seed()
    query = User.objects.load(
        User.comments.objects.load(Comment.post.author),
        User.posts.objects.load(Post.comments.objects.filter(Comment.author_id.is_null())),
    ).order_by(User.id)
    result = query.inspect()
    assert result.statement_count == 4
    assert result.has_batching
    assert [s.joins_count for s in result.statements] == [0, 2, 0, 0]
    assert all((s.correlated_count, s.uncorrelated_count) == (0, 0) for s in result.statements)
    with observe_queries(clean) as events:
        users = await query
        assert len(events) == 4
    assert [(x.post.title, x.post.author.name) for x in users[1].comments.cached] == [("a2", "Alice")]
    assert [[x.body for x in p.comments.cached] for p in users[0].posts.cached] == [[], ["c2"], []]


async def test_prefetch_errors(clean):
    with pytest.raises(ValueError, match="different query sets"):
        User.objects.load(User.posts.objects.filter(Post.views > 1), User.posts.objects.filter(Post.views > 2))
    with pytest.raises(TypeError, match="relation query set"):
        User.objects.load(Post.objects.all())
    with pytest.raises(TypeError, match="relation query set"):
        Post.objects.label("x")
    with pytest.raises(ValueError, match="does not start at User"):
        User.objects.load(Post.comments)


# -- instances built in Rust -----------------------------------------------------------------------


async def test_instances_get_their_database(clean):
    await seed()
    db = orm.get_database()
    users = await User.objects.using(db).load(User.posts).load()
    assert all(u.__dict__["_db"] is db for u in users)
    assert all(p.__dict__["_db"] is db for u in users for p in u.posts.cached)
    rows = await User.objects.using(db).select(User, User.name)
    assert rows[0][0].__dict__["_db"] is db
    new = await User.objects.using(db).insert(email="z@x.io", name="Z")
    assert new.__dict__["_db"] is db
    assert "_db" not in (await User.objects.first()).__dict__


async def test_prefetch_query_reading_a_cte(clean, observe_queries):
    await seed()
    best = Post.objects.select(Post.id).filter(Post.views >= 50).cte("best")
    query = User.objects.load(
        User.posts.objects.filter(Post.id.in_(best.select(best.c.id))).label("best")
    ).order_by(User.id)
    assert query.inspect() == orm.QueryInspection(2, True, (
        orm.StatementInspection(False, 0, 0, 0, 0),
        orm.StatementInspection(True, 0, 0, 1, 1),
    ))
    with observe_queries(clean) as events:
        users = await query
    assert len(events) == 2
    assert [[p.title for p in u.best] for u in users] == [["a2"], ["b1"], []]


async def test_correlated_in_subquery(clean):
    await seed()
    commented = Comment.objects.filter(Comment.author_id == outer(User.id)).select(Comment.post_id)
    query = User.objects.filter(exists(Post.objects.filter(Post.id.in_(commented)))).order_by(User.id)
    assert query.inspect() == orm.QueryInspection(1, False, (
        orm.StatementInspection(False, 0, 2, 0, 0),
    ))
    rows = await query
    assert [u.name for u in rows] == ["Alice", "Bob"]


# -- prefetch over many parents: keys split across queries ----------------------------------------


@pytest.fixture
async def small_params(clean):
    """A second connection whose statements take at most 5 parameters, so prefetches
    split their keys after a few parents."""
    db = await orm.connect(DATABASE_URL, max_connections=2, default=False, _disable=("max_params=5",))
    yield db
    await db.close()


async def test_max_params_option_is_checked(clean):
    with pytest.raises(orm.QueryError, match="invalid"):
        await orm.connect(DATABASE_URL, default=False, _disable=("max_params=abc",))


async def test_prefetch_splits_keys(small_params, observe_queries):
    db = small_params
    users = await User.objects.insert_many([{"email": f"u{i}@x.io", "name": f"u{i}"} for i in range(12)]).returning()
    posts = await Post.objects.insert_many(
        [{"author": u, "title": f"{u.name}-{j}", "body": "", "views": j} for u in users for j in range(3)]
    ).returning()
    await Comment.objects.insert_many([{"post": p, "body": f"on {p.title}"} for p in posts[::2]])

    def shape(us):
        return [[(p.title, [c.body for c in p.comments.cached]) for p in u.posts.cached] for u in us]

    qs = User.objects.load(User.posts.comments).order_by(User.id)
    query = qs.using(db)
    result = query.inspect()
    assert result.statement_count == 3
    assert result.has_batching
    assert [s.has_batching for s in result.statements] == [False, True, True]
    assert all(s.joins_count == 0 for s in result.statements)
    with observe_queries(db) as events:
        got = await query
    # 12 user keys in batches of 5, then 36 post keys in batches of 5.
    assert len(events) == 1 + 3 + 8
    assert shape(got) == shape(await qs)
    assert sum(len(u.posts.cached) for u in got) == 36

    # A slice per parent still holds: each parent's rows stay in one query.
    top = User.posts.objects.filter(Post.views >= 0).order_by(Post.views.desc())[:2].label("top")
    query = User.objects.load(top).order_by(User.id).using(db)
    assert query.inspect() == orm.QueryInspection(2, True, (
        orm.StatementInspection(False, 0, 0, 0, 0),
        orm.StatementInspection(True, 0, 0, 1, 0),
    ))
    with observe_queries(db) as events:
        got = await query
    # Filter and limit reserve 2 parameters: 3 parent keys per batch.
    assert len(events) == 1 + 4
    assert [[p.views for p in u.top] for u in got] == [[2, 1]] * 12

    # To-one, with repeated keys.
    to_one = Comment.post.objects.as_prefetch().load(Post.author.objects.as_prefetch())
    query = Comment.objects.load(to_one).order_by(Comment.id).using(db)
    assert query.inspect().statement_count == 3
    assert query.inspect().has_batching
    assert all(s.joins_count == 0 for s in query.inspect().statements)
    with observe_queries(db) as events:
        cs = await query
    assert len(events) == 1 + 4 + 3  # 18 distinct post keys, then 12 author keys
    names = {u.id: u.name for u in users}
    assert [c.post.author.name for c in cs] == [names[p.author_id] for p in posts[::2]]


async def test_prefetch_beyond_the_parameter_limit(clean, observe_queries):
    # 70 000 parents: more keys than Postgres takes parameters in one statement.
    db = orm.get_database()
    await db.execute("INSERT INTO users (email, name) SELECT 'u' || g || '@x.io', 'u' FROM generate_series(1, 70000) g")
    await db.execute("INSERT INTO posts (author_id, title, body) SELECT id, 't', '' FROM users WHERE id % 10000 = 0")
    query = User.objects.load(User.posts)
    result = query.inspect()
    assert result.statement_count == 2
    assert result.has_batching
    assert [s.has_batching for s in result.statements] == [False, True]
    assert all(s.joins_count == 0 for s in result.statements)
    with observe_queries(db) as events:
        users = await query
    assert len(events) == 3  # root + two key batches, rather than 70,000 queries
    assert len(users) == 70000
    assert sum(len(u.posts.cached) for u in users) == 7


@pytest.mark.parametrize("operation", [
    "await", "async_for", "iterate", "batches", "paginate", "first", "last", "get",
    "count", "exists", "in_bulk", "in_bulk_empty", "get_or_insert", "prepare",
    "update", "update_many", "delete", "insert", "insert_many", "copy", "select",
    "cte", "subquery", "explain", "sql", "inspect",
])
async def test_bound_query_refuses_standalone(clean, operation):
    qs = User.posts.objects.filter(Post.views >= 0)
    with pytest.raises(TypeError, match="inside load"):
        if operation == "await":
            await qs
        elif operation == "async_for":
            _ = [row async for row in qs]
        elif operation in ("iterate", "batches"):
            _ = [row async for row in getattr(qs, operation)()]
        elif operation == "paginate":
            await qs.paginate(first=1)
        elif operation == "get_or_insert":
            await qs.get_or_insert(id=1)
        elif operation == "in_bulk_empty":
            await qs.in_bulk([])
        elif operation == "update":
            await qs.update(title="blocked")
        elif operation == "update_many":
            await qs.update_many([])
        elif operation == "insert":
            await qs.insert()
        elif operation in ("insert_many", "copy"):
            await qs.insert_many([], copy=operation == "copy")
        elif operation == "select":
            await qs.select(Post.id)
        elif operation == "cte":
            qs.cte("bound")
        elif operation == "subquery":
            await User.objects.filter(exists(qs))
        elif operation in ("prepare", "sql", "inspect"):
            getattr(qs, operation)()
        else:
            await getattr(qs, operation)()


@pytest.mark.parametrize("change", ["filter", "order", "limit", "offset", "defaults"])
@pytest.mark.parametrize("nested", [False, True])
@pytest.mark.parametrize("reverse", [False, True])
def test_load_path_query_collision(change, nested, reverse):
    qs = {
        "filter": User.posts.objects.filter(Post.views > 10),
        "order": User.posts.objects.order_by(Post.id),
        "limit": User.posts.objects[:1],
        "offset": User.posts.objects[1:],
        "defaults": User.posts.objects.without_defaults(),
    }[change]
    path = User.posts.comments if nested else User.posts
    items = (qs, path) if reverse else (path, qs)
    with pytest.raises(ValueError, match=r"label\(\)"):
        User.objects.load(*items)
    with pytest.raises(ValueError, match=r"label\(\)"):
        User.objects.load(items[0]).load(items[1])
    # Separate labels intentionally request different sets of rows.
    User.objects.load(path, qs.label("chosen"))


@pytest.mark.parametrize("reverse", [False, True])
async def test_load_set_with_columns_keeps_sibling_columns(clean, reverse):
    await seed()
    for qs, column in (
        (User.posts.objects.load(Post.title), User.posts.views),
    ):
        items = (column, qs) if reverse else (qs, column)
        users = await User.objects.load(*items).order_by(User.id)
        post = users[0].posts.cached[0]
        assert (post.title, post.views) == ("a1", 5)
        with pytest.raises(orm.NotLoaded):
            _ = post.body
    items = (Post.author.email, Post.author.objects.load(User.name))
    posts = await Post.objects.load(*(items if reverse else items[::-1])).order_by(Post.id)
    assert (posts[0].author.name, posts[0].author.email) == ("Alice", "alice@example.com")
    two = (User.posts.objects.load(Post.title), User.posts.views, User.posts.body)
    users = await User.objects.load(*(two[::-1] if reverse else two)).order_by(User.id)
    assert (users[0].posts.cached[0].views, users[0].posts.cached[0].body) == (5, "")


@pytest.mark.parametrize("reverse", [False, True])
async def test_load_bare_path_and_set_with_columns_loads_all_fields(clean, reverse):
    await seed()
    items = (User.posts, User.posts.objects.load(Post.title))
    users = await User.objects.load(*(items[::-1] if reverse else items)).order_by(User.id)
    post = users[0].posts.cached[0]
    assert (post.title, post.views, post.body) == ("a1", 5, "")


@pytest.mark.parametrize("change", ["filter", "order", "limit", "offset", "defaults"])
@pytest.mark.parametrize("column", ["first", "deep"])
@pytest.mark.parametrize("reverse", [False, True])
def test_load_row_changing_set_refuses_columns(change, column, reverse):
    qs = {
        "filter": User.posts.objects.filter(Post.published),
        "order": User.posts.objects.order_by(Post.id),
        "limit": User.posts.objects[:1],
        "offset": User.posts.objects[1:],
        "defaults": User.posts.objects.without_defaults(),
    }[change]
    col = User.posts.title if column == "first" else User.posts.comments.body
    items = (col, qs) if reverse else (qs, col)
    with pytest.raises(ValueError, match=r"\.load\(\).*label\(\)"):
        User.objects.load(*items)
    with pytest.raises(ValueError, match=r"label\(\)"):
        User.objects.load(items[0]).load(items[1])
    User.objects.load(col, qs.label("chosen"))
    User.objects.load(qs.load(Post.title))


async def test_load_set_and_column_guards(clean):
    await seed()
    users = await User.objects.load(User.posts.objects, User.posts.views).order_by(User.id)
    assert users[0].posts.cached[0].body == ""
    users = await User.objects.load(User.posts.objects.load(), User.posts.views).order_by(User.id)
    assert users[0].posts.cached[0].body == ""
    users = await User.objects.load(User.posts, User.posts.views).order_by(User.id)
    assert users[0].posts.cached[0].body == ""
    labeled = User.posts.objects.filter(Post.views > 10).label("hot")
    users = await User.objects.load(labeled, User.posts.views).order_by(User.id)
    assert users[0].hot[0].views == 50


async def test_load_the_same_plain_set_twice(clean):
    await seed()
    for load in (
        lambda: User.objects.load(User.posts.objects, User.posts.objects),
        lambda: User.objects.load(User.posts.objects).load(User.posts.objects),
    ):
        users = await load().order_by(User.id)
        assert [len(u.posts.cached) for u in users] == [3, 1, 0]
    with pytest.raises(ValueError, match="loaded twice"):
        User.objects.load(User.posts.objects.load(Post.comments), User.posts.objects.load(Post.comments))
    with pytest.raises(ValueError, match="loaded twice"):
        User.objects.load(User.posts.objects, User.posts.objects.filter(Post.views > 10))
    with pytest.raises(ValueError, match="loaded twice"):
        User.objects.load(User.posts.objects).load(User.posts.objects.filter(Post.views > 10))
    same = User.posts.objects.filter(Post.views > 10)
    users = await User.objects.load(same, same).order_by(User.id)
    assert [len(u.posts.cached) for u in users] == [2, 1, 0]


async def test_load_plain_path_and_query_and_without_related(clean):
    await seed()
    for qs in (User.posts.objects, User.posts.objects.load(Post.comments)):
        users = await User.objects.load(User.posts, qs).order_by(User.id)
        assert [len(u.posts.cached) for u in users] == [3, 1, 0]
    query = Post.objects.load(Post.title, Post.author, Post.comments).without_related().order_by(Post.id)
    posts = await query
    assert [p.title for p in posts] == ["a1", "a2", "a3", "b1"]
    assert [len(p.comments.cached) for p in posts] == [0, 2, 0, 1]
    with pytest.raises(orm.NotLoaded):
        _ = posts[0].author
    with pytest.raises(orm.NotLoaded):
        _ = posts[0].body
    # Both uses of the same bound set still work after a rejected standalone run.
    bound = User.posts.objects.load(Post.comments)
    with pytest.raises(TypeError):
        await bound
    users = await User.objects.order_by(User.id)
    await orm.prefetch(users, User.posts, bound)
    assert [len(u.posts.cached) for u in users] == [3, 1, 0]


async def test_load_without_items_resets_the_columns(clean):
    await seed()
    posts = await Post.objects.load(Post.title).load().order_by(Post.id)
    assert posts[0].body == ""
    posts = await Post.objects.load(Post.title).load(Post.views).order_by(Post.id)
    assert (posts[0].title, posts[0].views) == ("a1", 5)
    with pytest.raises(orm.NotLoaded):
        _ = posts[0].body
    posts = await Post.objects.load(Post.author, Post.title).load().order_by(Post.id)
    assert posts[0].author.name == "Alice"
    assert posts[0].body == ""


async def test_load_to_one_query_sets(clean, observe_queries):
    await seed()
    filtered = Comment.post.objects.filter(Post.views > 60).as_prefetch()
    with observe_queries(clean) as events:
        comments = await Comment.objects.load(filtered).order_by(Comment.id)
        assert len(events) == 2
    assert comments[2].post.title == "b1"
    partial = await Comment.objects.load(Comment.post.objects.load(Post.title)).order_by(Comment.id)
    assert partial[0].post.title == "a2"
    with pytest.raises(orm.NotLoaded):
        _ = partial[0].post.body
    joined = Comment.objects.load(Comment.post.objects).order_by(Comment.id)
    assert joined.inspect().statement_count == 1
    with observe_queries(clean) as events:
        comments = await joined
        assert len(events) == 1
    assert comments[0].post.title == "a2"


async def test_load_set_of_another_model_raises(clean):
    with pytest.raises(TypeError):
        User.objects.load(Comment.objects.all())
    with pytest.raises(ValueError, match="Post.comments does not start at User"):
        User.objects.load(Post.comments.objects)
