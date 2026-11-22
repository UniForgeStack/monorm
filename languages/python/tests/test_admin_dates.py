"""Admin date drill-down: the `dates` route and the `date_hierarchy` list key, on PostgreSQL and SQLite."""

import os
from datetime import date, datetime, timezone
from types import SimpleNamespace

import pytest

import monorm
from monorm import admin, param

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SOURCE = """
datasource db {
  provider = "%(provider)s"
}

model DtPerson {
  id     Int       @id
  name   String
  joined DateTime  @db.Date
  events DtEvent[]
  @@map("dt_people")
}

model DtEvent {
  id        Int      @id
  title     String
  person_id Int
  at        DateTime
  day       DateTime? @db.Date
  person    DtPerson @relation(fields: [person_id], references: [id])
  @@index([at])
  @@map("dt_events")
}
"""

SU1 = {"key": 1, "is_superuser": True}
SU2 = {"key": 2, "is_superuser": True}
DATES = "/admin/api/dates/DtEvent"
MONTHS = "2026-01-01T00:00:00Z,2026-02-01T00:00:00Z,2026-03-01T00:00:00Z,2026-04-01T00:00:00Z"


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request):
    registry = monorm.Registry()
    models = monorm.loads(SOURCE % {"provider": request.param}, registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else PG_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    Person, Event = models["DtPerson"], models["DtEvent"]
    await Person.objects.using(db).insert_many(
        [{"id": 1, "name": "Ada", "joined": date(2025, 12, 31)}, {"id": 2, "name": "Bo", "joined": date(2026, 1, 15)}]
    )
    at = lambda *a: datetime(*a, tzinfo=timezone.utc)  # noqa: E731
    await Event.objects.using(db).insert_many(
        [
            {"id": 1, "title": "one", "person_id": 1, "at": at(2026, 1, 2, 3, 4, 5), "day": date(2026, 1, 2)},
            {"id": 2, "title": "two", "person_id": 1, "at": at(2026, 2, 1), "day": date(2026, 2, 1)},
            {"id": 3, "title": "three", "person_id": 2, "at": at(2026, 3, 1), "day": None},
            {"id": 4, "title": "four", "person_id": 1, "at": at(2026, 3, 15, 12), "day": date(2026, 3, 15)},
            {"id": 5, "title": "five", "person_id": 1, "at": at(2026, 3, 31, 22, 30), "day": date(2026, 3, 31)},
        ]
    )
    yield SimpleNamespace(registry=registry, db=db, Person=Person, Event=Event)
    await db.drop_tables()
    await db.close()


def client_of(w, hierarchy=None, user=SU1, **list_options):
    E = w.Event
    site = admin.Admin(w.registry)
    config = {"columns": [E.title], "order": [E.id], "search": [E.title], "date_hierarchy": E.at if hierarchy is None else hierarchy}
    site.register(E, {"list": {**config, **list_options}})
    site.register(w.Person)
    return site, site.test_client(w.db, user=user)


def as_instant(text):
    return datetime.fromisoformat(text.replace("Z", "+00:00"))


async def test_the_range_is_one_statement_and_follows_the_filters_and_the_search(world):
    _, client = client_of(world)
    preamble = len((await client.get("/admin/api/resources")).sql)
    reply = await client.get(DATES)
    assert reply.status == 200, reply.json()
    assert len(reply.sql) == preamble + 1
    body = reply.json()
    assert (as_instant(body["min"]), as_instant(body["max"])) == (datetime(2026, 1, 2, 3, 4, 5, tzinfo=timezone.utc), datetime(2026, 3, 31, 22, 30, tzinfo=timezone.utc))
    searched = (await client.get(DATES + "?q=th")).json()
    assert as_instant(searched["min"]) == as_instant(searched["max"]) == datetime(2026, 3, 1, tzinfo=timezone.utc), "the search narrows the range"
    assert as_instant((await client.get(DATES + "?f.at:gte=2026-02-01T00:00:00Z")).json()["min"]) == datetime(2026, 2, 1, tzinfo=timezone.utc)
    nothing = (await client.get(DATES + "?q=nothing")).json()
    assert (nothing["min"], nothing["max"]) == (None, None)


async def test_counts_per_month_use_the_edges_of_the_browser_zone_and_one_statement(world):
    _, client = client_of(world)
    preamble = len((await client.get("/admin/api/resources")).sql)
    reply = await client.get(f"{DATES}?edges={MONTHS}")
    assert reply.status == 200, reply.json()
    assert reply.json()["counts"] == [1, 1, 3] and len(reply.sql) == preamble + 1
    local = "2026-03-01T00:00:00%2B02:00,2026-04-01T00:00:00%2B02:00,2026-05-01T00:00:00%2B02:00"
    zoned = await client.get(f"{DATES}?edges={local}")
    assert zoned.json()["counts"] == [2, 1], "the row at 22:30Z on March 31 is April 1 at +02:00"
    assert len(zoned.sql) == preamble + 1
    assert (await client.get(f"{DATES}?edges={MONTHS}&q=o")).json()["counts"] == [1, 1, 1], "the search applies"


async def test_counts_of_a_date_field_use_plain_dates_and_a_null_is_in_no_period(world):
    _, client = client_of(world, world.Event.day)
    reply = await client.get(DATES + "?edges=2026-01-01,2026-02-01,2026-03-01,2026-04-01")
    assert reply.json()["counts"] == [1, 1, 2], "row 3 has no day"
    body = (await client.get(DATES)).json()
    assert (body["min"], body["max"]) == ("2026-01-02", "2026-03-31")


async def test_a_bad_edge_list_answers_400(world):
    _, client = client_of(world)
    years = [f"{1990 + i}-01-01T00:00:00Z" for i in range(62)]
    cases = {
        "equal edges": "2026-01-01T00:00:00Z,2026-01-01T00:00:00Z",
        "falling edges": "2026-02-01T00:00:00Z,2026-01-01T00:00:00Z",
        "one edge": "2026-01-01T00:00:00Z",
        "a bad instant": "2026-01-01T00:00:00Z,yesterday",
        "a date without a time": "2026-01-01,2026-02-01",
        "62 edges": ",".join(years),
    }
    for what, edges in cases.items():
        assert (await client.get(f"{DATES}?edges={edges}")).status == 400, what
    assert (await client.get(f"{DATES}?edges={','.join(years[:61])}")).status == 200, "61 edges are the most"
    _, by_day = client_of(world, world.Event.day)
    assert (await by_day.get(f"{DATES}?edges=2026-01-01T00:00:00Z,2026-02-01T00:00:00Z")).status == 400, "an instant for a date field"


async def test_a_resource_without_date_hierarchy_answers_404(world):
    site = admin.Admin(world.registry)
    site.register(world.Event, {"list": {"columns": [world.Event.title]}})
    site.register(world.Person)
    assert (await site.test_client(world.db, user=SU1).get(DATES)).status == 404


async def test_the_row_scope_limits_the_range_and_the_counts(world):
    E = world.Event
    scoped = E.objects.filter(E.person_id == param("user")).prepare()
    _, owner2 = client_of(world, user=SU2)
    site = admin.Admin(world.registry)
    site.register(E, {"queryset": scoped, "list": {"columns": [E.title], "order": [E.id], "date_hierarchy": E.at}})
    site.register(world.Person)
    client = site.test_client(world.db, user=SU2)
    assert (await client.get(f"{DATES}?edges={MONTHS}")).json()["counts"] == [0, 0, 1], "person 2 holds one row"
    body = (await client.get(DATES)).json()
    assert as_instant(body["min"]) == as_instant(body["max"]) == datetime(2026, 3, 1, tzinfo=timezone.utc)
    assert (await owner2.get(f"{DATES}?edges={MONTHS}")).json()["counts"] == [1, 1, 3], "without the queryset: every row"


async def test_a_field_of_a_to_one_relation_counts_the_rows_of_the_list(world):
    E = world.Event
    _, client = client_of(world, E.person.joined)
    reply = await client.get(DATES + "?edges=2025-12-01,2026-01-01,2026-02-01")
    assert reply.status == 200, reply.json()
    assert reply.json()["counts"] == [4, 1], "four events of Ada (joined in December) and one of Bo"
    meta = next(r for r in (await client.get("/admin/api/resources")).json()["resources"] if r["name"] == "DtEvent")
    assert meta["date_hierarchy"] == {"field": "person.joined", "counts": True, "kind": "date"}
    assert "person.joined" in [f["name"] for f in meta["filters"]], "the field joins the filters"


async def test_counts_false_reaches_the_metadata_and_the_field_joins_the_filters(world):
    E = world.Event
    _, client = client_of(world, {"field": E.at, "counts": False})
    meta = next(r for r in (await client.get("/admin/api/resources")).json()["resources"] if r["name"] == "DtEvent")
    assert meta["date_hierarchy"] == {"field": "at", "counts": False, "kind": "datetime"}
    assert [f["name"] for f in meta["filters"]] == ["at"]
    _, plain = client_of(world, filters=[E.day])
    meta = next(r for r in (await plain.get("/admin/api/resources")).json()["resources"] if r["name"] == "DtEvent")
    assert [f["name"] for f in meta["filters"]] == ["day", "at"], "a listed filter stays and the date field follows"


async def test_a_date_hierarchy_that_does_not_fit_is_a_build_error_or_a_warning(world):
    E = world.Event
    for hierarchy, text in [(E.title, "takes a date or datetime"), ("nope", "date_hierarchy"), ({"counts": True}, "needs a field")]:
        site = admin.Admin(world.registry)
        site.register(E, {"list": {"columns": [E.title], "date_hierarchy": hierarchy}})
        site.register(world.Person)
        with pytest.raises(admin.AdminConfigError, match=text):
            site.check()
    indexed, _ = client_of(world)
    assert not [w for w in indexed.check() if "date_hierarchy" in w], "`at` leads an index"
    plain, _ = client_of(world, world.Event.day)
    warnings = [w for w in plain.check() if "date_hierarchy" in w]
    assert len(warnings) == 1 and "DtEvent" in warnings[0] and "day" in warnings[0] and "index" in warnings[0]
