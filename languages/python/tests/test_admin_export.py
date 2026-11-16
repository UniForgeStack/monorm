"""Admin CSV export: the list state as a stream of keyset batches, the cells, the audit row, on PostgreSQL and SQLite."""

import csv
import io
import os
from types import SimpleNamespace

import pytest

import monorm
from monorm import admin, param

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SOURCE = """
datasource db {
  provider = "%(provider)s"
}
import "app:monorm-admin" (prefix: "admin_")

model ExpOwner {
  id    Int       @id
  name  String
  items ExpItem[]
  @@map("exp_owners")
}

model ExpItem {
  id       Int      @id
  owner_id Int
  title    String
  rank     Int
  big      BigInt
  owner    ExpOwner @relation(fields: [owner_id], references: [id])
  @@index([title])
  @@map("exp_items")
}
"""

ITEMS = "/admin/api/export/ExpItem"
OWNERS = "/admin/api/export/ExpOwner"
SU = {"key": 1, "is_superuser": True}


def table(response):
    assert response.status == 200, response.text
    return list(csv.reader(io.StringIO(response.text, newline="")))


def titles(response):
    return [row[0] for row in table(response)[1:]]


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request, tmp_path):
    (tmp_path / "schema.prisma").write_text(SOURCE % {"provider": request.param})
    registry = monorm.Registry()
    models = monorm.load(tmp_path / "schema.prisma", registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else PG_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    Owner, Item = models["ExpOwner"], models["ExpItem"]
    await Owner.objects.using(db).insert_many([{"id": 1, "name": "Ada"}, {"id": 2, "name": "Bo"}])
    await Item.objects.using(db).insert_many(
        [
            {"id": 1, "owner_id": 1, "title": "one", "rank": 1, "big": 1},
            {"id": 2, "owner_id": 1, "title": "two", "rank": 2, "big": 2},
            {"id": 3, "owner_id": 2, "title": "three", "rank": 3, "big": 9007199254740993},
            {"id": 4, "owner_id": 1, "title": "four", "rank": 3, "big": 4},
        ]
    )
    yield SimpleNamespace(provider=request.param, registry=registry, db=db, Owner=Owner, Item=Item, models=models)
    await db.drop_tables()
    await db.close()


def site_of(w, item=None, owner=None, queryset=None):
    site = admin.Admin(w.registry)
    I, O = w.Item, w.Owner
    config = {
        "columns": [I.title, I.rank, I.big, I.owner.name, "title_length"],
        "computed": [{"name": "title_length", "type": "int", "nullable": False, "sortable": True, "expr": monorm.func.length(I.title)}],
        "order": [I.id],
        "search": [I.title],
        "filters": [I.rank, I.owner],
    }
    config.update(item or {})
    owners = {"columns": [O.name, "item_count"], "aggregates": [{"name": "item_count", "label": "Items", "relation": O.items, "fn": "count"}], "order": [O.id]}
    owners.update(owner or {})
    site.register(I, {"list": config, **({"queryset": queryset} if queryset is not None else {})})
    site.register(O, {"list": owners})
    return site


def client_of(w, **kw):
    return site_of(w, **kw).test_client(w.db, user=SU)


async def exports(w):
    entry = w.models["AdminLogEntry"]
    rows = await entry.objects.using(w.db).filter(entry.action == "export").order_by(entry.id)
    return [(r.action, r.resource, r.record_key, r.changes) for r in rows]


async def test_the_export_is_the_list_as_csv_with_a_to_one_and_a_computed_column(world):
    r = await client_of(world).get(ITEMS)
    assert r.status == 200, r.text
    headers = {k.lower(): v for k, v in r.headers}
    assert headers["content-type"] == "text/csv; charset=utf-8"
    assert headers["content-disposition"] == 'attachment; filename="ExpItem.csv"'
    assert r.text.endswith("\r\n")
    assert table(r) == [
        ["Title", "Rank", "Big", "Owner name", "Title length", "Id"],
        ["one", "1", "1", "Ada", "3", "1"],
        ["two", "2", "2", "Ada", "3", "2"],
        ["three", "3", "9007199254740993", "Bo", "5", "3"],
        ["four", "3", "4", "Ada", "4", "4"],
    ]


async def test_an_aggregate_column_is_in_the_export(world):
    assert table(await client_of(world).get(OWNERS)) == [["Name", "Items", "Id"], ["Ada", "3", "1"], ["Bo", "1", "2"]]


async def test_the_search_the_filters_and_the_sort_of_the_list_apply(world):
    client = client_of(world)
    assert titles(await client.get(f"{ITEMS}?q=t")) == ["two", "three"]
    assert titles(await client.get(f"{ITEMS}?f.rank=3")) == ["three", "four"]
    assert titles(await client.get(f"{ITEMS}?f.owner=1&f.rank=3")) == ["four"]
    assert titles(await client.get(f"{ITEMS}?sort=-title_length")) == ["three", "four", "one", "two"]
    assert titles(await client.get(f"{ITEMS}?sort=-id&q=t")) == ["three", "two"]
    assert titles(await client.get(f"{ITEMS}?cursor=zz&page=9&limit=1")) == ["one", "two", "three", "four"]


async def test_the_queryset_filter_limits_the_export(world):
    mine = world.Item.objects.filter(world.Item.owner_id == param("user")).prepare()
    client = client_of(world, queryset=mine)
    assert titles(await client.get(ITEMS)) == ["one", "two", "four"]
    other = site_of(world, queryset=mine).test_client(world.db, user={"key": 2, "is_superuser": True})
    assert titles(await other.get(ITEMS)) == ["three"]


async def test_a_cell_that_starts_like_a_formula_is_escaped_and_a_special_cell_is_quoted(world):
    await world.Item.objects.using(world.db).insert_many(
        [
            {"id": 5, "owner_id": 1, "title": "=SUM(A1)", "rank": 5, "big": 5},
            {"id": 6, "owner_id": 1, "title": "+1 @x", "rank": 5, "big": 6},
            {"id": 7, "owner_id": 1, "title": "-9", "rank": 5, "big": 7},
            {"id": 8, "owner_id": 1, "title": "a,b \"c\"\nd", "rank": 5, "big": 8},
            {"id": 9, "owner_id": 1, "title": "fine=", "rank": 5, "big": 9},
        ]
    )
    r = await client_of(world).get(f"{ITEMS}?f.rank=5")
    assert titles(r) == ["'=SUM(A1)", "'+1 @x", "'-9", 'a,b "c"\nd', "fine="]
    assert '"a,b ""c""\nd"' in r.text, "a cell with a comma, a quote or a line break is quoted"
    assert [row[2] for row in table(r)[1:]] == ["5", "6", "7", "8", "9"], "a number is never escaped"


async def test_a_big_export_is_read_in_keyset_batches_of_a_thousand_rows_and_a_batch_function_runs_once_per_batch(world):
    batches = []

    async def doubled(keys):
        batches.append(len(keys))
        return [key * 2 for key in keys]

    await world.Item.objects.using(world.db).insert_many([{"id": 10 + i, "owner_id": 1, "title": f"m{i:04}", "rank": 7, "big": i} for i in range(2500)])
    computed = [{"name": "doubled", "type": "int", "nullable": False, "function": doubled}]
    client = client_of(world, item={"columns": [world.Item.title, "doubled"], "computed": computed})
    r = await client.get(f"{ITEMS}?f.rank=7")
    rows = table(r)
    assert batches == [1000, 1000, 500]
    assert rows[0] == ["Title", "Doubled", "Id"]
    assert len(rows) == 2501
    assert [(row[0], row[1], row[2]) for row in (rows[1], rows[1000], rows[1001], rows[2500])] == [
        ("m0000", "20", "10"),
        ("m0999", "2018", "1009"),
        ("m1000", "2020", "1010"),
        ("m2499", "5018", "2509"),
    ]
    assert [row[2] for row in rows[1:]] == [str(10 + i) for i in range(2500)], "every row once, in keyset order"


async def test_each_export_writes_one_audit_row_with_the_query_and_a_refused_export_writes_none(world):
    client = client_of(world)
    assert (await client.get(f"{ITEMS}?q=t&sort=-id")).status == 200
    assert await exports(world) == [("export", "ExpItem", "", {"query": "q=t&sort=-id"})]
    assert (await client.get(f"{ITEMS}?sort=nothing")).status == 400
    stranger = site_of(world).test_client(world.db, user={"key": 7})
    assert (await stranger.get(ITEMS)).status == 403
    assert (await site_of(world).test_client(world.db).get(ITEMS)).status == 401
    assert len(await exports(world)) == 1


async def test_a_bad_request_is_a_plain_json_error_before_the_file_starts(world):
    client = client_of(world)
    bad = await client.get(f"{ITEMS}?sort=nothing")
    assert bad.status == 400
    assert bad.json()["error"]["kind"] == "BadRequest"
    assert (await client.get(f"{ITEMS}?f.rank=x")).status == 422
    assert (await client.get("/admin/api/export/Nothing")).status == 404
