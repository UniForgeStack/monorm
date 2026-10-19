"""Run with an artifact that selects the timestamps, soft-delete or locking extension."""

import json
import os
from datetime import datetime, timezone

import pytest

import orm
from orm import _native
from soft_delete_ext import loads_soft_delete

CAPABILITIES = set(json.loads(_native.native_artifact())["capabilities"])
DATABASES = ["sqlite", "postgres"]
OLD = datetime(2000, 1, 1, tzinfo=timezone.utc)


def requires(capability: str) -> pytest.MarkDecorator:
    return pytest.mark.skipif(
        capability not in CAPABILITIES, reason=f"requires a selected {capability} native artifact"
    )


async def connect(dialect: str, source: str) -> tuple[orm.Database, dict]:
    if dialect == "sqlite":
        source = 'datasource db { provider = "sqlite" }\n' + source
    registry = orm.Registry()
    models = loads_soft_delete(source, registry=registry)
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


@requires("updated-at")
@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("mode", ["database", "application"])
async def test_updated_at_is_set_on_orm_updates_and_by_the_trigger(dialect, mode):
    db, models = await connect(
        dialect,
        f"""
model Note {{
  id      Int      @id
  views   Int      @default(0)
  changed DateTime @default(now()) @timestamps.updated_at(mode: "{mode}")
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
  deleted DateTime? @soft_delete.deleted_at(mode: "{mode}")
  books   Book[]
  @@map("tracking_authors")
}}
model Book {{
  id        Int       @id
  author_id Int
  author    Author    @relation(fields: [author_id], references: [id], onDelete: Cascade)
  removed   DateTime? @soft_delete.deleted_at(mode: "{mode}")
  @@map("tracking_books")
}}
"""


@requires("soft-delete")
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
  version Int       @default(0) @locking.version
  deleted DateTime? @soft_delete.deleted_at(mode: "application")
  @@map("tracking_soft_docs")
}
"""


@requires("soft-delete")
@requires("optimistic-locking")
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
  deleted DateTime? @soft_delete.deleted_at(mode: "application")
  @@query.filter("tenant", "shop == scope.shop")
  @@map("tracking_tenant_docs")
}
"""


@requires("soft-delete")
@requires("query-defaults")
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


@requires("soft-delete")
@requires("optimistic-locking")
@requires("query-defaults")
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
  deleted DateTime? @soft_delete.deleted_at(mode: "database")
  books   Book[]
  @@map("tracking_authors")
}
model Book {
  id        Int       @id
  author_id Int
  author    Author    @relation(fields: [author_id], references: [id], onDelete: Cascade)
  version   Int       @default(0) @locking.version
  removed   DateTime? @soft_delete.deleted_at(mode: "database")
  @@map("tracking_books")
}
"""


@requires("soft-delete")
@requires("optimistic-locking")
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


@requires("soft-delete")
@requires("optimistic-locking")
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


@requires("soft-delete")
@requires("optimistic-locking")
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


@requires("optimistic-locking")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_stale_instance_write_raises_version_conflict(dialect):
    db, models = await connect(
        dialect,
        """
model Doc {
  id      Int    @id
  title   String
  version Int    @default(0) @locking.version
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
    changed = f'changed DateTime @default(now()) @timestamps.updated_at(mode: "{mode}")' if mode is not None else ""
    version_field = "version Int @default(0) @locking.version" if version else ""
    return f"""
model Doc {{
  id      Int      @id
  title   String
  {changed}
  {version_field}
  @@map("tracking_upsert_docs")
}}
"""


@requires("optimistic-locking")
@requires("updated-at")
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


@requires("updated-at")
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


@requires("optimistic-locking")
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


@requires("updated-at")
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
  deleted DateTime? @soft_delete.deleted_at(mode: "application")
  @@map("tracking_join_post_tags")
}
"""


@requires("soft-delete")
@requires("query-defaults")
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


@requires("proxy-models")
@requires("optimistic-locking")
@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("operation", ["update", "delete"])
@pytest.mark.parametrize("mode", [None, "application", "database"])
async def test_a_stale_proxy_instance_checks_the_loaded_version(dialect, operation, mode):
    if mode is not None and "soft-delete" not in CAPABILITIES:
        pytest.skip("requires soft-delete")
    source = upsert_doc(version=True)
    if mode is not None:
        source = source.replace(
            "  title   String",
            f'  deleted DateTime? @soft_delete.deleted_at(mode: "{mode}")\n  title   String',
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


@requires("optimistic-locking")
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


@requires("model-composition")
@pytest.mark.parametrize(
    "capability,field,role",
    [
        ("updated-at", "changed DateTime @default(now()) @timestamps.updated_at", "@timestamps.updated_at"),
        ("soft-delete", "deleted DateTime? @soft_delete.deleted_at", "@soft_delete.deleted_at"),
        ("optimistic-locking", "version Int @default(0) @locking.version", "@locking.version"),
    ],
)
@pytest.mark.parametrize("owner", ["Person", "Employee", "Manager"])
def test_composed_ownership_chain_rejects_tracking_roles(capability, field, role, owner):
    if capability not in CAPABILITIES:
        pytest.skip(f"requires {capability}")
    source = f"""
model Person {{
 id Int @id
 {field if owner == "Person" else ""}
}}
model Employee {{
 salary Int
 {field if owner == "Employee" else ""}
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
}}
model Manager {{
 level Int
 {field if owner == "Manager" else ""}
 @@composition.model(parent: "Employee", parentRef: "employee", childRef: "manager")
}}
"""
    with pytest.raises(orm.SchemaError) as error:
        orm.loads(source, registry=orm.Registry())
    message = str(error.value)
    assert owner in message and field.split()[0] in message and role in message
    assert "Employee" in message or "Manager" in message


UPSERT_SOFT_DELETE = """
model Doc {
  id      Int       @id
  title   String
  deleted DateTime? @soft_delete.deleted_at(mode: "application")
  @@map("tracking_upsert_soft_docs")
}
"""


@requires("soft-delete")
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


CLASHING_DEFAULT = """
model Doc {
  id      Int       @id
  deleted DateTime? @soft_delete.deleted_at(mode: "application")
  @@query.filter("deleted", "id > 0")
  @@map("tracking_clash_docs")
}
"""


@requires("soft-delete")
@requires("query-defaults")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_user_default_named_deleted_clashes_with_the_soft_delete_default(dialect):
    with pytest.raises(Exception, match=r'default name is set by both orm\.soft-delete default "deleted" and orm\.query-defaults default "deleted"'):
        await connect(dialect, CLASHING_DEFAULT)


REFERENCE_TO_SOFT_DELETED = """
model Author {
  id      Int       @id
  deleted DateTime? @soft_delete.deleted_at(mode: "application")
  books   Book[]
  @@map("tracking_ref_authors")
}
model Book {
  id        Int    @id
  author_id Int
  author    Author @relation(fields: [author_id], references: [id])
  @@map("tracking_ref_books")
}
"""


@requires("soft-delete")
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
