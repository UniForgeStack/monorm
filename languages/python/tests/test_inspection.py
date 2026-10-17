"""Planner structure and actual execution are separate contracts."""

from dataclasses import FrozenInstanceError
import gc
import warnings

import pytest
from blog.models import Comment, Post, Tag, User

import orm


def test_inspection_is_typed_immutable_and_needs_no_connection():
    query = Post.objects.load(Post.author)
    result = query.inspect()
    assert isinstance(result, orm.QueryInspection)
    assert result.statement_count == 1
    assert not result.has_batching
    assert result.statements[0].joins_count == 1
    assert result.statements[0].correlated_count == 0
    assert result.statements[0].uncorrelated_count == 0
    assert query.inspect() == result
    with pytest.raises(FrozenInstanceError):
        result.statement_count = 2  # type: ignore[misc]


def test_nested_relation_filters_count_each_subquery_once():
    result = User.objects.filter(User.posts.comments.body == "hi").inspect()
    assert result.statements[0].correlated_count == 2
    assert result.statements[0].uncorrelated_count == 0


def test_nested_subqueries_have_their_own_dependencies():
    # Only the innermost query depends on its enclosing Post query. The Post
    # query itself has no dependency on User, despite containing a correlation.
    comments = orm.exists(Comment.objects.filter(Comment.post_id == orm.outer(Post.id)))
    result = User.objects.filter(orm.exists(Post.objects.filter(comments))).inspect()
    assert result.statements[0].correlated_count == 1
    assert result.statements[0].uncorrelated_count == 1
    # Same-model aliases must not mistake a local reference for an outer one.
    average = (
        Post.objects.filter(Post.author_id == orm.outer(Post.author_id)).select(orm.func.avg(Post.views)).as_scalar()
    )
    assert Post.objects.filter(Post.views > average).inspect().statements[0].correlated_count == 1
    assert Post.objects.filter(Post.views > average).inspect().statements[0].uncorrelated_count == 0


def test_ctes_are_counted_separately_from_subqueries():
    recent = Post.objects.filter(Post.views > 5).cte("recent")
    result = Post.objects.from_(recent).inspect()
    assert result.statements[0].correlated_count == 0
    assert result.statements[0].uncorrelated_count == 0
    assert result.statements[0].cte_count == 1
    totals = Post.objects.select(Post.author_id, orm.func.count().label("n")).group_by(Post.author_id).cte("totals")
    result = User.objects.filter(User.id.in_(totals.select(totals.c.author_id))).inspect()
    assert result.statements[0].correlated_count == 0
    assert result.statements[0].uncorrelated_count == 1
    assert result.statements[0].cte_count == 1


async def test_empty_parents_skip_prefetch_execution(clean):
    query = User.objects.load(User.posts.comments)
    planned = query.inspect()
    assert planned.statement_count == 3
    assert planned.has_batching
    assert [s.has_batching for s in planned.statements] == [False, True, True]
    events = []
    remove = clean.on_query(events.append)
    try:
        assert query.inspect() == planned
        assert events == []
        assert await query == []
    finally:
        remove()
    assert len(events) == 1


async def test_prefetch_templates_repeat_for_key_batches(clean):
    db = await orm.connect(clean.url, default=False, _disable=("max_params=2",))
    try:
        await User.objects.insert_many([{"email": f"u{i}@x", "name": f"U{i}"} for i in range(5)])
        query = User.objects.using(db).load(User.posts)
        assert query.inspect().statement_count == 2
        events = []
        remove = db.on_query(events.append)
        try:
            assert len(await query) == 5
        finally:
            remove()
        assert len(events) == 4  # root + three key batches, even with no children
    finally:
        await db.close()


async def test_write_batch_inspection_and_observation(clean):
    statement = Tag.objects.insert_many([{"name": f"t{i}"} for i in range(5)], batch_size=2).returning()
    result = statement.inspect()
    assert result.statement_count == 3
    assert result.has_batching and all(s.has_batching for s in result.statements)
    events = []
    remove = clean.on_query(events.append)
    try:
        assert events == []
        tags = await statement
    finally:
        remove()
    assert len(events) == result.statement_count
    update = Tag.objects.update_many([{"id": t.id, "name": t.name + "!"} for t in tags], batch_size=2)
    assert update.inspect().statement_count == 3
    assert update.inspect().has_batching
    returned = update.returning()
    assert returned.inspect() == update.inspect()
    events.clear()
    remove = clean.on_query(events.append)
    try:
        assert len(await returned) == 5
    finally:
        remove()
    assert len(events) == 3
    empty = Tag.objects.insert_many([])
    assert empty.inspect() == orm.QueryInspection(0, False, ())
    assert await empty == 0


async def test_mutations_inspect_generated_subqueries(clean):
    scalar = Comment.objects.filter(Comment.post_id == orm.outer(Post.id)).select(orm.func.count()).as_scalar()
    update = Post.objects.update(views=scalar)
    assert update.inspect().statements[0].correlated_count == 1
    assert update.inspect().statements[0].uncorrelated_count == 0
    assert await update == 0
    delete = Post.objects.filter(Post.comments.body == "hi").delete().returning()
    assert delete.inspect().statements[0].correlated_count == 1
    assert delete.inspect().statements[0].uncorrelated_count == 0
    assert await delete == []


def test_prepared_inspection_binds_parameters():
    query = Post.objects.filter(Post.views > orm.param("minimum")).prepare()
    assert query.inspect(minimum=5) == Post.objects.filter(Post.views > 5).inspect()
    with pytest.raises(TypeError):
        query.inspect()


def test_inspecting_a_write_is_a_valid_terminal_use():
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        assert Tag.objects.insert_many([{"name": "inspection only"}]).inspect().statement_count == 1
        assert Tag.objects.update_many([]).inspect().statement_count == 0
        assert Tag.objects.update(name="inspection only").inspect().statement_count == 1
        assert Tag.objects.filter(Tag.name == "absent").delete().returning().inspect().statement_count == 1
        gc.collect()
    assert caught == []


async def test_unbound_inspection_honors_default_connection_capabilities(clean, monkeypatch):
    db = await orm.connect(clean.url, default=False, _disable=("max_params=2",))
    try:
        with monkeypatch.context() as patch:
            patch.setattr(orm.db, "_default", db)
            result = Tag.objects.insert_many([{"name": f"t{i}"} for i in range(5)]).inspect()
            assert result.statement_count == 3
            assert result.has_batching
    finally:
        await db.close()
