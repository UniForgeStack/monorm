"""`@@history.versions`: numbered versions of an aggregate, dedupe, timeline, version_at and restore; needs the `monorm.history` package."""

import asyncio
import os
from datetime import datetime, timedelta, timezone

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
model BundleVersion {
 id Int @id @default(autoincrement())
 bundle_id Int
 @@history.versions(of: bundle_id, scope: [user])
 @@map("hv_bundle_versions")
}
model PlainVersion {
 id Int @id @default(autoincrement())
 doc_id Int
 @@history.versions(of: doc_id)
 @@map("hv_plain_versions")
}
"""


def url(provider):
    if provider == "sqlite":
        return "sqlite://:memory:"
    u = os.environ.get("ORM_TEST_DATABASE_URL")
    if not u:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    return str(u)


class Entity:
    def __init__(self, id):
        self.id = id


class Store:
    """A stand-in for the aggregate: the state per bundle id, and the restore calls."""

    def __init__(self):
        self.state = {}
        self.restored = []

    def generate(self, entity):
        return dict(self.state[getattr(entity, "id", entity)])

    def restore(self, snapshot):
        self.restored.append(snapshot)
        self.state[snapshot["id"]] = dict(snapshot)


@pytest.fixture(params=["sqlite", "postgresql"])
async def world(request):
    registry = monorm.Registry()
    models = monorm.loads(SOURCES[request.param] + SCHEMA, registry=registry)
    db = await monorm.connect(url(request.param), registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    store = Store()
    models["BundleVersion"].history.register(generate=store.generate, restore=store.restore)
    try:
        yield db, models, store
    finally:
        await db.drop_tables()
        await db.close()


def is_sqlite(db):
    return db.url.startswith("sqlite://")


async def test_the_version_model_has_the_version_columns(world):
    db, models, _ = world
    fields = set(models["BundleVersion"]._meta.input_fields) | {"id"}
    assert {"version_number", "snapshot", "content_hash", "effective_at", "change_type", "restored_from", "xact_id", "user"} <= fields
    assert "user" not in set(models["PlainVersion"]._meta.input_fields)


async def test_record_numbers_the_versions_of_each_entity_from_one(world):
    db, models, store = world
    BundleVersion = models["BundleVersion"]
    history = BundleVersion.history.using(db)
    store.state = {1: {"id": 1, "title": "a"}, 2: {"id": 2, "title": "x"}}
    first = await history.record(1, "create")
    store.state[1]["title"] = "b"
    second = await history.record(1, "update")
    other = await history.record(2, "create")
    assert [first.version_number, second.version_number, other.version_number] == [1, 2, 1]
    assert (first.change_type, second.change_type) == ("create", "update")
    assert second.snapshot == {"id": 1, "title": "b"}
    assert len(second.content_hash) == 64


async def test_a_snapshot_with_the_hash_of_the_latest_version_writes_nothing(world):
    db, models, store = world
    BundleVersion = models["BundleVersion"]
    history = BundleVersion.history.using(db)
    store.state = {1: {"id": 1, "title": "a"}}
    first = await history.record(1, "create")
    same = await history.record(1, "update")
    assert same.id == first.id
    assert await BundleVersion.objects.using(db).count() == 1
    store.state[1]["title"] = "b"
    assert (await history.record(1, "update")).version_number == 2
    store.state[1]["title"] = "a"
    assert (await history.record(1, "update")).version_number == 3


async def test_a_delete_version_is_written_even_when_the_snapshot_is_the_same(world):
    db, models, store = world
    history = models["BundleVersion"].history.using(db)
    store.state = {1: {"id": 1, "title": "a"}}
    await history.record(1, "create")
    gone = await history.record(1, "delete")
    assert (gone.version_number, gone.change_type) == (2, "delete")


async def test_a_version_after_a_delete_is_written_even_when_the_snapshot_is_the_same(world):
    db, models, store = world
    history = models["BundleVersion"].history.using(db)
    store.state = {1: {"id": 1, "title": "a"}}
    await history.record(1, "create")
    await history.record(1, "delete")
    again = await history.record(1, "create")
    assert (again.version_number, again.change_type) == (3, "create")
    assert (await history.record(1, "update")).version_number == 3


async def test_the_content_hash_is_the_sha256_of_the_canonical_snapshot(world):
    db, models, store = world
    store.state = {1: {"title": "a", "id": 1}}
    version = await models["BundleVersion"].history.using(db).record(1, "create")
    assert version.content_hash == "b7962b1130324bd10b5b262d3975757cb24a2a216bc9b5dd87a2ab5a0fe432fd"


async def test_record_takes_an_object_with_an_id(world):
    db, models, store = world
    history = models["BundleVersion"].history.using(db)
    store.state = {7: {"id": 7}}
    version = await history.record(Entity(7), "create")
    assert version.bundle_id == 7


async def test_record_before_register_raises():
    registry = monorm.Registry()
    models = monorm.loads(SOURCES["sqlite"] + SCHEMA, registry=registry)
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    try:
        with pytest.raises(RuntimeError, match="not registered"):
            await models["PlainVersion"].history.using(db).record(1, "create")
    finally:
        await db.close()


async def test_record_rejects_a_change_type_that_restore_owns(world):
    db, models, store = world
    store.state = {1: {"id": 1}}
    with pytest.raises(ValueError, match="change_type"):
        await models["BundleVersion"].history.using(db).record(1, "restore")


async def test_timeline_gives_the_versions_by_number_and_the_diff_with_the_previous_one(world):
    db, models, store = world
    history = models["BundleVersion"].history.using(db)
    store.state = {1: {"id": 1, "title": "a", "qty": 1}}
    await history.record(1, "create")
    store.state[1].update(title="b", qty=2)
    await history.record(1, "update")
    entries = await history.timeline(1, diff=True)
    assert [e.event.version_number for e in entries] == [1, 2]
    assert entries[0].diff is None
    assert entries[1].diff == {"title": {"old": "a", "new": "b"}, "qty": {"old": 1, "new": 2}}
    assert (await history.timeline(1))[1].diff is None


async def test_version_at_gives_the_last_version_at_or_before_a_time(world):
    db, models, store = world
    history = models["BundleVersion"].history.using(db)
    store.state = {1: {"id": 1, "title": "a"}}
    first = await history.record(1, "create")
    store.state[1]["title"] = "b"
    second = await history.record(1, "update")
    # SQLite stamps whole seconds, so the test moves the second version a day on.
    await models["BundleVersion"].objects.using(db).filter(models["BundleVersion"].id == second.id).update(effective_at=first.effective_at + timedelta(days=1))
    assert (await history.version_at(1, first.effective_at + timedelta(hours=1))).id == first.id
    assert (await history.version_at(1, first.effective_at + timedelta(days=2))).id == second.id
    assert await history.version_at(1, first.effective_at - timedelta(days=1)) is None
    assert (await history.at(1, first.effective_at + timedelta(hours=1))).id == first.id


async def test_restore_calls_the_function_and_leaves_a_restore_version(world):
    db, models, store = world
    history = models["BundleVersion"].history.using(db)
    store.state = {1: {"id": 1, "title": "a"}}
    first = await history.record(1, "create")
    store.state[1]["title"] = "b"
    await history.record(1, "update")
    restored = await history.restore(first)
    assert store.restored == [{"id": 1, "title": "a"}]
    assert store.state[1]["title"] == "a"
    assert (restored.version_number, restored.change_type, restored.restored_from) == (3, "restore", first.id)
    assert restored.snapshot == {"id": 1, "title": "a"}


async def test_a_restore_function_that_raises_writes_no_version(world):
    db, models, store = world
    history = models["BundleVersion"].history.using(db)
    store.state = {1: {"id": 1, "title": "a"}}
    first = await history.record(1, "create")

    def broken(snapshot):
        raise RuntimeError("no")

    models["BundleVersion"].history.register(generate=store.generate, restore=broken)
    with pytest.raises(RuntimeError, match="no"):
        await history.restore(first)
    assert await models["BundleVersion"].objects.using(db).count() == 1


async def test_a_version_rolls_back_with_the_outer_transaction(world):
    db, models, store = world
    history = models["BundleVersion"].history.using(db)
    store.state = {1: {"id": 1, "title": "a"}}
    with pytest.raises(RuntimeError, match="stop"):
        async with db.transaction():
            await history.record(1, "create")
            raise RuntimeError("stop")
    assert await models["BundleVersion"].objects.using(db).count() == 0
    assert (await history.record(1, "create")).version_number == 1


async def test_the_unique_index_refuses_a_repeated_number(world):
    db, models, store = world
    BundleVersion = models["BundleVersion"]
    store.state = {1: {"id": 1}}
    first = await BundleVersion.history.using(db).record(1, "create")
    with pytest.raises(Exception):
        await BundleVersion.objects.using(db).insert(
            bundle_id=1, version_number=1, snapshot={}, content_hash="x", effective_at=first.effective_at, change_type="update"
        )


async def test_the_scope_values_are_stored_on_postgresql_and_null_without_a_value(world):
    db, models, store = world
    history = models["BundleVersion"].history.using(db)
    store.state = {1: {"id": 1, "title": "a"}}
    with monorm.scope(user="ann"):
        first = await history.record(1, "create")
    store.state[1]["title"] = "b"
    second = await history.record(1, "update")
    assert second.user is None
    assert first.user == (None if is_sqlite(db) else "ann")
    assert (first.xact_id is None) == is_sqlite(db)


async def test_concurrent_writers_of_one_entity_get_distinct_numbers_on_postgresql(world):
    db, models, store = world
    if is_sqlite(db):
        pytest.skip("SQLite has one connection")
    BundleVersion = models["BundleVersion"]
    history = BundleVersion.history.using(db)
    calls = iter(range(100))
    BundleVersion.history.register(generate=lambda entity: {"n": next(calls)}, restore=store.restore)

    async def write():
        async with db.transaction():
            version = await history.record(1, "update")
            await asyncio.sleep(0.1)
            return version

    results = await asyncio.gather(write(), write(), write())
    assert sorted(r.version_number for r in results) == [1, 2, 3]
    assert await BundleVersion.objects.using(db).count() == 3
    # generate runs under the lock, so a later number never holds an older snapshot.
    assert [r.snapshot["n"] for r in sorted(results, key=lambda r: r.version_number)] == [0, 1, 2]


def test_a_version_model_cannot_also_be_a_tracker():
    source = SOURCES["sqlite"] + """
model Both {
 id Int @id @default(autoincrement())
 bundle_id Int
 @@history.versions(of: bundle_id)
 @@history.track
 @@map("hv_both")
}
"""
    with pytest.raises(SchemaError, match="history"):
        monorm.loads(source, registry=monorm.Registry())


def test_of_must_name_a_field_of_the_model():
    source = SOURCES["sqlite"] + """
model Bad {
 id Int @id @default(autoincrement())
 @@history.versions(of: nope)
 @@map("hv_bad")
}
"""
    with pytest.raises(SchemaError, match="nope"):
        monorm.loads(source, registry=monorm.Registry())


def test_of_cannot_be_the_primary_key():
    source = SOURCES["sqlite"] + """
model Bad {
 id Int @id @default(autoincrement())
 @@history.versions(of: id)
 @@map("hv_bad")
}
"""
    with pytest.raises(SchemaError, match="primary key"):
        monorm.loads(source, registry=monorm.Registry())


def test_migrations_create_the_version_table_and_the_unique_index(tmp_path):
    from monorm.migrations import Migrations

    registry = monorm.Registry()
    monorm.loads(SOURCES["postgresql"] + SCHEMA, registry=registry)
    up = [s.sql for s in Migrations(tmp_path, registry).plan().up]
    table = next(x for x in up if x.startswith('CREATE TABLE "hv_bundle_versions"'))
    assert all(f'"{column}"' in table for column in ("version_number", "snapshot", "content_hash", "effective_at", "change_type", "restored_from", "xact_id", "user"))
    assert 'CREATE UNIQUE INDEX "hv_bundle_versions_bundle_id_version_key" ON "hv_bundle_versions" ("bundle_id", "version_number")' in up
    assert 'CONSTRAINT "hv_bundle_versions_restored_from_fkey" FOREIGN KEY ("restored_from") REFERENCES "hv_bundle_versions" ("id") ON DELETE SET NULL' in table


def test_migrations_give_restored_from_a_self_reference_foreign_key_on_sqlite(tmp_path):
    from monorm.migrations import Migrations

    registry = monorm.Registry()
    monorm.loads(SOURCES["sqlite"] + SCHEMA, registry=registry)
    up = "\n".join(s.sql for s in Migrations(tmp_path, registry).plan().up)
    assert 'CONSTRAINT "hv_bundle_versions_restored_from_fkey" FOREIGN KEY ("restored_from") REFERENCES "hv_bundle_versions" ("id") ON DELETE SET NULL' in up
