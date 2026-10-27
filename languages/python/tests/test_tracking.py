"""Run with an artifact that selects the timestamps, soft-delete or locking extension."""

import os
from datetime import datetime, timezone
from typing import NamedTuple

import pytest

import orm
from artifact_caps import has_extension

DATABASES = ["sqlite", "postgres"]
OLD = datetime(2000, 1, 1, tzinfo=timezone.utc)


def requires(name: str) -> pytest.MarkDecorator:
    """Skip unless the artifact or an installed package has the extension, such as `orm.locking`."""
    return pytest.mark.skipif(not has_extension(name), reason=f"requires {name} in the native artifact")


async def connect(dialect: str, source: str) -> tuple[orm.Database, dict]:
    if dialect == "sqlite":
        source = 'datasource db { provider = "sqlite" }\n' + source
    registry = orm.Registry()
    models = orm.loads(source, registry=registry)
    url = (
        "sqlite://:memory:"
        if dialect == "sqlite"
        else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    )
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    return db, models


def as_utc(value: datetime) -> datetime:
    return value if value.tzinfo else value.replace(tzinfo=timezone.utc)


@requires("orm.timestamps")
@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("mode", ["database", "application"])
async def test_updated_at_is_set_on_orm_updates_and_by_the_trigger(dialect, mode):
    db, models = await connect(
        dialect,
        f"""
model Note {{
  id      Int      @id
  views   Int      @default(0)
  @@timestamps.updated_at("changed", mode: "{mode}")
  @@map("tracking_notes")
}}
""",
    )
    Note = models["Note"]
    try:
        note = await Note.objects.using(db).insert(id=1, changed=OLD)
        await Note.objects.using(db).insert(id=2, changed=OLD)
        assert note.changed == OLD
        await note.update(views=1)
        assert as_utc(note.changed) > OLD
        (row,) = await Note.objects.using(db).filter(Note.id == 2).update(views=2).returning()
        assert as_utc(row.changed) > OLD
        await Note.objects.using(db).update_many([{"id": 1, "views": 3}])
        await note.update(changed=OLD)
        assert note.changed == OLD
        await db.execute("UPDATE tracking_notes SET views = 4 WHERE id = 1")
        await note.load(reload=True)
        assert (as_utc(note.changed) > OLD) is (mode == "database")
    finally:
        await db.drop_tables()
        await db.close()


SOFT_DELETE = """
model Author {{
  id      Int       @id
  books   Book[]
  @@soft_delete.deleted_at("deleted", mode: "{mode}")
  @@map("tracking_authors")
}}
model Book {{
  id        Int       @id
  author_id Int
  author    Author    @relation(fields: [author_id], references: [id], onDelete: Cascade)
  @@soft_delete.deleted_at("removed", mode: "{mode}")
  @@map("tracking_books")
}}
"""


@requires("orm.soft-delete")
@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("mode", ["database", "application"])
async def test_soft_delete_updates_instead_of_deleting(dialect, mode):
    db, models = await connect(dialect, SOFT_DELETE.format(mode=mode))
    Author, Book = models["Author"], models["Book"]
    try:
        authors = Author.objects.using(db)
        await authors.insert_many([{"id": 1}, {"id": 2}, {"id": 3}])
        await Book.objects.using(db).insert_many([{"id": 1, "author_id": 1}, {"id": 2, "author_id": 2}])
        assert await authors.filter(Author.id == 1).delete() == 1
        assert await authors.filter(Author.id == 1).delete() == 0
        (row,) = await authors.filter(Author.id == 2).delete().returning()
        assert row.id == 2 and row.deleted is not None
        assert await authors.count() == 1
        assert [a.id for a in await authors.deleted_only().order_by(Author.id)] == [1, 2]
        assert await authors.all_with_deleted().count() == 3
        removed = [b.id for b in await Book.objects.using(db).deleted_only().order_by(Book.id)]
        assert removed == ([1, 2] if mode == "database" else [])
        assert await authors.filter(Author.id == 1).undelete() == 1
        restored = await authors.get(Author.id == 1)
        assert restored.deleted is None
        await restored.delete()
        assert (await authors.all_with_deleted().get(Author.id == 1)).deleted is not None
        assert await authors.get(Author.id == 1) is None
        await restored.undelete()
        assert restored.deleted is None
        await db.execute("DELETE FROM tracking_books")
        await db.execute("DELETE FROM tracking_authors WHERE id = 3")
        assert await authors.all_with_deleted().filter(Author.id == 3).count() == (1 if mode == "database" else 0)
        assert await authors.filter(Author.id == 3).count() == 0
        assert await authors.filter(Author.id == 2).hard_delete() == 1
        assert await authors.filter(Author.id == 1).hard_delete() == (0 if mode == "database" else 1)
    finally:
        await db.drop_tables()
        await db.close()


VERSIONED_SOFT_DELETE = """
model Doc {
  id      Int       @id
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@locking.version
  @@map("tracking_soft_docs")
}
"""


@requires("orm.soft-delete")
@requires("orm.locking")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_soft_deleted_instance_can_be_undeleted(dialect):
    db, models = await connect(dialect, VERSIONED_SOFT_DELETE)
    Doc = models["Doc"]
    try:
        await Doc.objects.using(db).insert_many([{"id": 1}])
        doc = await Doc.objects.using(db).get(Doc.id == 1)
        await doc.delete()
        assert doc.version == 1 and doc.deleted is not None
        await doc.undelete()
        assert doc.version == 2 and doc.deleted is None
    finally:
        await db.drop_tables()
        await db.close()


TENANT_SOFT_DELETE = """
model Doc {
  id      Int       @id
  shop    Int
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@query.filter("tenant", "shop == scope.shop")
  @@map("tracking_tenant_docs")
}
"""


@requires("orm.soft-delete")
@requires("orm.query-defaults")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_the_soft_delete_methods_bypass_only_the_deleted_default(dialect):
    db, models = await connect(dialect, TENANT_SOFT_DELETE)
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert_many([{"id": i, "shop": 1 if i < 3 else 2} for i in (1, 2, 3, 4)])
        with orm.scope(shop=2):
            assert await docs.filter(Doc.id == 3).delete() == 1
        with orm.scope(shop=1):
            assert await docs.filter(Doc.id == 1).delete() == 1
            assert [d.id for d in await docs.order_by(Doc.id)] == [2]
            assert [d.id for d in await docs.all_with_deleted().order_by(Doc.id)] == [1, 2]
            assert [d.id for d in await docs.deleted_only()] == [1]
            assert await docs.undelete() == 1
            assert await docs.count() == 2
            assert await docs.delete() == 2
            assert await docs.hard_delete() == 2
        # Shop 2's deleted row survived the shop 1 undelete and hard delete.
        rows = await docs.without_defaults().order_by(Doc.id)
        assert [(d.id, d.deleted is not None) for d in rows] == [(3, True), (4, False)]
        for statement in (
            docs.undelete(),
            docs.hard_delete(),
            docs.all_with_deleted().count(),
            docs.deleted_only().count(),
        ):
            with pytest.raises(Exception, match=r'default "tenant" from orm.query-defaults .* reads scope.shop'):
                await statement
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.soft-delete")
@requires("orm.query-defaults")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_an_instance_hard_delete_also_drops_the_tenant_filter(dialect):
    db, models = await connect(dialect, TENANT_SOFT_DELETE)
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert_many([{"id": 1, "shop": 1}, {"id": 2, "shop": 2}])
        with orm.scope(shop=2):
            doc = await docs.get(Doc.id == 2)
        # The instance uses a bare bypass: no scope is set, and the shop 2 row is gone.
        await doc.hard_delete()
        with orm.scope(shop=1):
            assert await docs.all_with_deleted().count() == 1
        assert [d.id for d in await docs.without_defaults()] == [1]
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.soft-delete")
@requires("orm.locking")
@requires("orm.query-defaults")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_an_instance_update_after_a_query_set_soft_delete_raises(dialect):
    db, models = await connect(dialect, VERSIONED_SOFT_DELETE)
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert_many([{"id": 1}])
        doc = await docs.get(Doc.id == 1)
        assert await docs.filter(Doc.id == 1).delete() == 1
        with pytest.raises(orm.VersionConflict):
            await doc.update(deleted=None)
    finally:
        await db.drop_tables()
        await db.close()


CASCADE_VERSIONED = """
model Author {
  id      Int       @id
  books   Book[]
  @@soft_delete.deleted_at("deleted", mode: "database")
  @@map("tracking_authors")
}
model Book {
  id        Int       @id
  author_id Int
  author    Author    @relation(fields: [author_id], references: [id], onDelete: Cascade)
  @@soft_delete.deleted_at("removed", mode: "database")
  @@locking.version
  @@map("tracking_books")
}
"""


@requires("orm.soft-delete")
@requires("orm.locking")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_hard_delete_of_a_soft_deleted_parent_leaves_no_orphan(dialect):
    db, models = await connect(dialect, CASCADE_VERSIONED)
    Author, Book = models["Author"], models["Book"]
    try:
        authors, books = Author.objects.using(db), Book.objects.using(db)
        await authors.insert_many([{"id": 1}])
        assert await authors.filter(Author.id == 1).delete() == 1
        await books.insert_many([{"id": 1, "author_id": 1}])
        author = await authors.all_with_deleted().get(Author.id == 1)
        if dialect == "postgres":
            await author.hard_delete()
            assert await books.all_with_deleted().count() == 0
            assert await authors.all_with_deleted().count() == 0
        else:
            with pytest.raises(Exception, match="FOREIGN KEY constraint failed"):
                await author.hard_delete()
            assert await books.all_with_deleted().count() == 1
            assert await authors.all_with_deleted().count() == 1
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.soft-delete")
@requires("orm.locking")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_the_soft_delete_triggers_bump_the_version(dialect):
    db, models = await connect(dialect, CASCADE_VERSIONED)
    Author, Book = models["Author"], models["Book"]
    try:
        authors, books = Author.objects.using(db), Book.objects.using(db)
        await authors.insert_many([{"id": 1}, {"id": 2}])
        await books.insert_many([{"id": 1, "author_id": 1}, {"id": 2, "author_id": 2}])
        cascaded = await books.get(Book.id == 1)
        raw = await books.get(Book.id == 2)
        assert await authors.filter(Author.id == 1).delete() == 1
        await db.execute("DELETE FROM tracking_books WHERE id = 2")
        stored = {b.id: b.version for b in await books.all_with_deleted()}
        assert stored == {1: 1, 2: 1}
        for stale in (cascaded, raw):
            with pytest.raises(orm.VersionConflict):
                await stale.update(removed=None)
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.soft-delete")
@requires("orm.locking")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_second_delete_of_the_same_instance_raises_nothing(dialect):
    db, models = await connect(dialect, VERSIONED_SOFT_DELETE)
    Doc = models["Doc"]
    try:
        await Doc.objects.using(db).insert_many([{"id": 1}])
        doc = await Doc.objects.using(db).get(Doc.id == 1)
        await doc.delete()
        await doc.delete()
        assert doc.version == 1
        stale = await Doc.objects.using(db).all_with_deleted().get(Doc.id == 1)
        await stale.undelete()
        with pytest.raises(orm.VersionConflict):
            await doc.delete()
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.locking")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_stale_instance_write_raises_version_conflict(dialect):
    db, models = await connect(
        dialect,
        """
model Doc {
  id      Int    @id
  title   String
  @@locking.version
  @@map("tracking_docs")
}
""",
    )
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        first = await docs.insert(id=1, title="a")
        second = await docs.get(Doc.id == 1)
        await first.update(title="b")
        assert first.version == 1 and first.title == "b"
        with pytest.raises(orm.VersionConflict):
            await second.update(title="c")
        with pytest.raises(orm.VersionConflict):
            await second.delete()
        assert (await docs.get(Doc.id == 1)).title == "b"
        assert await docs.update(title="d") == 1
        assert (await docs.get(Doc.id == 1)).version == 2
        await second.load(reload=True)
        await second.update(title="e")
        assert second.version == 3
        await second.delete()
        with pytest.raises(Doc.DoesNotExist):
            await first.update(title="f")
        assert not hasattr(docs, "deleted_only")
    finally:
        await db.drop_tables()
        await db.close()


def upsert_doc(mode: str | None = None, *, version: bool = False) -> str:
    changed = f'@@timestamps.updated_at("changed", mode: "{mode}")' if mode is not None else ""
    version_field = '@@locking.version' if version else ""
    return f"""
model Doc {{
  id      Int      @id
  title   String
  {changed}
  {version_field}
  @@map("tracking_upsert_docs")
}}
"""


@requires("orm.locking")
@requires("orm.timestamps")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_an_upsert_bumps_the_version(dialect):
    db, models = await connect(dialect, upsert_doc("application", version=True))
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert(id=1, title="a")
        stale = await docs.get(Doc.id == 1)
        await docs.insert(id=1, title="b").on_conflict(Doc.id, update=True)
        assert (await docs.get(Doc.id == 1)).version == 1
        await docs.insert(id=1, title="c").on_conflict(Doc.id, update=True, update_fields=[Doc.title])
        await docs.insert(id=1, title="d").on_conflict(Doc.id, update=True, update_values={"title": "e"})
        assert (await docs.get(Doc.id == 1)).version == 3
        with pytest.raises(orm.VersionConflict):
            await stale.update(title="x")
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.timestamps")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_an_upsert_sets_updated_at_in_application_mode(dialect):
    db, models = await connect(dialect, upsert_doc("application"))
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert(id=1, title="a", changed=OLD)
        await docs.insert(id=1, title="b").on_conflict(Doc.id, update=True)
        assert as_utc((await docs.get(Doc.id == 1)).changed) > OLD
        await docs.update(changed=OLD)
        await docs.insert(id=1, title="c", changed=OLD).on_conflict(
            Doc.id, update=True, update_fields=[Doc.title, Doc.changed]
        )
        assert as_utc((await docs.get(Doc.id == 1)).changed) == OLD
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.timestamps")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_the_default_upsert_update_list_drops_updated_at(dialect):
    db, models = await connect(dialect, upsert_doc("application"))
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert(id=1, title="a", changed=OLD)
        await docs.insert(id=1, title="b", changed=OLD).on_conflict(Doc.id, update=True)
        row = await docs.get(Doc.id == 1)
        assert row.title == "b" and as_utc(row.changed) > OLD
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.locking")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_an_update_true_upsert_does_not_reset_the_version(dialect):
    db, models = await connect(dialect, upsert_doc(version=True))
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert(id=1, title="a")
        await docs.filter(Doc.id == 1).update(title="a2")
        await docs.insert(id=1, title="b", version=0).on_conflict(Doc.id, update=True)
        assert (await docs.get(Doc.id == 1)).version == 2
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.timestamps")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_an_upsert_fires_the_updated_at_trigger_in_database_mode(dialect):
    db, models = await connect(dialect, upsert_doc("database"))
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert(id=1, title="a", changed=OLD)
        await docs.insert(id=1, title="b").on_conflict(Doc.id, update=True)
        assert as_utc((await docs.get(Doc.id == 1)).changed) > OLD
    finally:
        await db.drop_tables()
        await db.close()


FILTERED_JOIN_MODEL = """
model Post {
  id   Int   @id
  tags Tag[] @relation(through: PostTag, through_fields: [post, tag])
  @@map("tracking_join_posts")
}
model Tag {
  id    Int    @id
  name  String
  posts Post[] @relation(through: PostTag, through_fields: [tag, post])
  @@map("tracking_join_tags")
}
model PostTag {
  id      Int       @id
  post_id Int
  tag_id  Int
  post    Post      @relation(fields: [post_id], references: [id])
  tag     Tag       @relation(fields: [tag_id], references: [id])
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("tracking_join_post_tags")
}
"""


@requires("orm.soft-delete")
@requires("orm.query-defaults")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_soft_deleted_join_row_is_hidden_from_loads_and_filters(dialect):
    db, models = await connect(dialect, FILTERED_JOIN_MODEL)
    Post, Tag, PostTag = models["Post"], models["Tag"], models["PostTag"]
    try:
        await Post.objects.using(db).insert_many([{"id": 1}])
        await Tag.objects.using(db).insert_many([{"id": 1, "name": "a"}, {"id": 2, "name": "b"}])
        await PostTag.objects.using(db).insert_many(
            [{"id": 1, "post_id": 1, "tag_id": 1}, {"id": 2, "post_id": 1, "tag_id": 2}]
        )
        assert await PostTag.objects.using(db).filter(PostTag.id == 2).delete() == 1
        posts = Post.objects.using(db)
        (post,) = await posts.load(Post.tags)
        assert [t.name for t in post.tags.cached] == ["a"]
        assert await posts.filter(Post.tags.name == "b").count() == 0
        assert await posts.filter(Post.tags.name == "a").count() == 1
        assert await posts.without_defaults().filter(Post.tags.name == "b").count() == 1
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.proxy")
@requires("orm.locking")
@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("operation", ["update", "delete"])
@pytest.mark.parametrize("mode", [None, "application", "database"])
async def test_a_stale_proxy_instance_checks_the_loaded_version(dialect, operation, mode):
    if mode is not None and not has_extension("orm.soft-delete"):
        pytest.skip("requires soft-delete")
    source = upsert_doc(version=True)
    if mode is not None:
        source = source.replace(
            "  title   String",
            f'  title   String\n  @@soft_delete.deleted_at("deleted", mode: "{mode}")',
        )
    db, models = await connect(
        dialect,
        source
        + """
model Draft {
  @@proxy.of(Doc)
}
model Review {
  @@proxy.of(Draft)
}
""",
    )
    Doc, Review = models["Doc"], models["Review"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert_many([{"id": 1, "title": "original"}])
        stale = await Review.objects.using(db).get(Review.id == 1)
        await docs.filter(Doc.id == 1).update(title="other writer")
        with pytest.raises(orm.VersionConflict):
            if operation == "update":
                await stale.update(title="stale writer")
            else:
                await stale.delete()
        stored = await docs.get(Doc.id == 1)
        assert stored.title == "other writer" and stored.version == 1
        await stale.load(reload=True)
        await stale.update(title="fresh writer")
        assert stale.version == 2
        await stale.delete()
        if mode is None:
            assert await docs.count() == 0
        else:
            stored = await docs.without_defaults("deleted").get(Doc.id == 1)
            assert stored.deleted is not None and stored.version == 3
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.locking")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_an_instance_rejects_an_explicit_version(dialect):
    db, models = await connect(dialect, upsert_doc(version=True))
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        doc = await docs.insert(id=1, title="original")
        with pytest.raises(TypeError, match=r"Doc.objects.filter.*update\(version="):
            await doc.update(title="changed", version=7)
        stored = await docs.get(Doc.id == 1)
        assert stored.title == "original" and stored.version == doc.version == 0
        await docs.filter(Doc.id == 1).update(version=7)
        assert (await docs.get(Doc.id == 1)).version == 7
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.composition")
@pytest.mark.parametrize(
    "extension,field,attribute",
    [
        ("orm.timestamps", "changed", '@@timestamps.updated_at("changed")'),
        ("orm.soft-delete", "deleted", '@@soft_delete.deleted_at("deleted")'),
        ("orm.locking", "version", '@@locking.version'),
    ],
)
@pytest.mark.parametrize("owner", ["Person", "Employee", "Manager"])
def test_composed_ownership_chain_rejects_tracking_records(extension, field, attribute, owner):
    if not has_extension(extension):
        pytest.skip(f"requires {extension}")
    source = f"""
model Person {{
 id Int @id
 name String
 {attribute if owner == "Person" else ""}
}}
model Employee {{
 salary Int
 {attribute if owner == "Employee" else ""}
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
}}
model Manager {{
 level Int
 {attribute if owner == "Manager" else ""}
 @@composition.model(parent: "Employee", parentRef: "employee", childRef: "manager")
}}
"""
    with pytest.raises(orm.SchemaError) as error:
        orm.loads(source, registry=orm.Registry())
    message = str(error.value)
    assert owner in message and field in message and extension in message
    assert "Employee" in message or "Manager" in message


class Tracking(NamedTuple):
    extension: str
    attribute: str
    default: str
    custom: str


TRACKING = [
    Tracking("orm.timestamps", "@@timestamps.updated_at", "updated_at", "edited_at"),
    Tracking("orm.soft-delete", "@@soft_delete.deleted_at", "deleted_at", "removed_at"),
    Tracking("orm.locking", "@@locking.version", "version", "revision"),
]
each_tracking = pytest.mark.parametrize("tracking", TRACKING, ids=lambda tracking: tracking.default)


def tracking_schema(attribute: str) -> str:
    return f"""
model Story {{
  id Int @id
  {attribute}
  @@map("tracking_story")
}}
"""


@each_tracking
def test_the_model_attribute_adds_the_field_with_its_default_or_given_name(tracking):
    if not has_extension(tracking.extension):
        pytest.skip(f"requires {tracking.extension}")
    fields = lambda source: list(orm.loads(source, registry=orm.Registry())["Story"]._meta.fields)
    assert fields(tracking_schema(tracking.attribute)) == ["id", tracking.default]
    assert fields(tracking_schema(f'{tracking.attribute}("{tracking.custom}")')) == ["id", tracking.custom]


@requires("orm.timestamps")
@requires("orm.soft-delete")
@requires("orm.locking")
async def test_the_added_fields_have_the_documented_types_and_defaults():
    db, models = await connect(
        "sqlite",
        """
model Story {
  id Int @id
  @@timestamps.updated_at
  @@soft_delete.deleted_at
  @@locking.version
  @@map("tracking_story_all")
}
""",
    )
    try:
        row = await models["Story"].objects.using(db).insert(id=1)
        assert row.updated_at is not None and row.deleted_at is None and row.version == 0
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.timestamps")
@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("upsert,kept", [(None, False), ("drop", False), ("pick", True)])
async def test_timestamps_upsert_pick_keeps_the_field_in_the_default_upsert_list(dialect, upsert, kept):
    argument = "" if upsert is None else f', upsert: "{upsert}"'
    db, models = await connect(
        dialect,
        f"""
model Doc {{
  id    Int    @id
  title String
  @@timestamps.updated_at("changed", mode: "application"{argument})
  @@map("tracking_story_upsert")
}}
""",
    )
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert(id=1, title="a", changed=OLD)
        await docs.insert(id=1, title="b", changed=OLD).on_conflict(Doc.id, update=True)
        row = await docs.get(Doc.id == 1)
        assert row.title == "b"
        assert (as_utc(row.changed) == OLD) is kept
        await docs.insert(id=1, title="c", changed=OLD).on_conflict(Doc.id, update=True, update_fields=[Doc.title, Doc.changed])
        assert as_utc((await docs.get(Doc.id == 1)).changed) == OLD
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.locking")
@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("upsert,kept", [(None, False), ("drop", False), ("pick", True)])
async def test_locking_upsert_pick_keeps_the_field_in_the_default_upsert_list(dialect, upsert, kept):
    argument = "" if upsert is None else f', upsert: "{upsert}"'
    db, models = await connect(
        dialect,
        f"""
model Doc {{
  id    Int    @id
  title String
  @@locking.version("rev"{argument})
  @@map("tracking_locking_upsert")
}}
""",
    )
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert(id=1, title="a", rev=7)
        await docs.insert(id=1, title="b", rev=3).on_conflict(Doc.id, update=True)
        row = await docs.get(Doc.id == 1)
        assert (row.title, row.rev) == ("b", 3 if kept else 8)
        await docs.insert(id=1, title="c", rev=5).on_conflict(Doc.id, update=True, update_fields=[Doc.title, Doc.rev])
        assert (await docs.get(Doc.id == 1)).rev == 5
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.timestamps")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_timestamps_map_argument_keeps_the_live_column(dialect):
    db, models = await connect(dialect, 'model Story {\n  id Int @id\n  @@timestamps.updated_at(map: "_updated_at")\n  @@map("tracking_story_map_ts")\n}\n')
    try:
        stories = models["Story"].objects.using(db)
        await stories.insert_many([{"id": 1}, {"id": 2, "updated_at": OLD}])
        await db.execute("DELETE FROM tracking_story_map_ts WHERE \"_updated_at\" IS NOT NULL AND id = 1")
        assert [s.id for s in await stories.all()] == [2]
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.soft-delete")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_soft_delete_map_argument_keeps_the_live_column(dialect):
    db, models = await connect(dialect, 'model Story {\n  id Int @id\n  @@soft_delete.deleted_at(map: "_deleted_at")\n  @@map("tracking_story_map_sd")\n}\n')
    try:
        stories = models["Story"].objects.using(db)
        await stories.insert_many([{"id": 1}, {"id": 2}, {"id": 3}])
        assert await stories.filter(models["Story"].id == 1).delete() == 1
        await db.execute('UPDATE tracking_story_map_sd SET "_deleted_at" = CURRENT_TIMESTAMP WHERE id = 2')
        assert [s.id for s in await stories.all()] == [3]
        await db.execute('DELETE FROM tracking_story_map_sd WHERE "_deleted_at" IS NOT NULL')
        assert [s.id for s in await stories.all_with_deleted()] == [3]
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.locking")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_locking_map_argument_keeps_the_live_column(dialect):
    db, models = await connect(dialect, 'model Story {\n  id Int @id\n  @@locking.version(map: "rev")\n  @@map("tracking_story_map_lk")\n}\n')
    try:
        stories = models["Story"].objects.using(db)
        await stories.insert_many([{"id": 1}, {"id": 2, "version": 3}])
        await db.execute("UPDATE tracking_story_map_lk SET rev = 7 WHERE id = 1")
        assert (await stories.get(models["Story"].id == 1)).version == 7
        await db.execute("DELETE FROM tracking_story_map_lk WHERE rev = 3")
        assert [s.id for s in await stories.all()] == [1]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize(
    "extension,attribute,old,field,line,note",
    [
        ("orm.timestamps", "timestamps.updated_at", "changed DateTime @default(now()) @timestamps.updated_at", "changed", '@@timestamps.updated_at("changed")', ""),
        ("orm.timestamps", "timestamps.updated_at", 'updated_at DateTime @default(now()) @timestamps.updated_at(mode: "application")', "updated_at", '@@timestamps.updated_at(mode: "application")', ""),
        ("orm.timestamps", "timestamps.updated_at", "editedAt DateTime @default(now()) @timestamps.updated_at", "editedAt", '@@timestamps.updated_at("edited_at", map: "editedAt")', ""),
        ("orm.timestamps", "timestamps.updated_at", 'changed DateTime @default(now()) @map("_changed") @timestamps.updated_at(mode: "database")', "changed", '@@timestamps.updated_at("changed", map: "_changed", mode: "database")', ""),
        ("orm.soft-delete", "soft_delete.deleted_at", "deleted_at DateTime? @soft_delete.deleted_at", "deleted_at", "@@soft_delete.deleted_at", ""),
        ("orm.soft-delete", "soft_delete.deleted_at", 'removed DateTime? @soft_delete.deleted_at(mode: "database")', "removed", '@@soft_delete.deleted_at("removed", mode: "database")', ""),
        ("orm.soft-delete", "soft_delete.deleted_at", "_deleted_at DateTime? @soft_delete.deleted_at", "_deleted_at", '@@soft_delete.deleted_at(map: "_deleted_at")', ""),
        ("orm.soft-delete", "soft_delete.deleted_at", 'deleted_at DateTime? @map("_deleted_at") @soft_delete.deleted_at', "deleted_at", '@@soft_delete.deleted_at(map: "_deleted_at")', ""),
        ("orm.locking", "locking.version", "version Int @default(0) @locking.version", "version", "@@locking.version", ""),
        ("orm.locking", "locking.version", "rev Int @default(0) @locking.version", "rev", '@@locking.version("rev")', ""),
        ("orm.locking", "locking.version", "version BigInt @default(0) @locking.version", "version", "@@locking.version", "; the added field has the type Int, not BigInt; change the type by hand"),
    ],
)
def test_the_old_field_attribute_fails_with_the_replacement_line(extension, attribute, old, field, line, note):
    if not has_extension(extension):
        pytest.skip(f"requires {extension}")
    source = f"model Story {{\n  id Int @id\n  {old}\n}}\n"
    with pytest.raises(orm.SchemaError) as error:
        orm.loads(source, registry=orm.Registry())
    assert str(error.value).endswith(
        f"@{attribute}: a field attribute is no longer supported; delete the field Story.{field} and write {line} in the model block{note}"
    )


@each_tracking
def test_a_field_of_the_same_name_is_an_error_that_names_both(tracking):
    if not has_extension(tracking.extension):
        pytest.skip(f"requires {tracking.extension}")
    source = f'model Story {{\n  id Int @id\n  {tracking.custom} Int\n  {tracking.attribute}("{tracking.custom}")\n}}\n'
    with pytest.raises(orm.SchemaError) as error:
        orm.loads(source, registry=orm.Registry())
    message = str(error.value)
    assert f"Story.{tracking.custom}" in message
    assert tracking.attribute.removeprefix("@@") in message and "already exists" in message


@each_tracking
def test_a_field_on_the_same_column_is_an_error_that_names_both(tracking):
    if not has_extension(tracking.extension):
        pytest.skip(f"requires {tracking.extension}")
    source = f'model Story {{\n  id Int @id\n  taken Int @map("{tracking.default}")\n  {tracking.attribute}\n}}\n'
    with pytest.raises(orm.SchemaError) as error:
        orm.loads(source, registry=orm.Registry())
    message = str(error.value)
    assert f"adds the field Story.{tracking.default}, but the field Story.taken already uses the column {tracking.default}" in message


def inherited_schema(attributes: str) -> str:
    return f"""
model Base {{
  id    Int    @id
  title String
  {attributes}
  @@inherit.abstract
}}
model Mid {{
  @@inherit.extends(["Base"])
  @@inherit.abstract
}}
model Child {{
  name String?
  @@inherit.extends(["Base"])
  @@map("tracking_inherited_child")
}}
model Grandchild {{
  @@inherit.extends(["Mid"])
  @@map("tracking_inherited_grandchild")
}}
"""


@requires("orm.inherit")
@each_tracking
def test_a_tracking_attribute_on_a_base_reaches_each_concrete_child(tracking):
    if not has_extension(tracking.extension):
        pytest.skip(f"requires {tracking.extension}")
    models = orm.loads(inherited_schema(tracking.attribute), registry=orm.Registry())
    assert sorted(models) == ["Child", "Grandchild"]
    for name in models:
        assert tracking.default in models[name]._meta.fields


@requires("orm.inherit")
@requires("orm.timestamps")
@requires("orm.soft-delete")
@requires("orm.locking")
@pytest.mark.parametrize("name", ["Child", "Grandchild"])
async def test_a_concrete_child_maintains_the_tracking_fields_of_its_base(name):
    attributes = '@@timestamps.updated_at("changed", mode: "application")\n  @@soft_delete.deleted_at(mode: "application")\n  @@locking.version'
    db, models = await connect("sqlite", inherited_schema(attributes))
    Model = models[name]
    try:
        rows = Model.objects.using(db)
        await rows.insert(id=1, title="a", changed=OLD)
        await rows.filter(Model.id == 1).update(title="b")
        row = await rows.get(Model.id == 1)
        assert (row.title, row.version) == ("b", 1)
        assert as_utc(row.changed) > OLD
        assert await rows.filter(Model.id == 1).delete() == 1
        assert await rows.count() == 0
        assert await rows.all_with_deleted().count() == 1
    finally:
        await db.drop_tables()
        await db.close()


UPSERT_SOFT_DELETE = """
model Doc {
  id      Int       @id
  title   String
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("tracking_upsert_soft_docs")
}
"""


@requires("orm.soft-delete")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_an_upsert_onto_a_soft_deleted_row_changes_nothing(dialect):
    db, models = await connect(dialect, UPSERT_SOFT_DELETE)
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert_many([{"id": 1, "title": "a"}])
        await docs.filter(Doc.id == 1).delete()
        await docs.insert(id=1, title="b").on_conflict(Doc.id, update=True)
        (row,) = await docs.without_defaults().filter(Doc.id == 1)
        assert row.title == "a" and row.deleted is not None
        assert await docs.count() == 0
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.soft-delete")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_query_set_bypass_lets_an_upsert_update_a_soft_deleted_row(dialect):
    db, models = await connect(dialect, UPSERT_SOFT_DELETE)
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        await docs.insert_many([{"id": 1, "title": "a"}])
        await docs.filter(Doc.id == 1).delete()
        await docs.without_defaults("deleted").insert(id=1, title="b").on_conflict(Doc.id, update=True)
        (row,) = await docs.without_defaults().filter(Doc.id == 1)
        assert row.title == "b" and row.deleted is not None
        with pytest.raises(orm.QueryError, match='unknown default "nope"'):
            await docs.without_defaults("nope").insert(id=2, title="c")
    finally:
        await db.drop_tables()
        await db.close()


@requires("orm.soft-delete")
@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("mode", ["application", "database"])
async def test_add_restores_a_soft_deleted_link_and_inserts_the_missing_ones(dialect, mode):
    source = FILTERED_JOIN_MODEL.replace('mode: "application"', f'mode: "{mode}"').replace(
        "model PostTag {\n  id      Int ", "model PostTag {\n  id      Int @default(autoincrement()) "
    )
    db, models = await connect(dialect, source)
    Post, Tag, PostTag = models["Post"], models["Tag"], models["PostTag"]
    try:
        await Post.objects.using(db).insert_many([{"id": 1}, {"id": 2}])
        await Tag.objects.using(db).insert_many([{"id": i, "name": str(i)} for i in (1, 2, 3)])
        post = await Post.objects.using(db).get(Post.id == 1)
        await post.tags.add(1, 2)
        await post.tags.remove(2)
        assert [t.id for t in await post.tags.order_by(Tag.id)] == [1]
        await post.tags.add(1, 2, 3)
        assert [t.id for t in await post.tags.order_by(Tag.id)] == [1, 2, 3]
        links = await PostTag.objects.using(db).without_defaults().order_by(PostTag.tag_id)
        assert [(link.tag_id, link.deleted) for link in links] == [(1, None), (2, None), (3, None)]
        await post.tags.set([2])
        await post.tags.set([1, 2])
        links = await PostTag.objects.using(db).without_defaults().order_by(PostTag.tag_id)
        assert [(link.tag_id, link.deleted is None) for link in links] == [(1, True), (2, True), (3, False)]
    finally:
        await db.drop_tables()
        await db.close()


CLASHING_DEFAULT = """
model Doc {
  id      Int       @id
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@query.filter("deleted", "id > 0")
  @@map("tracking_clash_docs")
}
"""


@requires("orm.soft-delete")
@requires("orm.query-defaults")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_user_default_named_deleted_clashes_with_the_soft_delete_default(dialect):
    with pytest.raises(Exception, match=r'default name is set by both orm\.soft-delete default "deleted" and orm\.query-defaults default "deleted"'):
        await connect(dialect, CLASHING_DEFAULT)


REFERENCE_TO_SOFT_DELETED = """
model Author {
  id      Int       @id
  books   Book[]
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("tracking_ref_authors")
}
model Book {
  id        Int    @id
  author_id Int
  author    Author @relation(fields: [author_id], references: [id])
  @@map("tracking_ref_books")
}
"""


@requires("orm.soft-delete")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_reference_to_a_soft_deleted_row_loads_none(dialect):
    db, models = await connect(dialect, REFERENCE_TO_SOFT_DELETED)
    Author, Book = models["Author"], models["Book"]
    try:
        await Author.objects.using(db).insert_many([{"id": 1}, {"id": 2}])
        await Book.objects.using(db).insert_many([{"id": 1, "author_id": 1}, {"id": 2, "author_id": 2}])
        assert await Author.objects.using(db).filter(Author.id == 2).delete() == 1
        joined = await Book.objects.using(db).load(Book.author).order_by(Book.id)
        assert [b.author.id if b.author else None for b in joined] == [1, None]
        for book_id, expected in ((1, 1), (2, None)):
            book = await Book.objects.using(db).get(Book.id == book_id)
            await book.load(Book.author)
            assert (book.author.id if book.author else None) == expected
    finally:
        await db.drop_tables()
        await db.close()
