"""update_many() takes expressions over the model's own columns, and update_values= takes a fixed expression map."""

import os
from datetime import datetime, timezone

import pytest

import monorm
from monorm import QueryError, excluded, func, param

SCHEMA = """
model Author {
  id Int @id
  name String @default("")
  items Item[]
  @@map("ume_author")
}
model Item {
  id Int @id
  author_id Int
  author Author @relation(fields: [author_id], references: [id])
  title String @default("")
  views Int @default(0)
  revenue Int @default(0)
  at_end Int?
  read_at DateTime?
  frozen Boolean @default(false)
  @@map("ume_item")
}
"""
AT = datetime(2026, 1, 1, tzinfo=timezone.utc)


async def connect(provider):
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL")
    if not url:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    registry = monorm.Registry()
    models = monorm.loads(f'datasource db {{\n provider = "{provider}"\n}}\n' + SCHEMA, registry=registry)
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    Item = models["Item"]
    items = Item.objects.using(db)
    await models["Author"].objects.using(db).insert_many([{"id": 1, "name": "A"}, {"id": 2, "name": "B"}, {"id": 9}])
    await items.insert_many(
        [
            {"id": 1, "author_id": 1, "title": "a", "views": 5, "revenue": 10},
            {"id": 2, "author_id": 1, "title": "b", "views": 7, "revenue": 20},
            {"id": 3, "author_id": 2, "title": "c", "views": 1, "revenue": 30, "frozen": True},
        ]
    )
    return db, Item, items


async def sql_of(db, statement):
    events = []
    remove = db.on_query(events.append)
    try:
        result = await statement
    finally:
        remove()
    return result, [e.sql for e in events if e.sql.lstrip().upper().startswith("UPDATE")]


async def views(Item, items):
    return {i.id: i.views for i in await items.order_by(Item.id).all()}


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_many_adds_to_a_column_in_one_statement(provider):
    db, Item, items = await connect(provider)
    try:
        rows = [{"id": 1, "views": Item.views + 1}, {"id": 2, "views": Item.views + 5}]
        # One shape, one statement: the literals of the rows become VALUES cells.
        count, sql = await sql_of(db, items.update_many(rows))
        assert count == 2
        assert len(sql) == 1 and "VALUES" in sql[0] and "views" in sql[0]
        assert await views(Item, items) == {1: 6, 2: 12, 3: 1}
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_many_runs_one_statement_per_shape(provider):
    db, Item, items = await connect(provider)
    try:
        rows = [{"id": 1, "views": Item.views + 1}, {"id": 2, "views": Item.views * 2}, {"id": 3, "views": Item.views + 10}]
        count, sql = await sql_of(db, items.update_many(rows))
        assert count == 3
        assert len(sql) == 2
        assert await views(Item, items) == {1: 6, 2: 14, 3: 11}
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_many_mixes_plain_and_expression_cells_in_a_column(provider):
    db, Item, items = await connect(provider)
    try:
        rows = [{"id": 1, "title": "x", "views": Item.views + 1}, {"id": 2, "title": "y", "views": 100}]
        count, sql = await sql_of(db, items.update_many(rows))
        assert count == 2
        assert len(sql) == 2
        got = {i.id: (i.title, i.views) for i in await items.order_by(Item.id).all()}
        assert got == {1: ("x", 6), 2: ("y", 100), 3: ("c", 1)}
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_many_reads_case_and_coalesce_in_a_row(provider):
    db, Item, items = await connect(provider)
    try:
        rows = [
            {"id": 1, "at_end": func.coalesce(Item.at_end, Item.views)},
            {"id": 2, "at_end": func.coalesce(Item.at_end, Item.views)},
        ]
        await items.update_many(rows)
        rows = [{"id": 1, "views": func.case((Item.views > 100, 1), default=Item.views + 1)}, {"id": 3, "views": 9}]
        await items.update_many(rows)
        got = {i.id: (i.views, i.at_end) for i in await items.order_by(Item.id).all()}
        assert got == {1: (6, 5), 2: (7, 7), 3: (9, None)}
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_many_applies_the_filters_of_the_query_set(provider):
    db, Item, items = await connect(provider)
    try:
        count = await items.filter(Item.frozen == False).update_many(  # noqa: E712
            [{"id": 1, "views": Item.views + 1}, {"id": 3, "views": Item.views + 1}]
        )
        assert count == 1
        assert await views(Item, items) == {1: 6, 2: 7, 3: 1}
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_many_falls_back_to_case_with_an_expression(provider):
    db, Item, items = await connect(provider)
    try:
        native = Item._meta.registry.native()
        statement = items.update_many([{"id": 1, "views": Item.views + 1}, {"id": 2, "views": Item.views + 2}])
        params: list = []
        many = statement._many(params)
        (sql,) = native.update_many_sql(
            "Item", statement._fields, statement._rows, "[]", params, None, ["update_from_values"], many
        )
        assert "CASE" in sql and "VALUES" not in sql and '"ume_item"."views" + 1' in sql
        await statement
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_many_update_values_read_excluded(provider):
    db, Item, items = await connect(provider)
    try:
        rows = [{"id": 1, "views": 3}, {"id": 2, "views": 4}]
        count, sql = await sql_of(db, items.update_many(rows, update_values={"views": Item.views + excluded(Item.views)}))
        assert count == 2
        assert len(sql) == 1 and "VALUES" in sql[0]
        assert await views(Item, items) == {1: 8, 2: 11, 3: 1}
        # A field in the rows and in update_values: update_values wins, and the rows still name the VALUES column.
        await items.update_many(
            [{"id": 1, "views": 100}, {"id": 2, "views": 200}], update_values={"views": excluded(Item.views) + 1}
        )
        assert await views(Item, items) == {1: 101, 2: 201, 3: 1}
        # A field only in update_values gets a fixed value for every row.
        await items.update_many([{"id": 1, "views": 0}, {"id": 2, "views": 0}], update_values={"title": "same"})
        assert [i.title for i in await items.order_by(Item.id).all()] == ["same", "same", "c"]
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_a_prepared_update_many_reads_excluded_through_unnest(provider):
    db, Item, items = await connect(provider)
    try:
        await items.insert_many([{"id": i, "author_id": 9, "views": i} for i in range(10, 50)])
        prepared = items.update_many(
            param("rows", fields=[Item.id, Item.views]), update_values={"views": Item.views + excluded(Item.views)}
        ).prepare()
        assert await prepared(rows=[{"id": 1, "views": 3}, {"id": 2, "views": 4}]) == 2
        many = [{"id": i, "views": 1000} for i in range(10, 50)]
        assert await prepared(rows=many) == 40
        got = await views(Item, items)
        assert got[1] == 8 and got[2] == 11 and got[10] == 1010 and got[49] == 1049
        with pytest.raises(TypeError, match="a prepared update_many takes plain values"):
            await prepared(rows=[{"id": 1, "views": Item.views + 1}])
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_many_returns_the_rows_of_the_revenue_case(provider):
    db, Item, items = await connect(provider)
    try:
        revenue = excluded(Item.revenue)
        read_first = Item.read_at.is_null() | (Item.read_at > AT)
        at_end = func.case(
            (Item.at_end.is_null() & (Item.frozen == False), func.case((read_first, revenue), default=Item.revenue)),
            default=Item.at_end,
        )
        written = {
            r.id: r.at_end
            for r in await items.filter(Item.frozen == False)  # noqa: E712
            .update_many(
                [{"id": 1, "revenue": 111}, {"id": 2, "revenue": 222}, {"id": 3, "revenue": 333}],
                update_values={"at_end": at_end, "read_at": AT},
            )
            .returning(Item.id, Item.at_end)
        }
        assert written == {1: 111, 2: 222}
        # Second call: at_end is set, so the CASE keeps it.
        rows = await items.update_many([{"id": 1, "revenue": 5}], update_values={"at_end": at_end}).returning(
            Item.id, Item.at_end
        )
        written = {r.id: r.at_end for r in rows}
        assert written == {1: 111}
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_many_rolls_back_every_group_when_one_fails(provider):
    db, Item, items = await connect(provider)
    try:
        # Two shapes, two statements: the second sets NULL in a NOT NULL column.
        rows = [{"id": 1, "views": Item.views + 1}, {"id": 2, "views": Item.at_end}]
        with pytest.raises(Exception):
            await items.update_many(rows)
        assert await views(Item, items) == {1: 5, 2: 7, 3: 1}
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_many_expression_errors(provider):
    db, Item, items = await connect(provider)
    try:
        with pytest.raises(TypeError, match="primary key must be a plain value"):
            await items.update_many([{"id": Item.views + 1, "views": 1}])
        with pytest.raises(QueryError, match="relation path"):
            await items.update_many([{"id": 1, "views": Item.author.id}])
        with pytest.raises(QueryError, match="excluded"):
            await items.update_many([{"id": 1, "views": excluded(Item.views)}])
        with pytest.raises(QueryError, match="param"):
            await items.update_many([{"id": 1, "views": param("x")}])
        with pytest.raises(QueryError, match="needs views in the rows"):
            await items.update_many([{"id": 1, "title": "x"}], update_values={"views": excluded(Item.views)})
        with pytest.raises(QueryError, match="expression in a row and also in update_values"):
            await items.update_many([{"id": 1, "views": Item.views + 1}], update_values={"views": 3})
        with pytest.raises(ValueError, match="appears twice"):
            await items.update_many([{"id": 1, "views": Item.views + 1}, {"id": 1, "views": Item.views * 2}])
        with pytest.raises(QueryError, match="update_values cannot set id"):
            await items.update_many([{"id": 1, "views": 3}], update_values={"id": 5})
        assert await views(Item, items) == {1: 5, 2: 7, 3: 1}
    finally:
        await db.close()


PAIRS = """
model Pair {
  a Int
  b Int
  views Int @default(0)
  @@id([a, b])
  @@map("ume_pair")
}
"""


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_many_takes_expressions_on_a_composite_key(provider):
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL")
    if not url:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    registry = monorm.Registry()
    Pair = monorm.loads(f'datasource db {{\n provider = "{provider}"\n}}\n' + PAIRS, registry=registry)["Pair"]
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        pairs = Pair.objects.using(db)
        await pairs.insert_many([{"a": 1, "b": 1, "views": 1}, {"a": 1, "b": 2, "views": 2}, {"a": 2, "b": 1, "views": 3}])
        rows = [{"a": 1, "b": 2, "views": Pair.views + 10}, {"a": 2, "b": 1, "views": Pair.views * 2}]
        assert await pairs.update_many(rows) == 2
        assert await pairs.update_many([{"a": 1, "b": 1, "views": 7}], update_values={"views": Pair.views + excluded(Pair.views)}) == 1
        assert {(p.a, p.b): p.views for p in await pairs.all()} == {(1, 1): 8, (1, 2): 12, (2, 1): 6}
    finally:
        await db.close()
