"""update() and delete() with no caller filter raise UnfilteredWriteError; all() and the connect option lift it."""

import pytest

import monorm
from artifact_caps import has_extension
from conftest import DATABASE_URL

SCHEMA = """
model Parent {
  id Int @id
  n Int @default(0)
  children Child[]
  @@map("wg_parent")
}
model Child {
  id Int @id
  parent_id Int
  parent Parent @relation(fields: [parent_id], references: [id])
  views Int @default(0)
  @@map("wg_child")
}
"""


async def open_db(provider, **options):
    registry = monorm.Registry()
    models = monorm.loads(f'datasource db {{\n provider = "{provider}"\n}}\n' + SCHEMA, registry=registry)
    try:
        db = await monorm.connect(
            "sqlite://:memory:" if provider == "sqlite" else DATABASE_URL, registry=registry, default=False, **options
        )
    except monorm.DatabaseError as error:
        if provider == "postgresql":
            pytest.skip(f"Postgres not reachable at {DATABASE_URL}: {error}")
        raise
    await db.drop_tables()
    await db.create_tables()
    Parent, Child = models["Parent"], models["Child"]
    await Parent.objects.using(db).insert_many([{"id": 1}, {"id": 2}])
    await Child.objects.using(db).insert_many(
        [{"id": 1, "parent_id": 1, "views": 1}, {"id": 2, "parent_id": 1, "views": 2}, {"id": 3, "parent_id": 2, "views": 3}]
    )
    return db, Parent, Child


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_a_write_with_no_filter_raises_before_any_sql(provider):
    db, Parent, Child = await open_db(provider)
    try:
        events = []
        remove = db.on_query(events.append)
        children = Child.objects.using(db)
        for write in (lambda q: q.update(views=0), lambda q: q.delete()):
            with pytest.raises(monorm.UnfilteredWriteError) as raised:
                await write(children)
            assert isinstance(raised.value, monorm.ORMError)
            assert ".all()" in str(raised.value) and "allow_unfiltered_writes" in str(raised.value)
            with pytest.raises(monorm.UnfilteredWriteError):
                write(children).inspect()
        remove()
        assert events == []
        assert await children.count() == 3
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_a_caller_filter_passes_the_guard(provider):
    db, Parent, Child = await open_db(provider)
    try:
        children = Child.objects.using(db)
        assert await children.filter(Child.id == 1).update(views=10) == 1
        assert await children.exclude(Child.id == 1).update(views=20) == 2
        parent = await Parent.objects.using(db).get(Parent.id == 1)
        assert await parent.children.update(views=30) == 2
        assert await parent.children.delete() == 2
        assert await children.count() == 1
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_all_marks_every_row_on_each_kind_of_query_set(provider):
    db, Parent, Child = await open_db(provider)
    try:
        children = Child.objects.using(db)
        parent = await Parent.objects.using(db).get(Parent.id == 1)
        assert await children.all().update(views=5) == 3
        assert await children.filter(Child.id > 0).all().update(views=6) == 3
        assert await children.all().filter(Child.id > 0).update(views=7) == 3
        assert await parent.children.all().update(views=8) == 2
        assert await parent.children.all().delete() == 2
        assert await children.all().delete() == 1
        assert await children.count() == 0
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_the_connect_option_turns_the_guard_off(provider):
    db, Parent, Child = await open_db(provider, allow_unfiltered_writes=True)
    try:
        children = Child.objects.using(db)
        assert await children.update(views=1) == 3
        assert await children.delete() == 3
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_writes_outside_the_guard_still_run(provider):
    db, Parent, Child = await open_db(provider)
    try:
        children = Child.objects.using(db)
        await children.insert(id=4, parent_id=2, views=0)
        assert await children.update_many([{"id": 1, "views": 9}, {"id": 2, "views": 9}]) == 2
        first = await children.get(Child.id == 1)
        await first.update(views=11)
        await first.delete()
        await db.execute("DELETE FROM wg_child")
        assert await children.count() == 0
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_a_filter_that_is_always_true_does_not_pass_the_guard(provider):
    db, Parent, Child = await open_db(provider)
    try:
        children = Child.objects.using(db)
        always_true = (
            children.filter(monorm.and_()),
            children.exclude(monorm.or_()),
            children.filter(Child.id.not_in([])),
            children.filter(monorm.and_(), monorm.and_()),
        )
        for qs in always_true:
            with pytest.raises(monorm.UnfilteredWriteError):
                await qs.update(views=0)
            with pytest.raises(monorm.UnfilteredWriteError):
                await qs.delete()
        assert await children.count() == 3
        assert await children.filter(monorm.and_(), Child.id == 1).update(views=10) == 1
        assert await children.filter(monorm.and_()).all().update(views=5) == 3
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_a_union_keeps_the_every_row_mark_of_either_side(provider):
    db, Parent, Child = await open_db(provider)
    try:
        some = Child.objects.using(db).filter(Child.id == 1)
        every = Child.objects.using(db).all()
        assert await (some | Child.objects.all()).update(views=1) == 3
        assert await (every | Child.objects.filter(Child.id == 1)).update(views=2) == 3
        assert await (every | Child.objects.all()).update(views=3) == 3
        with pytest.raises(monorm.UnfilteredWriteError):
            await (some | Child.objects).update(views=4)
        assert await Child.objects.using(db).filter(Child.views == 3).count() == 3
    finally:
        await db.close()


async def test_a_write_with_no_database_raises_not_connected_before_the_guard(monkeypatch):
    registry = monorm.Registry()
    models = monorm.loads('datasource db {\n provider = "sqlite"\n}\n' + SCHEMA, registry=registry)
    monkeypatch.setattr("monorm.db._default", None)
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False, allow_unfiltered_writes=True)
    try:
        for write in (lambda q: q.update(views=0), lambda q: q.delete()):
            with pytest.raises(monorm.NotConnected):
                await write(models["Child"].objects)
            with pytest.raises(monorm.NotConnected):
                write(models["Child"].objects).inspect()
    finally:
        await db.close()


SOFT_DELETE_SCHEMA = """
datasource db {
  provider = "sqlite"
}
model Doc {
  id Int @id
  title String
  @@soft_delete.deleted_at("deleted", mode: "application")
}
"""

EXTENSIONS_SCHEMA = """
datasource db {
  provider = "sqlite"
}
model Ledger {
  id Int @id
  shop_id Int
  active Boolean @default(true)
  @@query.filter("active", "active == true")
  @@query.filter("tenant", "shop_id == scope.shop")
}
model Doc {
  id Int @id
  title String
  @@soft_delete.deleted_at("deleted", mode: "application")
}
"""

MANY_TO_MANY_SCHEMA = """
datasource db {
  provider = "sqlite"
}
model Post {
  id Int @id
  tags Tag[] @relation(through: PostTag, through_fields: [post, tag])
}
model Tag {
  id Int @id
  name String
  posts Post[] @relation(through: PostTag, through_fields: [tag, post])
}
model PostTag {
  id Int @id @default(autoincrement())
  post_id Int
  tag_id Int
  post Post @relation(fields: [post_id], references: [id])
  tag Tag @relation(fields: [tag_id], references: [id])
}
"""


@pytest.mark.skipif(
    not (has_extension("monorm.query-defaults") and has_extension("monorm.soft-delete")),
    reason="requires the monorm.query-defaults and monorm.soft-delete packages",
)
async def test_a_query_default_the_scope_and_soft_delete_do_not_count_as_a_filter():
    registry = monorm.Registry()
    models = monorm.loads(EXTENSIONS_SCHEMA, registry=registry)
    Ledger, Doc = models["Ledger"], models["Doc"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await Ledger.objects.using(db).insert_many([{"id": 1, "shop_id": 1}, {"id": 2, "shop_id": 2}])
        await Doc.objects.using(db).insert_many([{"id": 1, "title": "a"}, {"id": 2, "title": "b"}])
        with monorm.scope(shop=1):
            with pytest.raises(monorm.UnfilteredWriteError):
                await Ledger.objects.using(db).update(active=True)
            with pytest.raises(monorm.UnfilteredWriteError):
                await Ledger.objects.using(db).delete()
            assert await Ledger.objects.using(db).all().update(active=True) == 1
        with pytest.raises(monorm.UnfilteredWriteError):
            await Doc.objects.using(db).delete()
        assert await Doc.objects.using(db).all().delete() == 2
    finally:
        await db.close()


async def test_a_many_to_many_set_counts_as_a_filter():
    registry = monorm.Registry()
    models = monorm.loads(MANY_TO_MANY_SCHEMA, registry=registry)
    Post, Tag, PostTag = models["Post"], models["Tag"], models["PostTag"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await Post.objects.using(db).insert_many([{"id": 1}])
        await Tag.objects.using(db).insert_many([{"id": 1, "name": "x"}, {"id": 2, "name": "y"}])
        await PostTag.objects.using(db).insert_many([{"post_id": 1, "tag_id": 1}])
        with pytest.raises(monorm.UnfilteredWriteError):
            await Tag.objects.using(db).update(name="z")
        post = await Post.objects.using(db).get(Post.id == 1)
        assert await post.tags.update(name="z") == 1
        assert [t.name for t in await Tag.objects.using(db).order_by(Tag.id)] == ["z", "y"]
    finally:
        await db.close()


@pytest.mark.skipif(not has_extension("monorm.soft-delete"), reason="requires the monorm.soft-delete package")
async def test_undelete_passes_the_guard_and_hard_delete_does_not():
    registry = monorm.Registry()
    models = monorm.loads(SOFT_DELETE_SCHEMA, registry=registry)
    Doc = models["Doc"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await Doc.objects.using(db).insert_many([{"id": 1, "title": "a"}, {"id": 2, "title": "b"}])
        assert await Doc.objects.using(db).all().delete() == 2
        # The verb names its rows (the soft-deleted ones), so it needs no .all().
        assert await Doc.objects.using(db).undelete() == 2
        with pytest.raises(monorm.UnfilteredWriteError):
            await Doc.objects.using(db).hard_delete()
        assert await Doc.objects.using(db).all().hard_delete() == 2
    finally:
        await db.close()
