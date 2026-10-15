"""Declarative base-model partitions survive registry preparation and route ORM writes."""
import pytest

import orm
from conftest import DATABASE_URL

SOURCE = """
model PartitionRow {
 id Int @default(autoincrement())
 kind String
 amount Int?
 @@id([id, kind])
 @@map("py_partition_row")
 @@partition(list: [kind])
}
partition py_partition_a {
of = PartitionRow
values = ["a"]
@@check("amount IS NOT NULL AND amount > 0")
}
partition py_partition_other {
of = PartitionRow
default = true
}
"""


def test_partitions_survive_registry_extension():
    registry = orm.Registry()
    orm.loads("model PartitionBefore {\n id Int @id\n}", registry=registry)
    orm.loads(SOURCE, registry=registry)
    assert len(registry.ir()["partitions"]) == 2
    orm.loads("model PartitionExtra {\n id Int @id\n}", registry=registry)
    assert len(registry.ir()["partitions"]) == 2
    registry.prepare()


async def test_partition_routes_orm_inserts_and_checks_moves():
    registry = orm.Registry()
    Row = orm.loads(SOURCE, registry=registry)["PartitionRow"]
    db = await orm.connect(DATABASE_URL, registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        rows = await Row.objects.using(db).insert_many([
            {"kind": "a", "amount": 3}, {"kind": "b", "amount": None}, {"kind": "b", "amount": 5},
        ]).returning()
        with pytest.raises(orm.IntegrityError):
            await Row.objects.using(db).insert(kind="a", amount=None)
        with pytest.raises(orm.IntegrityError):
            await rows[1].update(kind="a")
        await rows[2].update(kind="a")
        assert await rows[2].refresh()
        assert rows[2].kind == "a"
        assert await Row.objects.using(db).count() == 3
    finally:
        await db.drop_tables()
        await db.close()
