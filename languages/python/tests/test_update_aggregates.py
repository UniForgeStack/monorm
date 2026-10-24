"""Same-path update aggregates share a row subquery on both databases."""

import pytest

import orm
from conftest import DATABASE_URL

SCHEMA = """
model Parent {
  id Int @id
  n Int @default(0)
  total BigInt?
  distinct_n Int @default(0)
  filtered_total BigInt?
  other_n Int @default(0)
  label String @default("old")
  children Child[]
  others Other[]
  @@map("ua_parent")
}
model Child {
  id Int @id
  parent_id Int
  parent Parent @relation(fields: [parent_id], references: [id])
  views Int
  @@map("ua_child")
}
model Other {
  id Int @id
  parent_id Int
  parent Parent @relation(fields: [parent_id], references: [id])
  @@map("ua_other")
}
"""


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_aggregates_share_only_their_relation_path(provider):
    registry = orm.Registry()
    models = orm.loads(f'datasource db {{\n provider = "{provider}"\n}}\n' + SCHEMA, registry=registry)
    Parent, Child, Other = (models[name] for name in ("Parent", "Child", "Other"))
    try:
        db = await orm.connect("sqlite://:memory:" if provider == "sqlite" else DATABASE_URL, registry=registry, default=False)
    except orm.DatabaseError as error:
        if provider == "postgresql":
            pytest.skip(f"Postgres not reachable at {DATABASE_URL}: {error}")
        raise
    try:
        await db.drop_tables()
        await db.create_tables()
        await Parent.objects.using(db).insert_many([{"id": 1}, {"id": 2}, {"id": 3}])
        await Child.objects.using(db).insert_many([
            {"id": 1, "parent_id": 1, "views": 5},
            {"id": 2, "parent_id": 1, "views": 5},
            {"id": 3, "parent_id": 1, "views": 20},
            {"id": 4, "parent_id": 2, "views": 7},
        ])
        await Other.objects.using(db).insert_many([{"id": 1, "parent_id": 1}, {"id": 2, "parent_id": 2}])
        query = Parent.objects.using(db).filter(Parent.id > 0).update(
            n=orm.func.count(Parent.children), label="updated",
            total=orm.func.sum(Parent.children.views),
            distinct_n=orm.func.count(Parent.children.views, distinct=True),
            filtered_total=orm.func.sum(Parent.children.views, filter=Parent.children.views > 10),
            other_n=orm.func.count(Parent.others),
        )
        assert query.inspect().statements == (orm.StatementInspection(scalar_correlated=2),)
        events = []
        remove = db.on_query(events.append)
        rows = sorted(await query.returning(), key=lambda row: row.id)
        remove()
        assert len(events) == 1
        sql = events[0].sql
        assert 'SET ("n", "total", "distinct_n", "filtered_total") = (SELECT' in sql
        assert sql.count("SELECT") == 2
        assert "DISTINCT" in sql and "FILTER" in sql and "LATERAL" not in sql
        assert [(r.n, r.total, r.distinct_n, r.filtered_total, r.other_n, r.label) for r in rows] == [
            (3, 30, 2, 20, 1, "updated"), (1, 7, 1, None, 1, "updated"), (0, None, 0, None, 0, "updated"),
        ]
        single = Parent.objects.using(db).update(n=orm.func.count(Parent.children))
        assert single.inspect().statements == (orm.StatementInspection(scalar_correlated=1),)
        events.clear()
        remove = db.on_query(events.append)
        assert await single == 3
        remove()
        assert 'SET "n" = (SELECT' in events[0].sql
        pair = Parent.objects.using(db).filter(Parent.id > 1).update(
            n=orm.func.count(Parent.children), total=orm.func.sum(Parent.children.views),
        )
        assert pair.inspect().statements == (orm.StatementInspection(scalar_correlated=1),)
        assert await pair == 2
        rows = await Parent.objects.using(db).order_by(Parent.id).all()
        assert [(r.n, r.total) for r in rows] == [(3, 30), (1, 7), (0, None)]
        wrapped = Parent.objects.using(db).update(
            n=orm.func.count(Parent.children) + 1,
            total=orm.func.sum(Parent.children.views),
        )
        assert wrapped.inspect().statements[0].scalar_correlated == 1
        events.clear()
        remove = db.on_query(events.append)
        rows = sorted(await wrapped.returning(), key=lambda row: row.id)
        remove()
        assert [(r.n, r.total) for r in rows] == [(4, 30), (2, 7), (1, None)]
        assert events[0].sql.count('FROM "ua_child"') == 1
        combined = Parent.objects.using(db).filter(Parent.id > 0).update(
            n=orm.func.case(
                (orm.func.count(Parent.children) > 0, orm.func.count(Parent.children) + orm.func.count(Parent.others)),
                default=99,
            ),
            total=orm.func.sum(Parent.children.views),
            other_n=orm.func.count(Parent.others) + 1, label="wrapped",
        )
        shape = combined.inspect().statements[0]
        assert shape.scalar_correlated == 1 and shape.derived_correlated == 2
        rows = sorted(await combined.returning(), key=lambda row: row.id)
        assert [(r.n, r.total, r.other_n, r.label) for r in rows] == [
            (4, 30, 2, "wrapped"), (2, 7, 2, "wrapped"), (99, None, 1, "wrapped"),
        ]
        repeated = Parent.objects.using(db).update(n=orm.func.count(Parent.children) + orm.func.count(Parent.children))
        assert repeated.inspect().statements[0].scalar_correlated == 1
        rows = sorted(await repeated.returning(), key=lambda row: row.id)
        assert [r.n for r in rows] == [6, 2, 0]
    finally:
        await db.drop_tables()
        await db.close()
