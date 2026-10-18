"""The native row builder sets `annotations` columns as attributes.
The public `load()` item arrives later, so these tests inject the IR key."""

import pytest
from blog.models import Comment, Post, User

from orm.query import QuerySet

ANNOTATION = {"name": "n", "expr": {"t": "func", "name": "count", "rel": ["comments"]}}


@pytest.fixture
def annotate_posts(monkeypatch):
    original = QuerySet._query_ir

    def patched(self, op, *args, **kwargs):
        ir, ctx = original(self, op, *args, **kwargs)
        if op == "select" and self._model is Post:
            ir["annotations"] = [ANNOTATION]
        return ir, ctx

    monkeypatch.setattr(QuerySet, "_query_ir", patched)


async def seed():
    (alice,) = await User.objects.insert_many([{"email": "a@example.com", "name": "Alice"}]).returning()
    p1, p2 = await Post.objects.insert_many(
        [{"author": alice, "title": "p1", "body": ""}, {"author": alice, "title": "p2", "body": ""}]
    ).returning()
    await Comment.objects.insert_many([{"post": p1, "body": "x"}, {"post": p1, "body": "y"}])


async def test_root_objects_get_the_annotation(clean, annotate_posts):
    await seed()
    posts = await Post.objects.order_by(Post.title)
    assert [p.n for p in posts] == [2, 0]


async def test_prefetched_objects_get_the_annotation(clean, annotate_posts):
    await seed()
    (alice,) = await User.objects.load(User.posts)
    assert sorted(p.n for p in alice.posts.cached) == [0, 2]
