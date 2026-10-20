"""of() applies no query default on the root of a bound read or write; a relation keeps the default of its target."""

import os

import pytest

import orm
from orm import func
from artifact_caps import artifact_capabilities

SOURCE = """
model BqOwner {
  id Int @id
  name String
  active Boolean @default(true)
  kids BqKid[]
  @@map("bqo_owner")
  @@query.filter("active", "active == true")
}
model BqKid {
  id Int @id
  owner_id Int
  active Boolean @default(true)
  owner BqOwner @relation(fields: [owner_id], references: [id])
  @@map("bqo_kid")
  @@query.filter("active", "active == true")
}
"""

query_defaults = pytest.mark.skipif(
    "query-defaults" not in artifact_capabilities(), reason="requires a native artifact with query-defaults"
)


@pytest.fixture
async def owners():
    registry = orm.Registry()
    models = orm.loads(SOURCE, registry=registry)
    Owner, Kid = models["BqOwner"], models["BqKid"]
    db = await orm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        await Owner.objects.using(db).insert_many(
            [{"id": 1, "name": "shown", "active": True}, {"id": 2, "name": "hidden", "active": False}]
        )
        await Kid.objects.using(db).without_defaults().insert_many(
            [
                {"id": 1, "owner_id": 1, "active": True},
                {"id": 2, "owner_id": 1, "active": False},
                {"id": 3, "owner_id": 2, "active": True},
            ]
        )
        held = await Owner.objects.using(db).without_defaults().order_by(Owner.id)
        yield Owner, Kid, db, held
    finally:
        await db.drop_tables()
        await db.close()


@query_defaults
async def test_a_bound_read_ignores_the_root_default_and_a_filter_still_narrows(owners):
    Owner, _, db, held = owners
    assert await Owner.objects.using(db).count() == 1
    assert await Owner.objects.of(held).using(db) == held
    assert await Owner.objects.of(held).using(db).count() == 2
    assert await Owner.objects.of(held).using(db).filter(Owner.id == 2) == [held[1]]
    assert await held[1].qs().using(db) == [held[1]]
    assert await orm.of(held[1]).using(db) == [held[1]]


@query_defaults
async def test_a_bound_write_ignores_the_root_default(owners):
    Owner, Kid, db, held = owners
    assert await Owner.objects.of(held).using(db).update(name="changed") == held
    assert [o.name for o in held] == ["changed", "changed"]
    assert await Owner.objects.of(held[1]).using(db).filter(Owner.id == 1).update(name="narrow") == []
    await Kid.objects.using(db).without_defaults().filter(Kid.id == 3).delete()
    assert await Owner.objects.of(held[1]).using(db).delete() == [held[1]]
    assert [o.id for o in await Owner.objects.using(db).without_defaults()] == [1]


@query_defaults
async def test_a_bound_aggregate_keeps_the_default_of_its_target(owners):
    Owner, _, db, held = owners
    await Owner.objects.of(held).using(db).load(func.count(Owner.kids).label("n"))
    assert [(o.id, o.n) for o in held] == [(1, 1), (2, 1)]


@query_defaults
async def test_an_instance_label_load_keeps_the_default_of_its_target(owners):
    Owner, _, db, held = owners
    n = func.count(Owner.kids).label("n")
    await held[0].load(n)
    await held[1].load(n)
    assert [held[0].n, held[1].n] == [1, 1]


@query_defaults
async def test_an_instance_label_is_read_again_on_each_load(owners):
    Owner, Kid, db, held = owners
    n = func.count(Owner.kids).label("n")
    await held[0].load(n)
    assert held[0].n == 1
    await Kid.objects.using(db).without_defaults().insert_many([{"id": 4, "owner_id": 1, "active": True}])
    await held[0].load(n)
    assert held[0].n == 2
