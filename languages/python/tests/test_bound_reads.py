"""Bound query sets (Model.objects.of, monorm.of, obj.qs()): reads that fill the given objects in place."""

import asyncio
import os

import pytest

import monorm
from blog.models import Comment, Post, User
from artifact_caps import has_extension



async def seed():
    alice = await User.objects.insert(email="alice@example.com", name="Alice")
    posts = await Post.objects.insert_many(
        [{"author": alice, "title": f"p{i}", "body": "b", "views": i} for i in range(4)]
    ).returning()
    return alice, posts


async def stale(posts):
    """The posts as another reader holds them, then changed in the database."""
    held = await Post.objects.filter(Post.id.in_([p.id for p in posts])).order_by(Post.id)
    await Post.objects.filter(Post.id.in_([p.id for p in posts])).update(title="changed", views=100)
    return held


async def test_a_bound_read_fills_the_objects_in_place_in_input_order(clean):
    _, posts = await seed()
    held = await stale(posts)
    wanted = [held[2], held[0], held[3]]
    result = await Post.objects.of(wanted)
    assert result == wanted and all(a is b for a, b in zip(result, wanted))
    assert [(p.title, p.views) for p in wanted] == [("changed", 100)] * 3
    assert held[1].title == "p1"


async def test_one_object_and_the_instance_shortcut(clean):
    _, posts = await seed()
    held = await stale(posts)
    assert await monorm.of(held[0]) == [held[0]]
    assert held[0].title == "changed"
    assert await held[1].qs() == [held[1]]
    assert held[1].views == 100
    assert await Post.objects.of(held[2]) == [held[2]]


async def test_a_bound_read_runs_one_statement_for_many_objects(clean, db, observe_queries):
    _, posts = await seed()
    held = await stale(posts)
    with observe_queries(db) as events:
        await Post.objects.of(held)
    assert len(events) == 1
    assert Post.objects.of(held).inspect().statement_count == 1


async def test_an_unmatched_object_stays_unchanged_and_is_left_out(clean):
    _, posts = await seed()
    held = await stale(posts)
    await Post.objects.filter(Post.id == held[1].id).delete()
    result = await Post.objects.of(held)
    assert result == [held[0], held[2], held[3]]
    assert held[1].title == "p1"


async def test_two_objects_with_one_key_are_both_filled_and_returned(clean):
    _, posts = await seed()
    first = await Post.objects.get(Post.id == posts[0].id)
    second = await Post.objects.get(Post.id == posts[0].id)
    await Post.objects.filter(Post.id == posts[0].id).update(title="changed")
    assert await Post.objects.of([first, second]) == [first, second]
    assert (first.title, second.title) == ("changed", "changed")


async def test_the_merge_rule_keeps_other_loaded_fields(clean):
    _, posts = await seed()
    partial = await Post.objects.load(Post.title, Post.views).get(Post.id == posts[0].id)
    await Post.objects.filter(Post.id == posts[0].id).update(title="changed", views=100, body="new body")
    await Post.objects.of(partial).load(Post.title)
    assert partial.title == "changed" and partial.views == 0
    with pytest.raises(monorm.NotLoaded):
        partial.body
    await Post.objects.of(partial)
    assert partial.body == "new body" and partial.views == 100


async def test_a_cached_relation_whose_key_changed_is_dropped(clean):
    alice, posts = await seed()
    bob = await User.objects.insert(email="bob@example.com", name="Bob")
    post = await Post.objects.load(Post.author).get(Post.id == posts[0].id)
    assert post.author.name == "Alice"
    await Post.objects.filter(Post.id == post.id).update(author=bob)
    await Post.objects.of(post)
    with pytest.raises(monorm.NotLoaded):
        post.author


async def test_load_selects_only_what_it_names_plus_the_hidden_keys(clean):
    _, posts = await seed()
    held = await stale(posts)
    bare = [await Post.objects.load(Post.id).get(Post.id == p.id) for p in posts[:2]]
    await Post.objects.of(bare).load(Post.title)
    assert [p.title for p in bare] == ["changed", "changed"]
    with pytest.raises(monorm.NotLoaded):
        bare[0].views
    assert held[0].title == "p0"


async def test_a_load_with_columns_joins_a_to_one_relation_in_the_one_statement(clean, db, observe_queries):
    _, posts = await seed()
    held = await Post.objects.filter(Post.id.in_([p.id for p in posts]))
    with observe_queries(db) as events:
        await Post.objects.of(held).load(Post.title, Post.author)
    assert len(events) == 1
    assert {p.author.name for p in held} == {"Alice"}


async def test_a_relation_only_load_runs_no_root_query(clean, db, observe_queries):
    alice, posts = await seed()
    await Comment.objects.insert_many([{"post": posts[0], "body": "a"}, {"post": posts[1], "body": "b"}])
    held = await stale(posts)
    with observe_queries(db) as events:
        result = await Post.objects.of(held).load(Post.comments)
    assert len(events) == 1 and "comment" in events[0].sql.lower() and result == held
    assert [len(p.comments.cached) for p in held] == [1, 1, 0, 0]
    assert held[0].title == "p0"


async def test_filter_narrows_the_bound_rows(clean):
    _, posts = await seed()
    held = await stale(posts)
    result = await Post.objects.of(held).filter(Post.id.in_([held[0].id, held[3].id]))
    assert result == [held[0], held[3]]
    assert held[1].title == "p1"
    assert await Post.objects.of(held).exclude(Post.id == held[0].id).count() == 3


async def test_a_relation_only_load_with_a_filter_reads_the_keys_first(clean):
    _, posts = await seed()
    await Comment.objects.insert_many([{"post": posts[0], "body": "a"}, {"post": posts[1], "body": "b"}])
    held = await Post.objects.filter(Post.id.in_([p.id for p in posts])).order_by(Post.id)
    result = await Post.objects.of(held).filter(Post.id == held[1].id).load(Post.comments)
    assert result == [held[1]]
    assert [c.body for c in held[1].comments.cached] == ["b"]
    with pytest.raises(monorm.NotLoaded):
        held[0].comments.cached


async def test_count_and_exists_count_the_matching_bound_rows(clean):
    _, posts = await seed()
    held = await Post.objects.filter(Post.id.in_([p.id for p in posts]))
    await Post.objects.filter(Post.id == held[0].id).delete()
    assert await Post.objects.of(held).count() == 3
    assert await Post.objects.of(held).exists()
    assert not await Post.objects.of(held[0]).exists()
    assert await Post.objects.of(held[0]).count() == 0


async def test_a_default_query_set_filter_stays_on_the_bound_set(clean):
    _, posts = await seed()
    held = await stale(posts)
    published = Post.objects.filter(Post.views > 1000)
    assert await published.of(held) == []
    assert held[0].title == "p0"


async def test_lock_needs_a_transaction_and_locks_the_rows(clean, db):
    _, posts = await seed()
    held = await Post.objects.filter(Post.id.in_([p.id for p in posts]))
    with pytest.raises(monorm.TransactionRequired):
        await Post.objects.of(held).lock()
    async with db.transaction():
        query = Post.objects.of(held).lock(False, skip_locked=True)
        assert "FOR SHARE OF" in (sql := query.sql()) and sql.endswith("SKIP LOCKED")
        assert len(await query) == 4
    with pytest.raises(ValueError):
        Post.objects.of(held).lock(nowait=True, skip_locked=True)


async def test_a_relation_only_read_with_a_lock_reads_the_keys_with_for_update(clean, db):
    if "postgres" not in os.environ["ORM_TEST_DATABASE_URL"]:
        pytest.skip("FOR UPDATE needs Postgres")
    _, posts = await seed()
    held = await Post.objects.filter(Post.id.in_([p.id for p in posts]))
    events: list[monorm.QueryEvent] = []
    db.on_query(events.append)
    async with db.transaction():
        await monorm.of(held).lock().load(Post.comments)
    assert any("FOR UPDATE" in e.sql for e in events)


async def test_a_relation_only_read_gives_its_statements_to_on_query(clean, db):
    _, posts = await seed()
    held = await stale(posts)
    events: list[monorm.QueryEvent] = []
    db.on_query(events.append)
    await monorm.of(held).load(Post.comments)
    assert any("comment" in e.sql.lower() for e in events)


async def test_using_names_the_database_to_read(clean, db):
    _, posts = await seed()
    held = await stale(posts)
    other = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], default=False)
    try:
        events: list[monorm.QueryEvent] = []
        other.on_query(events.append)
        await Post.objects.of(held).using(other)
        assert len(events) == 1 and held[0].title == "changed"
        await Post.objects.of(held).using("primary")
    finally:
        await other.close()
    with pytest.raises(ValueError):
        Post.objects.of(held).using("replica")  # type: ignore[arg-type]


async def test_objects_from_two_databases_need_using(clean, db):
    _, posts = await seed()
    first = await Post.objects.get(Post.id == posts[0].id)
    other = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], default=False)
    try:
        second = await Post.objects.using(other).get(Post.id == posts[1].id)
        with pytest.raises(monorm.QueryError):
            await Post.objects.of([first, second])
        assert await Post.objects.of([first, second]).using(db) == [first, second]
    finally:
        await other.close()


async def test_input_errors(clean):
    alice, posts = await seed()
    with pytest.raises(TypeError):
        Post.objects.of([])
    with pytest.raises(TypeError):
        Post.objects.of(iter(()))
    with pytest.raises(TypeError):
        Post.objects.of([posts[0], alice])
    with pytest.raises(TypeError):
        monorm.of([posts[0], alice])
    with pytest.raises(TypeError):
        User.objects.of(posts[0])
    partial = await Post.objects.load(Post.title).get(Post.id == posts[0].id)
    del partial.__dict__["_orm_internal"]["id"]
    with pytest.raises(TypeError, match="primary key"):
        Post.objects.of(partial)


async def test_a_query_set_with_shape_state_is_refused(clean):
    alice, posts = await seed()
    for shaped in (
        Post.objects.order_by(Post.id),
        Post.objects[:2],
        Post.objects.limit(1),
        Post.objects.offset(1),
        Post.objects.require(),
    ):
        with pytest.raises(TypeError):
            getattr(shaped, "of")(posts)
    with pytest.raises(TypeError):
        User.posts.objects.of(posts[0])
    with pytest.raises(TypeError):
        alice.posts.of(posts[0])


def test_a_bound_query_set_has_no_shape_methods():
    bound = Post.objects.of.__self__  # the root query set; the bound type is below
    del bound
    for name in (
        "order_by", "limit", "offset", "first", "last", "get", "paginate", "select", "values", "group_by",
        "distinct", "join", "cte", "union", "insert", "insert_many", "get_or_insert", "in_bulk", "prepare",
        "batches", "require", "label", "as_prefetch", "__getitem__", "__aiter__",
    ):  # fmt: skip
        assert not hasattr(monorm.BoundQuerySet, name), name
    assert "qs" in monorm.model._RESERVED


async def test_a_column_named_qs_is_reserved():
    with pytest.raises(TypeError):
        monorm.loads("model Q {\n  id Int @id\n  qs Int\n}\n", registry=monorm.Registry())


@pytest.mark.skipif(not has_extension("monorm.soft-delete"), reason="requires a selected soft-delete native artifact")
async def test_a_soft_deleted_object_is_refreshed_because_of_applies_no_default():
    source = 'model Doc {\n  id Int @id\n  title String\n  @@soft_delete.deleted_at("deleted", mode: "application")\n}\n'
    registry = monorm.Registry()
    Doc = monorm.loads(source, registry=registry)["Doc"]
    db = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        docs = Doc.objects.using(db)
        await docs.insert_many([{"id": 1, "title": "a"}, {"id": 2, "title": "b"}])
        held = await docs.order_by(Doc.id)
        await docs.filter(Doc.id == 1).update(title="changed")
        await docs.filter(Doc.id == 1).delete()
        await docs.filter(Doc.id == 2).update(title="changed")
        assert await Doc.objects.of(held).using(db) == held
        assert held[0].title == "changed" and held[1].title == "changed"
        assert await Doc.objects.of(held).using(db).without_defaults("deleted") == held
        assert await Doc.objects.of(held[0]).using(db).without_defaults() == [held[0]]
        assert await Doc.objects.of(held).using(db).count() == 2
        assert await Doc.objects.of(held).using(db).filter(Doc.id == 2) == [held[1]]
    finally:
        await db.drop_tables()
        await db.close()


async def test_a_large_bound_read_runs_in_chunks_of_500_keys(clean, db, observe_queries):
    alice = await User.objects.insert(email="alice@example.com", name="Alice")
    await Post.objects.insert_many([{"author": alice, "title": f"p{i}", "body": "b"} for i in range(1200)])
    held = await Post.objects.order_by(Post.id)
    await Post.objects.update(title="changed")
    wanted = list(reversed(held))
    with observe_queries(db) as events:
        result = await Post.objects.of(wanted)
        assert await Post.objects.of(wanted).count() == 1200
        assert await Post.objects.of(wanted).exists()
    assert len(events) == 3 + 3 + 1
    assert result == wanted and all(a is b for a, b in zip(result, wanted))
    assert {p.title for p in held} == {"changed"}
    with observe_queries(db) as events:
        assert len(await Post.objects.of(wanted).filter(Post.id > 0).load(Post.comments)) == 1200
    assert len([e for e in events if "comments" not in e.sql.lower()]) == 3
    assert Post.objects.of(wanted).sql().rsplit("IN (", 1)[1].count(",") == 499


async def test_a_large_composite_key_bound_read_does_not_hang(db, observe_queries):
    source = "model Cell {\n  a Int\n  b Int\n  v Int @default(0)\n  @@id([a, b])\n}\n"
    registry = monorm.Registry()
    Cell = monorm.loads(source, registry=registry)["Cell"]
    other = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=registry, default=False)
    try:
        await other.drop_tables()
        await other.create_tables()
        await Cell.objects.using(other).insert_many([{"a": i, "b": 1} for i in range(1200)])
        held = await Cell.objects.using(other).order_by(Cell.a)
        await Cell.objects.using(other).update(v=7)
        with observe_queries(other) as events:
            assert await asyncio.wait_for(Cell.objects.of(held).using(other), 60) == held
            assert await Cell.objects.of(held).using(other).count() == 1200
        assert len(events) == 10
        assert {c.v for c in held} == {7}
    finally:
        await other.drop_tables()
        await other.close()
