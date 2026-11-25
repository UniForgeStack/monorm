"""`@@history.track`: event tables, triggers, timeline, restore and purge; needs the `monorm.history` package."""

import os
from datetime import datetime, time, timedelta, timezone
from decimal import Decimal

import pytest

import monorm
from artifact_caps import has_extension
from monorm import SchemaError

pytestmark = pytest.mark.skipif(not has_extension("monorm.history", "monorm.timestamps"), reason="requires the history and timestamps packages")

SOURCES = {
    "postgresql": 'datasource db {\n provider = "postgresql"\n}\n',
    "sqlite": 'datasource db {\n provider = "sqlite"\n}\n',
}

SCHEMA = """
model Post {
 id Int @id @default(autoincrement())
 title String
 body String?
 views Int @default(0)
 @@timestamps.updated_at("changed")
 @@history.track
 @@map("hx_posts")
}
model Note {
 id Int @id @default(autoincrement())
 title String
 body String?
 @@history.track(events: [update], fields: [title], label: "title_changed")
 @@map("hx_notes")
}
model Both {
 id Int @id @default(autoincrement())
 name String
 @@history.track
 @@history.track(model: "BothAudit", events: [delete])
 @@map("hx_both")
}
model Typed {
 id Int @id @default(autoincrement())
 at DateTime
 extra Json?
 @@history.track
 @@map("hx_typed")
}
model Line {
 order_id Int
 sku String
 qty Int
 @@id([order_id, sku])
 @@history.track
 @@map("hx_lines")
}
"""


def url(provider):
    if provider == "sqlite":
        return "sqlite://:memory:"
    u = os.environ.get("ORM_TEST_DATABASE_URL")
    if not u:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    return str(u)


@pytest.fixture(params=["sqlite", "postgresql"])
async def world(request):
    bypass = ' bypass_roles = ["postgres"]\n' if request.param == "postgresql" and "a_skip_outside" not in request.node.name else ""
    datasource = SOURCES[request.param].replace("}", bypass + "}")
    registry = monorm.Registry()
    models = monorm.loads(datasource + SCHEMA, registry=registry)
    db = await monorm.connect(url(request.param), registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        yield db, models
    finally:
        await db.drop_tables()
        await db.close()


async def labels(db, Post, row):
    return [e.event.label for e in await Post.history.using(db).timeline(row)]


async def test_the_event_model_is_named_after_the_table_and_holds_the_row_key(world):
    db, models = world
    event = models["PostEvent"]
    assert event._meta.table == "hx_posts_events"
    assert {"row_id", "label", "data", "created_at", "xact_id"} <= set(event._meta.input_fields)


async def test_an_insert_an_update_and_a_delete_write_one_event_each(world):
    db, models = world
    Post = models["Post"]
    post = await Post.objects.using(db).insert(title="a")
    await post.update(title="b")
    await post.delete()
    entries = await Post.history.using(db).timeline(post)
    assert [e.event.label for e in entries] == ["insert", "update", "delete"]
    assert [e.event.data["title"] for e in entries] == ["a", "b", "b"]
    assert all(e.diff is None for e in entries)


async def test_the_timeline_with_diff_pairs_each_event_with_the_previous_one(world):
    db, models = world
    Post = models["Post"]
    post = await Post.objects.using(db).insert(title="a", views=1)
    await post.update(title="b", views=2)
    entries = await Post.history.using(db).timeline(post, diff=True)
    assert entries[0].diff is None
    assert entries[1].diff["title"] == {"old": "a", "new": "b"}
    assert entries[1].diff["views"] == {"old": 1, "new": 2}


async def test_an_update_that_changes_only_updated_at_writes_no_event(world):
    db, models = world
    Post = models["Post"]
    post = await Post.objects.using(db).insert(title="a")
    await Post.objects.using(db).filter(Post.id == post.id).update(changed=datetime(2030, 1, 1, tzinfo=timezone.utc))
    assert await labels(db, Post, post) == ["insert"]


async def test_events_filter_by_label_fields_and_when(world):
    db, models = world
    Note = models["Note"]
    note = await Note.objects.using(db).insert(title="a", body="x")
    await note.update(body="y")
    assert await Note.history.using(db).timeline(note) == []
    await note.update(title="b")
    entries = await Note.history.using(db).timeline(note)
    assert [e.event.label for e in entries] == ["title_changed"]
    assert entries[0].event.data == {"title": "b"}


async def test_a_composite_key_is_one_column_per_key_field(world):
    db, models = world
    Line = models["Line"]
    line = await Line.objects.using(db).insert(order_id=1, sku="a", qty=1)
    await Line.objects.using(db).filter(Line.order_id == 1, Line.sku == "a").update(qty=2)
    entries = await Line.history.using(db).timeline((1, "a"), diff=True)
    assert [e.event.label for e in entries] == ["insert", "update"]
    assert entries[1].diff == {"qty": {"old": 1, "new": 2}}
    assert [e.event.id for e in await Line.history.using(db).timeline(line)] == [e.event.id for e in entries]
    assert await Line.history.using(db).timeline((1, "b")) == []


async def test_a_manual_event_and_a_backfill_write_events_with_the_label(world):
    db, models = world
    Post = models["Post"]
    history = Post.history.using(db)
    a, b = await Post.objects.using(db).insert_many([{"title": "a"}, {"title": "b"}]).returning()
    manual = await history.event(a, "reviewed")
    assert (manual.label, manual.data["title"]) == ("reviewed", "a")
    assert await history.backfill("snapshot") == 2
    assert await labels(db, Post, a) == ["insert", "reviewed", "snapshot"]
    assert await labels(db, Post, b) == ["insert", "snapshot"]
    with pytest.raises(LookupError):
        await history.event(999, "reviewed")


async def test_at_gives_the_last_event_at_or_before_a_time(world):
    db, models = world
    Post = models["Post"]
    history = Post.history.using(db)
    post = await Post.objects.using(db).insert(title="a")
    await post.update(title="b")
    first, second = (e.event for e in await history.timeline(post))
    assert (await history.at(post, datetime.now(timezone.utc) + timedelta(minutes=1))).id == second.id
    assert await history.at(post, first.created_at - timedelta(days=1)) is None


async def test_restore_updates_a_row_and_leaves_one_restore_event(world):
    db, models = world
    Post = models["Post"]
    history = Post.history.using(db)
    post = await Post.objects.using(db).insert(title="a", body="x")
    await post.update(title="b", body="y")
    first = (await history.timeline(post))[0].event
    restored = await history.restore(first)
    assert (restored.title, restored.body) == ("a", "x")
    entries = await history.timeline(post)
    assert [e.event.label for e in entries] == ["insert", "update", "restore"]
    assert entries[-1].event.data["title"] == "a"


async def test_a_write_after_a_restore_in_the_same_transaction_gets_its_own_label(world):
    db, models = world
    Post = models["Post"]
    post = await Post.objects.using(db).insert(title="a")
    await post.update(title="b")
    first = (await Post.history.using(db).timeline(post))[0].event
    async with db.transaction():
        await Post.history.using(db).restore(first)
        await Post.objects.using(db).filter(Post.id == post.id).update(title="c")
    assert await labels(db, Post, post) == ["insert", "update", "restore", "update"]


async def test_restore_writes_to_the_database_that_loaded_the_event(world):
    db, models = world
    Post = models["Post"]
    post = await Post.objects.using(db).insert(title="a")
    await post.update(title="b")
    first = (await Post.history.using(db).timeline(post))[0].event
    restored = await Post.history.restore(first)  # no default database: the event knows its own
    assert restored.title == "a"


async def test_restore_inserts_a_deleted_row_again(world):
    db, models = world
    Post = models["Post"]
    history = Post.history.using(db)
    post = await Post.objects.using(db).insert(title="a", views=3)
    await post.delete()
    deleted = (await history.timeline(post))[-1].event
    restored = await history.restore(deleted)
    assert (restored.id, restored.title, restored.views) == (post.id, "a", 3)
    assert await labels(db, Post, post) == ["insert", "delete", "restore"]


async def test_restore_gives_dates_and_json_back_from_the_event_data(world):
    db, models = world
    Typed = models["Typed"]
    history = Typed.history.using(db)
    at = datetime(2030, 1, 2, 3, 4, 5, tzinfo=timezone.utc)
    row = await Typed.objects.using(db).insert(at=at, extra={"k": [1, "x"]})
    await row.delete()
    restored = await history.restore((await history.timeline(row))[-1].event)
    assert (restored.at, restored.extra) == (at, {"k": [1, "x"]})
    await Typed.objects.using(db).filter(Typed.id == row.id).update(at=at + timedelta(days=1))
    again = await history.restore((await history.timeline(row))[0].event)
    assert again.at == at


async def test_restore_of_a_deleted_row_needs_a_tracker_that_covers_every_field(world):
    db, models = world
    Note = models["Note"]
    note = await Note.objects.using(db).insert(title="a")
    await note.update(title="b")
    event = (await Note.history.using(db).timeline(note))[0].event
    await note.delete()
    with pytest.raises(ValueError, match="only some fields"):
        await Note.history.using(db).restore(event)
    assert await Note.objects.using(db).filter(Note.id == note.id).count() == 0


async def test_restore_raises_for_a_required_field_that_the_event_lacks(world):
    db, models = world
    Post = models["Post"]
    history = Post.history.using(db)
    post = await Post.objects.using(db).insert(title="a")
    await post.delete()
    event = (await history.timeline(post))[-1].event
    event_class = type(event)
    with monorm.allow_writes(event_class):
        await event_class.objects.using(db).filter(event_class.id == event.id).update(data={"id": post.id})
    event = await event_class.objects.using(db).get(event_class.id == event.id)
    with pytest.raises(ValueError, match="required field"):
        await history.restore(event)


async def test_restore_ignores_event_fields_that_the_model_no_longer_has(world):
    db, models = world
    Post = models["Post"]
    history = Post.history.using(db)
    post = await Post.objects.using(db).insert(title="a")
    event = (await history.timeline(post))[0].event
    event_class = type(event)
    with monorm.allow_writes(event_class):
        await event_class.objects.using(db).filter(event_class.id == event.id).update(data={**event.data, "dropped": 1})
    event = await event_class.objects.using(db).get(event_class.id == event.id)
    await post.update(title="b")
    assert (await history.restore(event)).title == "a"


async def test_purge_deletes_the_events_before_a_time_and_only_those(world):
    db, models = world
    Post = models["Post"]
    history = Post.history.using(db)
    post = await Post.objects.using(db).insert(title="a")
    assert await history.purge(before=datetime.now(timezone.utc) - timedelta(days=1)) == 0
    assert await history.purge(before=datetime.now(timezone.utc) + timedelta(days=1)) == 1
    assert await history.timeline(post) == []


async def test_the_event_model_has_no_update_or_delete_in_the_app(world):
    db, models = world
    Post = models["Post"]
    post = await Post.objects.using(db).insert(title="a")
    event = (await Post.history.using(db).timeline(post))[0].event
    with pytest.raises(monorm.WriteProtected):
        await type(event).objects.using(db).filter(type(event).id == event.id).delete()
    if not db.url.startswith("sqlite://"):
        with pytest.raises(monorm.WriteProtected, match="append_only"):
            await db.execute("UPDATE hx_posts_events SET label = 'forged'")


async def test_events_of_one_transaction_share_a_transaction_id_on_postgresql(world):
    db, models = world
    Post = models["Post"]
    async with db.transaction():
        a = await Post.objects.using(db).insert(title="a")
        b = await Post.objects.using(db).insert(title="b")
    events = [(await Post.history.using(db).timeline(p))[0].event for p in (a, b)]
    if db.url.startswith("sqlite://"):
        assert [e.xact_id for e in events] == [None, None]
    else:
        assert events[0].xact_id is not None
        assert events[0].xact_id == events[1].xact_id


async def test_a_skip_outside_the_bypass_roles_still_writes_events(world):
    db, models = world
    Post = models["Post"]
    post = await Post.objects.using(db).insert(title="a")
    if db.url.startswith("sqlite://"):
        with pytest.raises(monorm.QueryError, match="sqlite has none"):
            with monorm.skip_triggers("history"):
                await post.update(title="b")
        return
    with monorm.skip_triggers("history"):
        await post.update(title="b")
    assert await labels(db, Post, post) == ["insert", "update"]


async def test_a_skip_for_a_bypass_role_writes_no_event():
    if not os.environ.get("ORM_TEST_DATABASE_URL"):
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    source = SOURCES["postgresql"].replace('provider = "postgresql"', 'provider = "postgresql"\n bypass_roles = ["postgres"]') + SCHEMA
    registry = monorm.Registry()
    models = monorm.loads(source, registry=registry)
    db = await monorm.connect(url("postgresql"), registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        Post = models["Post"]
        post = await Post.objects.using(db).insert(title="a")
        with monorm.skip_triggers("history"):
            await post.update(title="b")
        await post.update(title="c")
        assert await labels(db, Post, post) == ["insert", "update"]
    finally:
        await db.drop_tables()
        await db.close()


async def test_a_decimal_keeps_every_digit_in_the_event_and_in_a_restore_on_postgresql():
    if not os.environ.get("ORM_TEST_DATABASE_URL"):
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    source = SOURCES["postgresql"] + 'model Price {\n id Int @id @default(autoincrement())\n amount Decimal\n @@history.track\n @@map("hx_prices")\n}\n'
    registry = monorm.Registry()
    models = monorm.loads(source, registry=registry)
    db = await monorm.connect(url("postgresql"), registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        Price = models["Price"]
        exact = Decimal("12345678901234567890.123456789")
        price = await Price.objects.using(db).insert(amount=exact)
        await price.update(amount=Decimal("1"))
        first = (await Price.history.using(db).timeline(price))[0].event
        assert first.data["amount"] == str(exact)
        restored = await Price.history.using(db).restore(first)
        assert restored.amount == exact
    finally:
        await db.drop_tables()
        await db.close()


async def test_restore_gives_uuid_time_and_big_integers_back_on_postgresql():
    if not os.environ.get("ORM_TEST_DATABASE_URL"):
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    source = SOURCES["postgresql"] + (
        'model Wide {\n id Int @id @default(autoincrement())\n ref String @db.Uuid\n at DateTime @db.Time\n big BigInt\n'
        ' @@history.track\n @@map("hx_wide")\n}\n'
    )
    registry = monorm.Registry()
    models = monorm.loads(source, registry=registry)
    db = await monorm.connect(url("postgresql"), registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        Wide = models["Wide"]
        row = await Wide.objects.using(db).insert(
            ref="0190a5c2-7b3e-7d2a-9c1f-1234567890ab", at=time(13, 14, 15), big=9007199254740993
        )
        await row.delete()
        restored = await Wide.history.using(db).restore((await Wide.history.using(db).timeline(row))[-1].event)
        assert (restored.ref, restored.at, restored.big) == (row.ref, row.at, 9007199254740993)
    finally:
        await db.drop_tables()
        await db.close()


async def test_a_model_with_two_trackers_needs_the_tracker_name(world):
    db, models = world
    Both = models["Both"]
    history = Both.history.using(db)
    both = await Both.objects.using(db).insert(name="a")
    await both.delete()
    with pytest.raises(ValueError, match="several trackers"):
        await history.timeline(both)
    assert [e.event.label for e in await history.timeline(both, tracker="BothEvent")] == ["insert", "delete"]
    assert [e.event.label for e in await history.timeline(both, tracker="BothAudit")] == ["delete"]
    with pytest.raises(ValueError, match="no tracker with the event model"):
        await history.timeline(both, tracker="Nope")
    assert await history.purge(before=datetime.now(timezone.utc) + timedelta(days=1)) == 3


SCOPED = """
model Doc {
 id Int @id @default(autoincrement())
 title String
 @@history.track(scope: [user])
 @@map("hx_docs")
}
"""


@pytest.fixture(params=["sqlite", "postgresql"])
async def scoped(request):
    registry = monorm.Registry()
    models = monorm.loads(SOURCES[request.param] + SCOPED, registry=registry)
    db = await monorm.connect(url(request.param), registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        yield db, models
    finally:
        await db.drop_tables()
        await db.close()


async def test_the_scope_values_are_stored_on_postgresql_and_null_without_a_value(scoped):
    db, models = scoped
    Doc = models["Doc"]
    with monorm.scope(user="ann"):
        a = await Doc.objects.using(db).insert(title="a")
    b = await Doc.objects.using(db).insert(title="b")
    history = Doc.history.using(db)
    first = (await history.timeline(a))[0].event
    second = (await history.timeline(b))[0].event
    assert second.user is None
    assert first.user == (None if db.url.startswith("sqlite://") else "ann")


def test_a_composed_model_cannot_be_tracked():
    source = SOURCES["sqlite"] + """
model Base {
 id Int @id @default(autoincrement())
 name String
 @@map("hx_base")
}
model Child {
 extra String
 @@composition.model(parent: "Base", parentRef: "base", childRef: "child")
 @@history.track
 @@map("hx_child")
}
"""
    with pytest.raises(SchemaError, match="composed model"):
        monorm.loads(source, registry=monorm.Registry())


def test_when_without_the_update_event_is_an_error():
    source = SOURCES["sqlite"] + """
model Thing {
 id Int @id @default(autoincrement())
 name String
 @@history.track(events: [insert], when: "new.name <> old.name")
 @@map("hx_things")
}
"""
    with pytest.raises(SchemaError, match="when applies to update events"):
        monorm.loads(source, registry=monorm.Registry())


def test_two_trackers_with_the_same_trigger_suffix_are_an_error():
    source = SOURCES["sqlite"] + """
model Thing {
 id Int @id @default(autoincrement())
 name String
 @@history.track
 @@history.track(model: "Events")
 @@map("hx_things")
}
"""
    with pytest.raises(SchemaError, match="already uses the trigger names"):
        monorm.loads(source, registry=monorm.Registry())


def test_two_trackers_with_the_same_event_model_name_are_an_error():
    source = SOURCES["sqlite"] + """
model Thing {
 id Int @id @default(autoincrement())
 name String
 @@history.track
 @@history.track
 @@map("hx_things")
}
"""
    with pytest.raises(SchemaError, match="@@history.track"):
        monorm.loads(source, registry=monorm.Registry())


MIGRATION_V1 = 'datasource db {\n provider = "postgresql"\n}\nmodel Item {\n id Int @id @default(autoincrement())\n name String\n @@history.track\n @@map("hm_items")\n}\n'
MIGRATION_V2 = MIGRATION_V1.replace("@@history.track", '@@history.track(events: [update], fields: [name])')
MIGRATION_V3 = MIGRATION_V1.replace("@@history.track", '@@history.track\n @@history.track(model: "ItemAudit", events: [delete])')
MIGRATION_V4 = MIGRATION_V1.replace(" @@history.track\n", "")


def test_migrations_create_change_add_and_drop_the_event_table_and_triggers(tmp_path):
    from _planned import planned
    from monorm.migrations import Migrations

    def registry(source):
        reg = monorm.Registry()
        monorm.loads(source, registry=reg)
        return reg

    def up_down(source):
        return planned(Migrations(tmp_path, registry(source)))

    def make(source):
        m = Migrations(tmp_path, registry(source)).make()
        return m

    up, _ = up_down(MIGRATION_V1)
    assert any(x.startswith('CREATE TABLE "hm_items_events"') for x in up)
    assert 'CREATE TRIGGER "h_events_ins" AFTER INSERT ON "hm_items"' in " ".join(x[:60] for x in up)
    make(MIGRATION_V1)
    up, _ = up_down(MIGRATION_V2)  # a tracker change replaces triggers; the event table stays
    assert any(x.startswith('DROP TRIGGER "h_events_ins"') for x in up)
    assert not any(x.startswith("DROP TABLE") for x in up)
    make(MIGRATION_V2)
    up, _ = up_down(MIGRATION_V3)
    assert any(x.startswith('CREATE TABLE "item_audit"') for x in up)
    make(MIGRATION_V3)
    up, down = up_down(MIGRATION_V4)
    assert 'DROP TABLE "hm_items_events"' in up and 'DROP TABLE "item_audit"' in up
    assert any(x.startswith('CREATE TABLE "hm_items_events"') for x in down)
