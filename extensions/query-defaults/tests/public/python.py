"""`@@query.defaults` from the schema language to a query; needs a query-defaults artifact."""

import asyncio
import json
import os
from pathlib import Path

import orm
from orm import _native

FIXTURE = Path(__file__).parent.parent.joinpath("fixtures/schema.prisma").read_text()
assert "query-defaults" in json.loads(_native.native_artifact())["capabilities"], (
    "select orm-query-defaults in the build"
)


async def run(provider: str, url: str) -> None:
    registry = orm.Registry()
    models = orm.loads(FIXTURE.replace('"sqlite"', f'"{provider}"'), registry=registry)
    Policy, Scoped = models["Policy"], models["Scoped"]
    Notice, Draft, Archive = models["Notice"], models["Draft"], models["Archive"]
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        await Policy.objects.using(db).insert_many(
            [{"name": "Alice", "bio": "large"}, {"name": "hidden", "active": False, "bio": "large"}]
        )
        assert await Policy.objects.using(db).count() == 1
        assert await Policy.objects.using(db).without_defaults().count() == 2
        row = await Policy.objects.using(db).get()
        assert row.to_dict() == {"id": row.pk, "name": "Alice"}, row.to_dict()
        try:
            row.bio
        except orm.NotLoaded:
            pass
        else:
            raise AssertionError("a selected-out field was loaded")
        full = await Policy.objects.using(db).without_defaults().get(Policy.name == "hidden")
        assert full.bio == "large" and full.active is False
        scoped = Scoped.objects.using(db)
        tenants = await scoped.insert_many([{"shop_id": 1}, {"shop_id": 2}]).returning()
        first, second = tenants
        # DO NOTHING and plain INSERT do not need scope; DO UPDATE fails closed.
        await scoped.insert(id=first.pk, shop_id=9).on_conflict(Scoped.id, update=False)
        try:
            await scoped.insert(id=first.pk, shop_id=9).on_conflict(Scoped.id, update=True)
        except orm.QueryError as e:
            for text in (
                "Scoped",
                'default "tenant"',
                "orm.query-defaults",
                "scope.shop",
                'without_defaults("tenant")',
            ):
                assert text in str(e), e
        else:
            raise AssertionError("an upsert with no scope ran")
        with orm.scope(shop=2):
            blocked = scoped.insert(id=first.pk, shop_id=9).on_conflict(Scoped.id, update=True)
            blocked.inspect()
            assert await blocked == 0
            assert (
                await scoped.insert_many([{"id": second.pk, "shop_id": 2}]).on_conflict(
                    Scoped.id, update=True, update_values={"shop_id": 3}
                )
                == 1
            )
        assert (await scoped.without_defaults().get(Scoped.id == first.pk)).shop_id == 1
        assert (await scoped.without_defaults().get(Scoped.id == second.pk)).shop_id == 3
        with orm.scope(shop=3):
            await scoped.insert(id=second.pk, shop_id=2).on_conflict(Scoped.id, update=True)
        # Non-scoped defaults also guard the existing row, not EXCLUDED.
        hidden = await Policy.objects.using(db).without_defaults().get(Policy.name == "hidden")
        assert (
            await Policy.objects.using(db)
            .insert(id=hidden.pk, name="changed", bio="new")
            .on_conflict(Policy.id, update=True)
            == 0
        )
        assert (await Policy.objects.using(db).without_defaults().get(Policy.id == hidden.pk)).name == "hidden"
        try:
            await Scoped.objects.using(db).count()
        except orm.QueryError as e:
            for text in (
                "Scoped",
                'default "tenant"',
                "orm.query-defaults",
                "scope.shop",
                'without_defaults("tenant")',
            ):
                assert text in str(e), e
        else:
            raise AssertionError("a read with no scope ran")
        with orm.scope(shop=2):
            assert [r.shop_id for r in await Scoped.objects.using(db)] == [2]
        with orm.scope(shop=None):
            try:
                await Scoped.objects.using(db).count()
            except orm.QueryError as e:
                assert "scope.shop is None" in str(e), e
            else:
                raise AssertionError("a read with a None scope ran")
            assert await Scoped.objects.using(db).without_defaults().count() == 2
        with orm.scope(shop=0):
            assert await Scoped.objects.using(db).count() == 0
        for falsy in ("", False):
            with orm.scope(shop=falsy):
                try:
                    await Scoped.objects.using(db).count()
                except Exception as e:
                    assert "is None" not in str(e), e  # a type error is fine; the NULL rejection is not
        rows = [
            {"owner": 1, "published": True},
            {"owner": 1, "published": False},
            {"owner": 2, "published": True},
            {"owner": 2, "published": False},
        ]
        for model in (Notice, Draft, Archive):
            await model.objects.using(db).insert_many(rows)
        notices = Notice.objects.using(db)
        # Named filters AND; a name bypasses one and keeps the others.
        assert await notices.count() == 1
        assert await notices.without_defaults("published").count() == 2
        assert await notices.without_defaults("mine").count() == 2
        assert await notices.without_defaults("mine", "published").count() == 4
        assert await notices.without_defaults().count() == 4
        assert [
            (d["name"], d["active"]) for d in notices.without_defaults("mine").current_filter() if d["name"] != "query"
        ] == [
            ("mine", False),
            ("published", True),
        ]
        try:
            await notices.without_defaults("nope").count()
        except orm.QueryError as e:
            assert 'unknown default "nope"; available defaults: ["mine", "published", "query"]' in str(e), e
        else:
            raise AssertionError("an unknown default name ran")
        # A child inherits each name; the same name replaces, "none" removes.
        assert await Draft.objects.using(db).count() == 1
        assert [(r.owner, r.published) for r in await Draft.objects.using(db)] == [(1, False)]
        assert await Draft.objects.using(db).without_defaults("mine").count() == 2
        assert await Archive.objects.using(db).count() == 2
        assert [d["name"] for d in Archive.objects.current_filter() if d["name"] != "query"] == ["mine"]
        for source, text in (
            ('@@query.filter("owner == 1")', 'takes a name and an expression: @@query.filter("<name>", "<expr>")'),
            ('@@query.filter("query", "owner == 1")', "reserved"),
            ('@@query.filter("a", "owner == 1")\n @@query.filter("a", "owner == 2")', "is given twice"),
        ):
            try:
                orm.loads(f"model Bad {{\n id Int @id\n owner Int\n {source}\n}}", registry=orm.Registry())
            except Exception as e:
                assert text in str(e), e
            else:
                raise AssertionError(f"accepted {source}")
    finally:
        await db.drop_tables()
        await db.close()


async def main() -> None:
    await run("sqlite", "sqlite://:memory:")
    if os.environ.get("ORM_TEST_DATABASE_URL"):
        await run("postgresql", os.environ["ORM_TEST_DATABASE_URL"])
    print("Python compiled query defaults: passed")


asyncio.run(main())
