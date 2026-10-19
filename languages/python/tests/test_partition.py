"""Declarative base-model partitions survive registry preparation and route ORM writes."""

import pytest

import orm
from conftest import DATABASE_URL
from artifact_caps import artifact_capabilities

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
        rows = (
            await Row.objects.using(db)
            .insert_many(
                [
                    {"kind": "a", "amount": 3},
                    {"kind": "b", "amount": None},
                    {"kind": "b", "amount": 5},
                ]
            )
            .returning()
        )
        with pytest.raises(orm.IntegrityError):
            await Row.objects.using(db).insert(kind="a", amount=None)
        with pytest.raises(orm.IntegrityError):
            await rows[1].update(kind="a")
        await rows[2].update(kind="a")
        await rows[2].load(reload=True)
        assert rows[2].kind == "a"
        assert await Row.objects.using(db).count() == 3
    finally:
        await db.drop_tables()
        await db.close()


async def test_proxy_partition_bound_is_storage_and_prunes_without_defaults():
    import json
    from pathlib import Path
    from orm import _native

    if "proxy-models" not in artifact_capabilities():
        pytest.skip("requires proxy native artifact")
    source = Path(__file__).parents[3].joinpath("fixtures").joinpath("partition-proxy.prisma").read_text()
    registry = orm.Registry()
    models = orm.loads(source, registry=registry)
    Reward, Percent, Fixed = (models[n] for n in ("Reward", "PercentageReward", "FixedReward"))
    db = await orm.connect(DATABASE_URL, registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        row = await Percent.objects.using(db).insert(shop=1, percent=10, content_hash="same")
        assert row.pk == (row.id, models["RewardKind"].PERCENT)
        fixed = await Fixed.objects.using(db).insert(shop=1, amount=20, content_hash="same")
        await Reward.objects.using(db).insert(kind=models["RewardKind"].PRICE, shop=1, price=5)
        assert await Reward.objects.using(db).count() == 3
        assert await Percent.objects.using(db).without_defaults().count() == 1
        assert type((await Reward.objects.using(db).filter(Reward.id == row.id).first())) is Reward
        assert " IN ('percent')" in Percent.objects.using(db).without_defaults().sql()
        plan = str(await Percent.objects.using(db).without_defaults().explain())
        assert "partition_proxy_percent" in plan
        assert "partition_proxy_fixed" not in plan and "partition_proxy_price" not in plan
        with pytest.raises(orm.IntegrityError):
            await Percent.objects.using(db).insert(shop=1, percent=10, content_hash="same")
        with pytest.raises(orm.IntegrityError):
            await Percent.objects.using(db).insert(shop=1, percent=None)
        with pytest.raises(orm.IntegrityError):
            await Percent.objects.using(db).insert(shop=1, percent=10, kind=models["RewardKind"].FIXED)
        assert await Percent.objects.using(db).without_defaults().update(percent=15) == 1
        await fixed.load(reload=True)
        assert fixed.amount == 20
        assert await Percent.objects.using(db).without_defaults().delete() == 1
        assert await Reward.objects.using(db).count() == 2
        assert await Percent.objects.using(db).insert_many([{"shop": 2, "percent": 25}], copy=True) == 1
        assert await Percent.objects.using(db).without_defaults().count() == 1
    finally:
        await db.drop_tables()
        await db.close()


async def test_rewards_proxy_partition_inherits_scope_and_soft_delete():
    import json
    from pathlib import Path
    from orm import _native

    capabilities = artifact_capabilities()
    if not {"proxy-models", "query-defaults", "soft-delete"}.issubset(capabilities):
        pytest.skip("requires combined proxy, query-defaults and soft-delete artifact")
    source = Path(__file__).parents[3].joinpath("fixtures").joinpath("partition-proxy.prisma").read_text()
    source = source.replace("deleted DateTime?", "deleted DateTime? @soft_delete.deleted_at")
    source = source.replace(
        '@@map("partition_proxy_reward")',
        '@@map("partition_proxy_reward")\n @@query.filter("tenant", "shop == scope.shop and deleted == null")',
    )
    registry = orm.Registry()
    models = orm.loads(source, registry=registry)
    Reward, Percent, Fixed = (models[n] for n in ("Reward", "PercentageReward", "FixedReward"))
    db = await orm.connect(DATABASE_URL, registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        await Percent.objects.using(db).insert_many([{"shop": 1, "percent": 10}, {"shop": 2, "percent": 20}])
        await Fixed.objects.using(db).insert(shop=1, amount=5)
        with orm.scope(shop=1):
            assert await Percent.objects.using(db).count() == 1
            assert await Reward.objects.using(db).count() == 2
            assert await Percent.objects.using(db).delete() == 1
            assert await Percent.objects.using(db).count() == 0
            assert await Fixed.objects.using(db).count() == 1
        # Neither scope nor the deleted-row default remains; storage ownership does.
        assert await Percent.objects.using(db).without_defaults().count() == 2
        deleted = Percent.objects.using(db).without_defaults().filter(Percent.deleted.is_not_null())
        assert await deleted.count() == 1
    finally:
        await db.drop_tables()
        await db.close()


async def test_multi_value_proxy_partition_requires_a_key_and_owns_only_its_values():
    import json
    from orm import _native

    if "proxy-models" not in artifact_capabilities():
        pytest.skip("requires proxy native artifact")
    source = """
model Item {
 id Int
 kind String
 @@id([id, kind])
 @@map("multi_partition_item")
 @@partition(list: [kind])
}
model Selected {
 @@proxy.of(Item)
 @@partition.values(["a", "b"])
 @@map("multi_partition_selected")
}
partition multi_partition_other { of = Item default = true }
"""
    registry = orm.Registry()
    models = orm.loads(source, registry=registry)
    Item, Selected = models["Item"], models["Selected"]
    db = await orm.connect(DATABASE_URL, registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        await Selected.objects.using(db).insert_many([{"id": 1, "kind": "a"}, {"id": 1, "kind": "b"}])
        await Item.objects.using(db).insert(id=1, kind="other")
        assert await Selected.objects.using(db).without_defaults().count() == 2
        assert await Item.objects.using(db).count() == 3
        with pytest.raises(ValueError, match="Selected.kind is required"):
            await Selected.objects.using(db).insert(id=2)
        with pytest.raises(orm.IntegrityError):
            await Selected.objects.using(db).insert(id=2, kind="other")
        with pytest.raises(orm.IntegrityError):
            await Selected.objects.using(db).insert_many([{"id": 2, "kind": "other"}], copy=True)
        assert (
            await Selected.objects.using(db).insert_many([{"id": 2, "kind": "a"}, {"id": 2, "kind": "b"}], copy=True)
            == 2
        )
        assert await Selected.objects.using(db).without_defaults().count() == 4
    finally:
        await db.drop_tables()
        await db.close()


async def test_spec_rewards_example_with_decimals_local_uniqueness_and_soft_delete():
    import json
    from decimal import Decimal
    from pathlib import Path
    from orm import _native

    if not {"proxy-models", "query-defaults", "soft-delete"}.issubset(
        artifact_capabilities()
    ):
        pytest.skip("requires combined proxy, query-defaults and soft-delete artifact")
    source = Path(__file__).parents[3].joinpath("fixtures").joinpath("partition-rewards.prisma").read_text()
    registry = orm.Registry()
    models = orm.loads(source, registry=registry)
    Tier, Reward, Percent, Fixed, Price = (
        models[n]
        for n in (
            "Tier",
            "Reward",
            "PercentageDiscountReward",
            "FixedDiscountReward",
            "SetPriceReward",
        )
    )
    db = await orm.connect(DATABASE_URL, registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        await Tier.objects.using(db).insert(id=1)
        with orm.scope(shop=1):
            percent = await Percent.objects.using(db).insert(
                shop_id=1, tier_id=1, content_hash="same", percent=Decimal("10.25")
            )
            await Fixed.objects.using(db).insert(shop_id=1, tier_id=1, content_hash="same", amount=Decimal("2.50"))
            await Price.objects.using(db).insert(shop_id=1, tier_id=1, content_hash="same", price=Decimal("3.75"))
            assert percent.percent == Decimal("10.25")
            assert await Reward.objects.using(db).count() == 3
            assert await Percent.objects.using(db).count() == 1
            with pytest.raises(orm.IntegrityError):
                await Percent.objects.using(db).insert(shop_id=1, tier_id=1, content_hash="same", percent=Decimal("12"))
            with pytest.raises(orm.IntegrityError):
                await Reward.objects.using(db).insert(
                    shop_id=1, tier_id=1, type=models["RewardType"].PERCENTAGE_DISCOUNT, percent=None
                )
            assert await Percent.objects.using(db).delete() == 1
            assert await Percent.objects.using(db).count() == 0
            assert await Percent.objects.using(db).without_defaults().count() == 1
            await Percent.objects.using(db).insert(shop_id=1, tier_id=1, content_hash="same", percent=Decimal("20"))
            assert await Reward.objects.using(db).count() == 3
        with orm.scope(shop=2):
            assert await Reward.objects.using(db).count() == 0
            assert await Percent.objects.using(db).count() == 0
        assert await Reward.objects.using(db).without_defaults().count() == 4
    finally:
        await db.drop_tables()
        await db.close()
