"""`load()` takes a labeled relation aggregate; the label becomes an attribute of each row."""

import pytest
from blog.models import Comment, Post, User
from pathlib import Path

import monorm
from monorm import QueryError, func

SQLITE_SOURCE = Path(__file__).resolve().parents[3].joinpath("examples/sqlite/schema.prisma").read_text()


def n_comments():
    return func.count(Post.comments).label("n")


async def seed(views=(0, 0)):
    (alice,) = await User.objects.insert_many([{"email": "a@example.com", "name": "Alice"}]).returning()
    p1, p2 = await Post.objects.insert_many(
        [{"author": alice, "title": "p1", "body": "", "views": views[0]}, {"author": alice, "title": "p2", "body": "", "views": views[1]}]
    ).returning()
    await Comment.objects.insert_many([{"post": p1, "body": "x"}, {"post": p1, "body": "y"}])
    return alice, p1, p2


async def test_query_set_load_sets_the_label(clean):
    await seed()
    posts = await Post.objects.load(n_comments()).order_by(Post.title)
    assert [(p.title, p.n) for p in posts] == [("p1", 2), ("p2", 0)]


async def test_labeled_load_keeps_the_other_columns_and_relations(clean):
    await seed()
    posts = await Post.objects.load(Post.title, Post.author, n_comments()).order_by(Post.title)
    assert [(p.title, p.n, p.author.name) for p in posts] == [("p1", 2, "Alice"), ("p2", 0, "Alice")]


async def test_labeled_load_only_adds_to_the_full_row(clean):
    await seed()
    posts = await Post.objects.load(n_comments()).order_by(Post.title)
    assert [p.body for p in posts] == ["", ""]


async def test_relation_query_set_nests_the_aggregate(clean):
    await seed()
    (alice,) = await User.objects.load(User.posts.objects.load(n_comments()))
    assert sorted((p.title, p.n) for p in alice.posts.cached) == [("p1", 2), ("p2", 0)]


async def test_labeled_relation_query_set_nests_the_aggregate(clean):
    await seed()
    (alice,) = await User.objects.load(User.posts.objects.load(n_comments()).label("counted"))
    assert sorted((p.title, p.n) for p in alice.counted) == [("p1", 2), ("p2", 0)]


async def test_instance_load_reads_only_the_aggregate(clean):
    _, p1, _ = await seed()
    post = await Post.objects.load(Post.title).get(Post.id == p1.id)
    await post.load(n_comments())
    assert post.n == 2
    assert post.title == "p1"


async def test_instance_load_of_a_label_does_not_read_other_columns(clean):
    _, p1, _ = await seed()
    post = await Post.objects.load(Post.title).get(Post.id == p1.id)
    await post.load(n_comments())
    assert "body" not in post.__dict__


async def test_cached_relation_set_is_not_used_when_it_has_a_label(clean):
    await seed()
    (alice,) = await User.objects.load(User.posts)
    posts = await alice.posts.load(n_comments())
    assert sorted(p.n for p in posts) == [0, 2]


async def test_instance_load_reads_a_loaded_label_again(clean):
    _, p1, _ = await seed()
    post = await Post.objects.load(Post.title).get(Post.id == p1.id)
    await post.load(n_comments())
    await Comment.objects.insert(post=p1, body="z")
    await post.load(n_comments())
    assert post.n == 3


async def test_aggregate_over_a_filtered_relation(clean):
    await seed()
    posts = await Post.objects.load(func.count(Post.comments, filter=Post.comments.body == "x").label("n")).order_by(
        Post.title
    )
    assert [p.n for p in posts] == [1, 0]


async def test_load_takes_a_labeled_expression(clean):
    alice, p1, p2 = await seed(views=(4, 0))
    posts = await Post.objects.load((Post.views * 2).label("double_views"), Post.id.label("same_id")).order_by(Post.title)
    assert [(p.title, p.double_views, p.same_id) for p in posts] == [("p1", 8, p1.id), ("p2", 0, p2.id)]
    assert [p.body for p in posts] == ["", ""]


async def test_load_takes_a_window_expression_with_one_row_per_instance(clean):
    alice, p1, p2 = await seed()
    (bob,) = await User.objects.insert_many([{"email": "b@example.com", "name": "Bob"}]).returning()
    await Post.objects.insert_many([{"author": bob, "title": "p3", "body": ""}])
    posts = await Post.objects.load(func.count().over(partition_by=Post.author_id).label("author_posts")).order_by(Post.title)
    assert [(p.title, p.author_posts) for p in posts] == [("p1", 2), ("p2", 2), ("p3", 1)]


def test_load_rejects_an_aggregate_that_merges_rows():
    with pytest.raises(QueryError, match="annotation x is an aggregate"):
        Post.objects.load(func.count().label("x")).sql()


async def test_a_merging_aggregate_fails_when_run(clean):
    await seed()
    with pytest.raises(QueryError, match="annotation x is an aggregate"):
        await Post.objects.load(func.count().label("x"))


async def test_bound_load_takes_a_labeled_expression(clean):
    alice, p1, p2 = await seed(views=(3, 0))
    posts = await Post.objects.all()
    result = await Post.objects.of(posts).load((Post.views + 1).label("next_views"))
    assert sorted((p.title, p.next_views) for p in result) == [("p1", 4), ("p2", 1)]
    again = await Post.objects.of(posts).load(func.count().over(partition_by=Post.author_id).label("same_author"))
    assert [p.same_author for p in again] == [2, 2]
    with pytest.raises(ValueError, match="is a field, relation or member"):
        Post.objects.of(posts).load((Post.views * 2).label("views"))


def test_a_labeled_expression_must_not_name_a_field():
    with pytest.raises(ValueError, match="is a field, relation or member"):
        Post.objects.load((Post.views * 2).label("title"))


def totals_cte():
    return Post.objects.select(Post.author_id, func.sum(Post.views).label("views")).group_by(Post.author_id).cte("totals")


async def seed_views():
    alice, p1, p2 = await seed(views=(80, 40))
    bob, carol = await User.objects.insert_many(
        [{"email": "b@example.com", "name": "Bob"}, {"email": "c@example.com", "name": "Carol"}]
    ).returning()
    await Post.objects.insert_many([{"author": bob, "title": "b1", "body": "", "views": 5}])
    return alice, bob, carol


async def test_join_a_cte_filters_and_annotates_instances(clean):
    alice, bob, carol = await seed_views()
    totals = totals_cte()
    users = await User.objects.all()
    result = await (
        User.objects.of(users)
        .join(totals, totals.c.author_id == User.id)
        .filter(totals.c.views > 100)
        .load(totals.c.views.label("total_views"))
    )
    assert [(u.name, u.total_views) for u in result] == [("Alice", 120)]


async def test_join_a_cte_on_a_query_set_loads_the_column(clean):
    await seed_views()
    totals = totals_cte()
    users = await User.objects.join(totals, totals.c.author_id == User.id).load(totals.c.views.label("total_views")).order_by(User.name)
    assert [(u.name, u.total_views) for u in users] == [("Alice", 120), ("Bob", 5)]


async def test_a_bound_join_that_matches_twice_raises(clean):
    await seed_views()
    posts = Post.objects.select(Post.author_id, Post.views).cte("posts_of")
    users = await User.objects.all()
    with pytest.raises(QueryError, match=r"join\(posts_of\)"):
        await User.objects.of(users).join(posts, posts.c.author_id == User.id).load(posts.c.views.label("v"))


async def test_a_bound_outer_join_without_a_match_gives_none(clean):
    await seed_views()
    totals = totals_cte()
    users = await User.objects.order_by(User.name).all()
    result = await (
        User.objects.of(users).join(totals, totals.c.author_id == User.id, outer=True).load(totals.c.views.label("total_views"))
    )
    assert {u.name: u.total_views for u in result} == {"Alice": 120, "Bob": 5, "Carol": None}


async def test_a_bound_join_filters_the_relation_load_too(clean):
    await seed_views()
    totals = totals_cte()
    users = await User.objects.all()
    result = await User.objects.of(users).join(totals, totals.c.author_id == User.id).filter(totals.c.views > 100).load(User.posts)
    assert [(u.name, len(u.posts.cached)) for u in result] == [("Alice", 2)]


async def test_a_bound_join_cannot_write(clean):
    await seed_views()
    totals = totals_cte()
    users = await User.objects.all()
    with pytest.raises(QueryError, match="join"):
        await User.objects.of(users).join(totals, totals.c.author_id == User.id).update(name="x")


def test_load_rejects_an_expression_without_a_label():
    with pytest.raises(TypeError, match="needs a label"):
        Post.objects.load(func.count(Post.comments))


def test_label_must_not_name_a_field_relation_or_member():
    for name in ("title", "author", "pk"):
        with pytest.raises(ValueError, match="is a field, relation or member"):
            Post.objects.load(func.count(Post.comments).label(name))


def test_label_must_be_unique_among_loads():
    with pytest.raises(ValueError, match="twice"):
        Post.objects.load(func.count(Post.comments).label("n"), func.count(Post.comments).label("n"))
    with pytest.raises(ValueError, match="twice"):
        Post.objects.load(n_comments(), Post.comments.objects.label("n"))
    with pytest.raises(ValueError, match="twice"):
        Post.objects.load(Post.comments.objects.label("n")).load(n_comments())


def test_relation_query_set_checks_its_own_labels():
    with pytest.raises(ValueError, match="is a field, relation or member"):
        User.objects.load(User.posts.objects.load(func.count(Post.comments).label("title")))


async def test_select_ignores_labeled_loads(clean):
    await seed()
    rows = await Post.objects.load(n_comments()).select(Post.title).order_by(Post.title)
    assert [tuple(r) for r in rows] == [("p1",), ("p2",)]


async def test_sqlite_load_sets_the_label():
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
            [{"author_id": a.id, "title": "one"}, {"author_id": a.id, "title": "two"}]
        )
        n = func.count(Author.books).label("n")
        authors = await Author.objects.using(db).load(n).order_by(Author.name)
        assert [(x.name, x.n) for x in authors] == [("Alice", 2), ("Bob", 0)]
        (alice,) = await Author.objects.using(db).load(Author.books.objects.load(Book.title)).filter(Author.id == a.id)
        await alice.load(func.count(Author.books).label("m"))
        assert alice.m == 2
        nested = await Author.objects.using(db).load(Author.books.objects.load(func.count(Book.author).label("k")))
        assert sorted(bk.k for x in nested for bk in x.books.cached) == [1, 1]
    finally:
        await db.drop_tables()
        await db.close()
