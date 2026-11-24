"""A JSON-stored nested type takes no dynamic default; a `date` subfield stores a date; a DateTime subfield is stored as fixed-width UTC text."""

import datetime as dt
import json
import os
import re

import pytest

import monorm

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

COLUMNS = """
type Stamp {
  at DateTime @default(now())
  tok String @client_default(uuid())
  n Int
}

model Cols {
  id Int   @id
  s  Stamp
}
"""

DAY = """
type Dated {
  day DateTime @db.Date
}

model Event {
  id Int   @id
  d  Dated @storage(json)
}

model EventCols {
  id Int   @id
  d  Dated
}
"""

MOMENT = """
type Moment {
  at DateTime
}

model Clock {
  id Int    @id
  m  Moment @storage(json)
}
"""

SQLITE = 'datasource db { provider = "sqlite" }\n'
WHEN = dt.datetime(2024, 1, 2, 3, 4, 5, tzinfo=dt.UTC)


@pytest.fixture(params=["postgres", "sqlite"])
async def world(request):
    registry = monorm.Registry()
    schema = COLUMNS + DAY + MOMENT
    if request.param == "sqlite":
        classes = monorm.loads(SQLITE + schema, registry=registry)
        db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    else:
        classes = monorm.loads(schema, registry=registry)
        try:
            db = await monorm.connect(PG_URL, registry=registry, default=False, max_connections=2)
        except monorm.DatabaseError as e:
            pytest.skip(f"Postgres not reachable at {PG_URL}: {e}")
    await db.drop_tables()
    await db.create_tables()
    yield db, classes
    await db.drop_tables()
    await db.close()


@pytest.mark.parametrize(
    "field",
    ["at DateTime @default(now())", "at String @client_default(\"x\")", "at String @default(dbgenerated(\"'x'\"))"],
)
def test_a_json_field_refuses_a_type_with_a_dynamic_default(field):
    schema = f"type Stamp {{\n  {field}\n}}\nmodel M {{\n  id Int @id\n  s Stamp @storage(json)\n}}"
    with pytest.raises(Exception, match=r"Stamp\.at has a dynamic default"):
        monorm.loads(schema, registry=monorm.Registry())


def test_a_json_field_refuses_a_dynamic_default_below_a_nested_level():
    schema = "type In {\n  at DateTime @default(now())\n}\ntype Out {\n  i In\n}\nmodel M {\n  id Int @id\n  o Out @storage(json)\n}"
    with pytest.raises(Exception, match=r"In\.at has a dynamic default"):
        monorm.loads(schema, registry=monorm.Registry())


async def test_a_columns_subfield_with_a_dynamic_default_may_be_left_out(world):
    db, classes = world
    Cols = classes["Cols"]
    await Cols.objects.using(db).insert(id=1, s={"n": 1})
    got = (await Cols.objects.using(db).get(Cols.id == 1)).s
    assert got.n == 1
    assert got.at is not None
    assert got.tok


async def test_a_datetime_for_a_date_subfield_is_stored_as_a_date(world):
    db, classes = world
    Event, EventCols = classes["Event"], classes["EventCols"]
    await Event.objects.using(db).insert_many([{"id": 1, "d": {"day": WHEN}}, {"id": 2, "d": {"day": WHEN.date()}}])
    await EventCols.objects.using(db).insert(id=1, d={"day": WHEN})
    assert {e.id: e.d.day for e in await Event.objects.using(db).order_by(Event.id)} == {1: WHEN.date(), 2: WHEN.date()}
    assert (await EventCols.objects.using(db).get(EventCols.id == 1)).d.day == WHEN.date()


async def test_a_datetime_subfield_is_stored_as_fixed_width_utc_text_that_sorts_by_time(world):
    db, classes = world
    plus3 = dt.timezone(dt.timedelta(hours=3))
    times = [
        dt.datetime(2024, 1, 2, 3, 4, 5, 123456, tzinfo=dt.UTC),
        dt.datetime(2024, 1, 2, 3, 4, 5, tzinfo=dt.UTC),
        dt.datetime(2024, 1, 2, 6, 4, 5, 1, tzinfo=plus3),
        dt.datetime(2023, 12, 31, 23, 59, 59, 999999, tzinfo=dt.UTC),
    ]
    await classes["Clock"].objects.using(db).insert_many([{"id": i, "m": {"at": t}} for i, t in enumerate(times)])
    rows = await db.fetch("SELECT id, m FROM clock")
    texts = {r["id"]: (json.loads(r["m"]) if isinstance(r["m"], str) else r["m"])["at"] for r in rows}
    assert all(re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{6}Z", t) for t in texts.values())
    assert sorted(texts, key=texts.get) == sorted(texts, key=lambda i: times[i].astimezone(dt.UTC))
    assert texts[1] == "2024-01-02T03:04:05.000000Z"
    assert texts[2] == "2024-01-02T03:04:05.000001Z"
