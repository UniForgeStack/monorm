"""Admin lists: keyset paging, the capped count, sort, search, and the statement timeout, on PostgreSQL and SQLite."""

import os
from datetime import datetime, timezone
from types import SimpleNamespace

import pytest

import monorm
from monorm import admin

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SOURCE = """
datasource db {
  provider = "%(provider)s"
}

model AdmNote {
  id    BigInt   @id @default(autoincrement())
  title String
  body  String
  rank  Int
  at    DateTime
  @@index([title])
  @@map("adm_notes")
}
"""

SU = {"is_superuser": True}
NOTES = "/admin/api/resources/AdmNote"
TITLES = ["alpha", "beta", "gamma", "delta", "epsilon"]
LIST = {"columns": ["title", "rank"], "order": ["id"], "page_size": 2}


def titles(response):
    return [row[0] for row in response.json()["rows"]]


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request):
    registry = monorm.Registry()
    Note = monorm.loads(SOURCE % {"provider": request.param}, registry=registry)["AdmNote"]
    url = "sqlite://:memory:" if request.param == "sqlite" else PG_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    at = datetime(2026, 1, 2, 3, 4, 5, tzinfo=timezone.utc)
    bodies = ["red apple", "green pear", "blue plum", "red plum", "white fig"]
    ranks = [1, 2, 2, 3, 1]
    await Note.objects.using(db).insert_many(
        [{"title": t, "body": b, "rank": r, "at": at} for t, b, r in zip(TITLES, bodies, ranks)]
    )

    yield SimpleNamespace(provider=request.param, db=db, Note=Note, make=lambda **options: admin.Admin(registry, **options))
    await db.drop_tables()
    await db.close()


def register(w, list_config=None, **options):
    site = w.make(**options)
    config = {"columns": [w.Note.title, w.Note.rank], "order": [w.Note.id], "page_size": 2}
    config.update(list_config or {})
    site.register(w.Note, {"list": config})
    return site, site.test_client(w.db, user=SU)


async def test_first_last_and_before_walk_the_keyset_both_ways_in_one_statement_each(world):
    site, client = register(world)
    preamble = (await client.get("/admin/api/resources")).queries
    last = await client.get(f"{NOTES}?last=1")
    assert last.queries == preamble + 1
    assert titles(last) == ["delta", "epsilon"]
    assert last.json()["next"] is None
    middle = await client.get(f"{NOTES}?before={last.json()['prev']}")
    assert titles(middle) == ["beta", "gamma"]
    first = await client.get(f"{NOTES}?before={middle.json()['prev']}")
    assert titles(first) == ["alpha"]
    assert first.json()["prev"] is None
    forth = await client.get(f"{NOTES}?cursor={first.json()['next']}")
    assert titles(forth) == ["beta", "gamma"]


async def test_sort_orders_rows_and_a_cursor_belongs_to_its_sort(world):
    site, client = register(world, {"sortable": [world.Note.title, world.Note.rank]})
    page = await client.get(f"{NOTES}?sort=-title")
    assert page.status == 200
    assert titles(page) == ["gamma", "epsilon"]
    assert titles(await client.get(f"{NOTES}?sort=-title&cursor={page.json()['next']}")) == ["delta", "beta"]
    assert (await client.get(f"{NOTES}?sort=title&cursor={page.json()['next']}")).status == 400
    assert (await client.get(f"{NOTES}?sort=body")).status == 400
    assert (await client.get(f"{NOTES}?sort=nope")).status == 400


async def test_the_page_size_stops_at_200(world):
    site, client = register(world)
    assert (await client.get(f"{NOTES}?limit=200")).status == 200
    assert (await client.get(f"{NOTES}?limit=201")).status == 400


async def test_the_count_is_its_own_request_and_a_capped_count_has_a_limit(world):
    site, client = register(world)
    rows = await client.get(NOTES)
    assert "count" not in rows.json()
    capped = await client.get("/admin/api/counts/AdmNote")
    assert (capped.json()["count"], capped.json()["capped"]) == (5, False)
    assert "LIMIT" in capped.sql[-1]
    exact = await client.get("/admin/api/counts/AdmNote?exact=1")
    assert exact.json()["count"] == 5
    assert "LIMIT" not in exact.sql[-1]


async def test_a_page_jump_uses_an_offset_when_the_count_is_exact(world):
    site, client = register(world)
    page = await client.get(f"{NOTES}?page=3")
    assert page.status == 200
    assert titles(page) == ["epsilon"]
    assert "OFFSET" in page.sql[-1]
    assert "OFFSET" not in (await client.get(NOTES)).sql[-1]


async def test_search_uses_a_prefix_on_an_indexed_field_and_a_term_prefix_picks_the_strategy(world):
    site, client = register(world, {"search": [world.Note.title]})
    search = lambda q: f"{NOTES}?q={q}"  # noqa: E731
    assert titles(await client.get(search("al"))) == ["alpha"], "an index on the field gives a prefix"
    assert titles(await client.get(search("lp"))) == [], "a prefix is not contains"
    assert titles(await client.get(search("%3Dbeta"))) == ["beta"], "= is exact"
    assert titles(await client.get(search("%5Ede"))) == ["delta"], "^ is a prefix"
    assert titles(await client.get(search("%25"))) == [], "a wildcard is text"
    assert (await client.get(search("%40word"))).status == 400, "@ needs a full-text field"
    assert titles(await client.get(search("3"))) == ["gamma"], "a number also tries the primary key"
    assert (await client.get(search("%5Ede"))).sql[-1].count("LIKE") == 1


async def test_a_search_field_with_no_index_warns_and_one_with_no_strategy_is_an_error(world):
    indexed = world.make()
    indexed.register(world.Note, {"list": {"search": [world.Note.title]}})
    assert indexed.check() == []
    plain = world.make()
    plain.register(world.Note, {"list": {"search": [world.Note.body]}})
    warnings = plain.check()
    assert len(warnings) == 1 and "AdmNote" in warnings[0] and "body" in warnings[0]
    broken = world.make()
    broken.register(world.Note, {"list": {"search": [world.Note.at]}})
    with pytest.raises(admin.AdminConfigError, match="no search strategy"):
        broken.check()


async def test_search_with_a_function_makes_one_bridge_call_and_a_prepared_query_set_works_too(world):
    calls = []

    async def by_body(q, limit):
        calls.append((q, limit))
        return [row.id for row in await world.Note.objects.using(world.db).filter(world.Note.body.contains(q)).order_by(world.Note.id)]

    site, client = register(world, {"search_with": by_body})
    page = await client.get(f"{NOTES}?q=plum")
    assert titles(page) == ["gamma", "delta"]
    assert len(calls) == 1 and calls[0][0] == "plum"
    prepared = world.Note.objects.using(world.db).filter(world.Note.title == monorm.param("q")).prepare()
    site, client = register(world, {"search_with": prepared})
    assert titles(await client.get(f"{NOTES}?q=beta")) == ["beta"]
    assert titles(await client.get(f"{NOTES}?q=nobody")) == []


async def test_each_request_runs_in_a_statement_timeout_on_postgres(world):
    site, client = register(world)
    first = (await client.get(NOTES)).sql[0]
    assert (world.provider == "postgres") == first.startswith("SET LOCAL statement_timeout = 15000")
    if world.provider != "postgres":
        return

    async def slow(q, limit):
        await world.db.fetch("SELECT pg_sleep(5)::text")
        return []

    slowed, slow_client = register(world, {"search_with": slow}, statement_timeout=0.2)
    page = await slow_client.get(f"{NOTES}?q=x")
    assert page.status in (500, 504)
    assert "SET LOCAL statement_timeout = 200" == page.sql[0]
