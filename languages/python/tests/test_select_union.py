"""`select(...).union(other)` and `.union_all(other)`: one statement, one `CompoundSelect`."""

import os
from pathlib import Path

import pytest
from blog.models import Comment, Post, Profile, Tag, User

import monorm
from monorm import CompoundSelect, DoesNotExist, MultipleObjectsReturned, QueryError, func

SQLITE_SOURCE = Path(__file__).resolve().parents[3].joinpath("examples/sqlite/schema.prisma").read_text()


async def seed():
    alice, bob, carol = await User.objects.insert_many(
        [{"email": "a@example.com", "name": "Alice"}, {"email": "b@example.com", "name": "Bob"}, {"email": "c@example.com", "name": "Carol"}]
    ).returning()
    posts = await Post.objects.insert_many(
        [
            {"author": alice, "title": "a1", "body": "", "views": 80},
            {"author": alice, "title": "a2", "body": "", "views": 40},
            {"author": bob, "title": "b1", "body": "", "views": 5},
        ]
    ).returning()
    return (alice, bob, carol), posts


def names(*, low: int, high: int):
    return User.objects.select(User.id, User.name.label("label")).filter(User.id >= low, User.id <= high)


def titles():
    return Post.objects.select(Post.id, Post.title.label("label"))


async def test_union_deduplicates(clean):
    await seed()
    left, right = names(low=1, high=2), names(low=2, high=3)
    rows = await left.union(right).order_by("id")
    assert [tuple(r) for r in rows] == [(1, "Alice"), (2, "Bob"), (3, "Carol")]


async def test_union_all_keeps_duplicates(clean):
    await seed()
    left, right = names(low=1, high=2), names(low=2, high=3)
    rows = await left.union_all(right).order_by("id")
    assert [tuple(r) for r in rows] == [(1, "Alice"), (2, "Bob"), (2, "Bob"), (3, "Carol")]
    assert await left.union_all(left).count() == 4


async def test_null_and_empty_branches(clean):
    (alice, *_), posts = await seed()
    await Comment.objects.insert_many(
        [{"post": posts[0], "body": "x", "author": None}, {"post": posts[0], "body": "y", "author": None}, {"post": posts[0], "body": "z", "author": alice}]
    )
    authors = Comment.objects.select(Comment.author_id)
    assert len(await authors.union(authors)) == 2
    assert len(await authors.union_all(authors)) == 6
    none = Comment.objects.select(Comment.author_id).filter(Comment.body == "nope")
    assert len(await none.union_all(authors)) == 3
    assert len(await authors.union_all(none)) == 3
    assert await none.union(none) == []
    assert await none.union(none).count() == 0
    assert not await none.union(none).exists()


async def test_scalar_cross_model(clean):
    await seed()
    rows = await names(low=1, high=1).union_all(titles().filter(Post.title == "b1")).order_by("label")
    assert [(r.id, r.label) for r in rows] == [(1, "Alice"), (3, "b1")]
    assert isinstance(names(low=1, high=1).union(titles()), CompoundSelect)


async def test_shape_validation(clean):
    await seed()
    with pytest.raises(QueryError, match="same columns"):
        names(low=1, high=2).union(User.objects.select(User.id))
    with pytest.raises(QueryError, match="same columns"):
        names(low=1, high=2).union(User.objects.select(User.id, User.name.label("other")))
    with pytest.raises(QueryError, match="same columns"):
        names(low=1, high=2).union(User.objects.select(User.name.label("label"), User.id))
    with pytest.raises(TypeError):
        names(low=1, high=2).union([1, 2])  # type: ignore[call-overload]
    with pytest.raises(QueryError):
        await User.objects.select(User.name.label("v")).union(User.objects.select(User.id.label("v")))


async def test_nullable_result(clean):
    (alice, *_), posts = await seed()
    await Comment.objects.insert_many([{"post": posts[0], "body": "x", "author": None}, {"post": posts[0], "body": "y", "author": alice}])
    rows = await Comment.objects.select(Comment.author_id.label("id")).union_all(User.objects.select(User.id)).order_by("id")
    assert sorted(((r.id is None), r.id or 0) for r in rows) == [(False, 1), (False, 1), (False, 2), (False, 3), (True, 0)]


async def test_enum_and_storage_mismatch(clean):
    await seed()
    with pytest.raises(QueryError):
        await Profile.objects.select(Profile.role.label("v")).union(User.objects.select(User.name.label("v")))
    with pytest.raises(QueryError):
        await Tag.objects.select(Tag.priority.label("v")).union(Post.objects.select(Post.views.label("v")))


async def test_branch_slice_and_final_slice(clean):
    await seed()
    left = names(low=1, high=3).order_by(User.id.desc())[:2]
    right = titles().order_by(Post.id)[:1]
    rows = await left.union_all(right).order_by("id")
    assert [tuple(r) for r in rows] == [(1, "a1"), (2, "Bob"), (3, "Carol")]
    assert [r.id for r in await left.union_all(right).order_by("id")[1:3]] == [2, 3]
    assert [r.id for r in await left.union_all(right).order_by("id").offset(2)] == [3]
    assert await left.union_all(right).order_by("id").limit(0) == []
    assert await left.union_all(right).count() == 3
    with pytest.raises(ValueError):
        left.union_all(right)[-1:]
    with pytest.raises(QueryError):
        left.union_all(right).limit(1).limit(2)


async def test_nested_compound_grouping(clean):
    await seed()
    a, b, c = names(low=1, high=2), names(low=2, high=3), names(low=3, high=3)
    left_group = await a.union_all(b).union(c)
    right_group = await a.union_all(b.union(c))
    assert sorted(r.id for r in left_group) == [1, 2, 3]
    assert sorted(r.id for r in right_group) == [1, 2, 2, 3]
    assert sorted(r.id for r in await a.union(b).union_all(c)) == [1, 2, 3, 3]


async def test_final_order_alias(clean):
    await seed()
    q = names(low=1, high=2).union(titles().filter(Post.title == "b1"))
    assert [r.id for r in await q.order_by("-id", "label")] == [3, 2, 1]
    assert [r.label for r in await q.order_by("id")] == ["Alice", "Bob", "b1"]
    for bad in ("nope", 'x"; DROP TABLE users; --', ""):
        with pytest.raises((QueryError, TypeError)):
            await q.order_by(bad)
    with pytest.raises(TypeError):
        q.order_by(User.id)  # type: ignore[arg-type]


async def test_branch_defaults_and_scope(clean):
    await seed()
    left = User.objects.select(User.id, User.name.label("label")).filter(User.name != "Bob")
    right = User.objects.select(User.id, User.name.label("label")).exclude(User.name != "Bob")
    assert sorted(r.label for r in await left.union_all(right)) == ["Alice", "Bob", "Carol"]
    assert await left.union_all(right).count() == 3


async def test_unsupported_model_shapes(clean):
    await seed()
    assert not hasattr(User.objects, "union")
    with pytest.raises(TypeError):
        names(low=1, high=2).union(User.objects.filter(User.id == 1))  # type: ignore[call-overload]
    with pytest.raises(QueryError):
        names(low=1, high=2).union(User.objects.select(User))
    with pytest.raises(QueryError):
        User.objects.lock().select(User.id, User.name.label("label")).union(names(low=1, high=2))


async def test_parameters_across_branches(clean):
    await seed()
    left = User.objects.select(User.id, User.name.label("label")).filter(User.name == "Alice")
    right = User.objects.select(User.id, User.name.label("label")).filter(User.name == "Carol")
    mid = titles().filter(Post.title == "b1")
    rows = await left.union_all(mid).union_all(right).order_by("id")[1:3]
    assert [tuple(r) for r in rows] == [(3, "b1"), (3, "Carol")]
    paged = await left.union_all(mid).union_all(right).order_by("id", "label").limit(3).offset(1)
    assert [tuple(r) for r in paged] == [(3, "b1"), (3, "Carol")]
    assert {r.label for r in await left.union(right)} == {"Alice", "Carol"}


async def test_database_mismatch(clean, db):
    await seed()
    other = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], default=False)
    try:
        with pytest.raises(QueryError, match="same database"):
            names(low=1, high=2).using(other).union(names(low=2, high=3).using(db))
        assert len(await names(low=1, high=2).using(other).union(names(low=2, high=3).using(other))) == 3
        with pytest.raises(QueryError, match="same database"):
            await names(low=1, high=2).using(other).union(names(low=2, high=3))
    finally:
        await other.close()


async def test_transaction_read(clean, db):
    await seed()
    async with db.transaction():
        await User.objects.insert(email="t@example.com", name="Tess")
        rows = await names(low=1, high=10).union(titles().filter(Post.title == "b1")).order_by("-id")
        assert rows[0].label == "Tess"
    assert await names(low=1, high=10).union(titles()).exists()


async def test_lazy_cache_and_all(clean, db, observe_queries):
    await seed()
    with observe_queries(db) as events:
        q = names(low=1, high=10).union(titles()).order_by("id")
        q2 = q.limit(1)
        assert q is not q2 and not events
        first = await q
        assert await q == first and len(events) == 1
        await User.objects.insert(email="n@example.com", name="Nina")
        assert len(await q.all()) == len(first) + 1
        assert len(await q) == len(first)


async def test_terminals_after_slice(clean):
    await seed()
    q = names(low=1, high=3).union(titles().filter(Post.title == "b1")).order_by("id")
    assert await q[:2].count() == 2
    assert await q[:2].exists()
    assert not await q[10:].exists()
    assert (await q.first()).label == "Alice"
    assert (await q.require().first()).id == 1
    with pytest.raises(MultipleObjectsReturned):
        await q.get()
    assert (await names(low=1, high=1).union(names(low=1, high=1)).get()).label == "Alice"
    assert await names(low=9, high=9).union(names(low=9, high=9)).get() is None
    with pytest.raises(DoesNotExist):
        await names(low=9, high=9).union(names(low=9, high=9)).require().first()
    with pytest.raises(DoesNotExist):
        await names(low=9, high=9).union(names(low=9, high=9)).require()
    with pytest.raises(QueryError):
        await q[:2].first()
    ids = User.objects.select(User.id)
    assert sorted(await ids.union(ids).scalars()) == [1, 2, 3]
    assert await ids.union(ids).order_by("id").scalar() == 1
    with pytest.raises(QueryError):
        await q.scalar()


async def test_unsupported_chaining(clean):
    await seed()
    q = names(low=1, high=2).union(names(low=2, high=3))
    for name in ("filter", "exclude", "select", "group_by", "having", "join", "cte", "lock", "delete", "update", "prepare", "paginate", "batches", "distinct"):
        assert not hasattr(q, name), name
    with pytest.raises(QueryError):
        names(low=1, high=2).require().union(names(low=2, high=3))
    with pytest.raises(QueryError):
        names(low=1, high=2).union(names(low=2, high=3).require())
    totals = Post.objects.select(Post.author_id, func.count().label("n")).group_by(Post.author_id).cte("totals")
    joined = User.objects.select(User.id, totals.c.n).join(totals, totals.c.author_id == User.id)
    with pytest.raises(QueryError, match="CTE"):
        joined.union(User.objects.select(User.id, func.count().label("n")).group_by(User.id))
    with pytest.raises(TypeError):
        for _ in q:  # type: ignore[attr-defined]
            pass


async def test_inspection_and_explain(clean, db):
    await seed()
    q = names(low=1, high=2).union_all(names(low=2, high=3)).order_by("-id").limit(2)
    sql = q.sql().upper()
    assert sql.count("UNION") == 1 and "ALL" in sql
    inspection = q.inspect()
    assert inspection.statement_count == 1 and len(inspection.statements) == 1
    assert "union" in (await q.explain()).lower() or "append" in (await q.explain()).lower()
    assert [r.id for r in await q] == [3, 2]


async def test_recursive_cte_regression(clean):
    await seed()
    totals = Post.objects.select(Post.author_id, func.count().label("n")).group_by(Post.author_id).cte("totals")
    rows = await User.objects.select(User.name, totals.c.n).join(totals, totals.c.author_id == User.id).order_by(User.name)
    assert [tuple(r) for r in rows] == [("Alice", 2), ("Bob", 1)]
    assert [r.id for r in await User.objects.select(User.id).order_by(User.id)] == [1, 2, 3]


async def test_or_is_still_filter_combination(clean):
    await seed()
    high = Post.objects.filter(Post.views > 50)
    low = Post.objects.filter(Post.views < 10)
    assert sorted(p.title for p in await (high | low)) == ["a1", "b1"]
    assert sorted(r.title for r in await Post.objects.select(Post.title).filter((Post.views > 50) | (Post.views < 10))) == ["a1", "b1"]


async def test_string_and_text_columns_union(clean):
    (alice, *_), posts = await seed()
    await Post.objects.filter(Post.title == "a1").update(body="first body")
    await Comment.objects.insert(post=posts[0], body="first body", author=alice)
    # Post.title is `varchar(200)` and Comment.body is `text`: one family.
    q = Post.objects.select(Post.title.label("text")).union(Comment.objects.select(Comment.body.label("text")))
    assert sorted(r.text for r in await q) == ["a1", "a2", "b1", "first body"]
    assert await q.count() == 4


async def test_unbound_param_and_outer(clean):
    await seed()
    bound = Post.objects.filter(Post.id == monorm.param("n")).select(Post.id, Post.title.label("label"))
    with pytest.raises(QueryError, match="placeholder"):
        await names(low=1, high=2).union(bound)
    inner = Post.objects.filter(Post.id == monorm.outer(User.id)).select(Post.id, Post.title.label("label"))
    with pytest.raises(ValueError, match="enclosing"):
        await names(low=1, high=2).union(inner)


async def test_sqlite_select_union():
    registry = monorm.Registry()
    m = monorm.loads(SQLITE_SOURCE, registry=registry)
    Author, Book = m["Author"], m["Book"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    try:
        a, b = await Author.objects.using(db).insert_many([{"email": "a@example.com", "name": "Alice"}, {"email": "b@example.com", "name": "Bob"}]).returning()
        await Book.objects.using(db).insert_many(
            [{"author_id": a.id, "title": "one"}, {"author_id": a.id, "title": "Alice"}, {"author_id": b.id, "title": "three"}]
        )
        people = Author.objects.using(db).select(Author.name.label("label"))
        books = Book.objects.using(db).select(Book.title.label("label"))
        assert sorted(r.label for r in await people.union(books)) == ["Alice", "Bob", "one", "three"]
        assert sorted(r.label for r in await people.union_all(books)) == ["Alice", "Alice", "Bob", "one", "three"]
        left = people.order_by(Author.name)[:1]
        right = books.order_by(Book.title.desc())[:1]
        assert sorted(r.label for r in await left.union_all(right)) == ["Alice", "three"]
        assert [r.label for r in await left.union_all(right).order_by("label")[1:2]] == ["three"]
        assert await left.union_all(right).union_all(people).count() == 4
        assert await left.union_all(right).order_by("-label")[:1].exists()
        # A mismatch is an error, not a wrong result; a type mismatch names the branches.
        with pytest.raises(QueryError, match="same columns"):
            await people.union(Book.objects.using(db).select(Book.title.label("other")))
        with pytest.raises(QueryError, match="branch 0 and .* in branch 1"):
            await people.union(Book.objects.using(db).select(Book.id.label("label")))
        with pytest.raises(QueryError):
            await people.union(books).order_by("missing")
        async with db.transaction():
            await Author.objects.using(db).insert(email="t@example.com", name="Tess")
            assert "Tess" in [r.label for r in await people.union(books)]
        q = people.union(books).order_by("label")
        first = await q
        assert await q == first
        assert len(await q.all()) == len(first)
    finally:
        await db.drop_tables()
        await db.close()
