"""Bound query sets: update() and delete() write the rows of the given objects and fill them in place."""

import json
import os

import pytest

import orm
from blog.models import Post, User
from orm import _native
from soft_delete_ext import loads_soft_delete

CAPABILITIES = set(json.loads(_native.native_artifact())["capabilities"])


async def seed(count=4):
    alice = await User.objects.insert(email="alice@example.com", name="Alice")
    posts = await Post.objects.insert_many(
        [{"author": alice, "title": f"p{i}", "body": "b", "views": i} for i in range(count)]
    ).returning()
    return await Post.objects.filter(Post.id.in_([p.id for p in posts])).order_by(Post.id)


async def test_a_bound_update_writes_one_statement_and_fills_in_input_order(clean, db, observe_queries):
    held = await seed()
    wanted = [held[2], held[0], held[3]]
    with observe_queries(db) as events:
        result = await Post.objects.of(wanted).update(title="changed", views=Post.views + 10)
    assert len(events) == 1
    assert result == wanted and all(a is b for a, b in zip(result, wanted))
    assert [(p.title, p.views) for p in wanted] == [("changed", 12), ("changed", 10), ("changed", 13)]
    assert (held[1].title, held[1].views) == ("p1", 1)
    rows = await Post.objects.order_by(Post.id)
    assert [r.title for r in rows] == ["changed", "p1", "changed", "changed"]


async def test_a_bound_update_of_a_partial_object_keeps_its_shape(clean):
    held = await seed()
    partial = await Post.objects.load(Post.title).get(Post.id == held[0].id)
    [done] = await orm.of([partial]).update(title="changed")
    assert done is partial and partial.title == "changed"
    with pytest.raises(orm.NotLoaded):
        partial.views


async def test_a_bound_update_leaves_out_an_object_with_no_row_and_keeps_the_filters(clean):
    held = await seed()
    await Post.objects.filter(Post.id == held[0].id).delete()
    assert await Post.objects.of(held).update(title="x") == held[1:]
    assert held[0].title == "p0"
    assert await Post.objects.of(held).filter(Post.views >= 2).update(title="y") == held[2:]
    assert [p.title for p in held[1:]] == ["x", "y", "y"]


async def test_a_bound_update_takes_the_instance_shortcut_and_a_duplicate(clean):
    held = await seed(2)
    assert await held[0].qs().update(views=7) == [held[0]]
    assert held[0].views == 7
    assert await Post.objects.of([held[1], held[1]]).update(views=9) == [held[1], held[1]]
    assert held[1].views == 9


async def test_a_bound_delete_gives_the_deleted_objects_and_leaves_the_others(clean, db, observe_queries):
    held = await seed()
    await Post.objects.filter(Post.id == held[3].id).delete()
    with observe_queries(db) as events:
        result = await Post.objects.of([held[2], held[0], held[3]]).delete()
    assert len(events) == 1
    assert result == [held[2], held[0]]
    assert [p.id for p in await Post.objects.order_by(Post.id)] == [held[1].id]


async def test_a_bound_write_checks_the_database_and_the_input(clean):
    held = await seed(2)
    with pytest.raises(TypeError):
        Post.objects.of([])
    assert await Post.objects.of(held).using("primary").update(views=3) == held
    assert not hasattr(Post.objects.of(held), "hard_delete")
    assert not hasattr(Post.objects.of(held), "undelete")


async def test_a_bound_write_above_the_parameter_cap_is_a_query_error(clean):
    alice = await User.objects.insert(email="alice@example.com", name="Alice")
    rows = [{"author": alice, "title": "p", "body": "b"} for _ in range(65_600)]
    held = await Post.objects.insert_many(rows).returning()
    for write in (lambda s: s.delete(), lambda s: s.update(title="x")):
        with pytest.raises(orm.QueryError, match="parameters"):
            await write(Post.objects.of(held))
    assert await Post.objects.count() == 65_600


SOURCE = (
    'model Doc {\n  id Int @id\n  title String\n  deleted DateTime? @soft_delete.deleted_at(mode: "application")\n}\n'
)


@pytest.fixture
async def docs():
    registry = orm.Registry()
    Doc = loads_soft_delete(SOURCE, registry=registry)["Doc"]
    db = await orm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        manager = Doc.objects.using(db)
        await manager.insert_many([{"id": i, "title": f"d{i}"} for i in range(1, 5)])
        yield Doc, db, await manager.order_by(Doc.id)
    finally:
        await db.drop_tables()
        await db.close()


soft_delete = pytest.mark.skipif(
    "soft-delete" not in CAPABILITIES, reason="requires a selected soft-delete native artifact"
)


@soft_delete
async def test_a_bound_delete_soft_deletes_and_fills_the_objects(docs):
    Doc, db, held = docs
    result = await Doc.objects.of([held[1], held[0]]).using(db).delete()
    assert result == [held[1], held[0]]
    assert held[0].deleted is not None and held[1].deleted is not None and held[2].deleted is None
    assert await Doc.objects.using(db).count() == 2
    assert await Doc.objects.of(held).using(db) == held[2:]


@soft_delete
async def test_the_soft_delete_members_are_on_the_bound_set(docs):
    Doc, db, held = docs
    await Doc.objects.of(held[:2]).using(db).delete()
    bound = Doc.objects.of(held).using(db)
    assert type(bound).__name__ == "DocBoundQuerySet"
    assert await bound.deleted_only() == held[:2]
    assert await bound.all_with_deleted() == held
    assert await bound.all_with_deleted().deleted_only().count() == 2
    assert await Doc.objects.of(held).using(db).count() == 2


@soft_delete
async def test_a_bound_undelete_restores_only_the_soft_deleted_rows(docs):
    Doc, db, held = docs
    await Doc.objects.of(held[:2]).using(db).delete()
    assert await Doc.objects.of(held).using(db).undelete() == held[:2]
    assert all(d.deleted is None for d in held)
    assert await Doc.objects.using(db).count() == 4


@soft_delete
async def test_a_bound_hard_delete_removes_the_rows_and_gives_the_objects(docs):
    Doc, db, held = docs
    await Doc.objects.of(held[:2]).using(db).delete()
    assert await Doc.objects.of([held[2], held[0]]).using(db).hard_delete() == [held[2], held[0]]
    assert held[0].deleted is not None and held[2].deleted is None
    assert [d.id for d in await Doc.objects.using(db).all_with_deleted().order_by(Doc.id)] == [2, 4]
    assert await Doc.objects.of(held).using(db).all_with_deleted() == [held[1], held[3]]


async def test_a_bound_update_of_two_objects_cannot_assign_a_key_field(clean):
    held = await seed(3)
    with pytest.raises(TypeError, match="key field"):
        await Post.objects.of(held[:2]).update(id=Post.id + 100)
    assert [p.id for p in await Post.objects.order_by(Post.id)] == [p.id for p in held]
    assert await Post.objects.of([held[2], held[2]]).update(views=1) == [held[2], held[2]]
    [moved] = await Post.objects.of([held[0]]).update(id=Post.id + 100)
    assert moved is held[0] and moved.id >= 100
