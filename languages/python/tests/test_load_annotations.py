"""`load()` takes a labeled relation aggregate; the label becomes an attribute of each row."""

import pytest
from blog.models import Comment, Post, User
from pathlib import Path

import orm
from orm import func

SQLITE_SOURCE = Path(__file__).resolve().parents[3].joinpath("examples/sqlite/schema.prisma").read_text()


def n_comments():
    return func.count(Post.comments).label("n")


async def seed():
    (alice,) = await User.objects.insert_many([{"email": "a@example.com", "name": "Alice"}]).returning()
    p1, p2 = await Post.objects.insert_many(
        [{"author": alice, "title": "p1", "body": ""}, {"author": alice, "title": "p2", "body": ""}]
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


def test_load_rejects_a_non_aggregate():
    with pytest.raises(TypeError, match="relation aggregates with a label"):
        Post.objects.load(Post.id.label("x"))
    with pytest.raises(TypeError, match="relation aggregates with a label"):
        Post.objects.load(func.count().label("x"))


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
    registry = orm.Registry()
    m = orm.loads(SQLITE_SOURCE, registry=registry)
    Author, Book = m["Author"], m["Book"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
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
