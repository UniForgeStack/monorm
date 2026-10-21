"""A columns group keeps the nullability of its model field, whatever the levels below it hold."""

import os

import pytest

import orm

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SCHEMA = """
type Geo {
  lat Float
  lng Float?
}

type Wrap {
  inner Geo?
}

type Tagged {
  tag   String
  inner Geo?
}

model Plain {
  id Int @id
  g  Geo
}

model Wrapped {
  id Int  @id
  w  Wrap
}

model Opt {
  id Int     @id
  t  Tagged?
}
"""

SQLITE = 'datasource db { provider = "sqlite" }\n'


@pytest.fixture(params=["postgres", "sqlite"])
async def world(request):
    registry = orm.Registry()
    if request.param == "sqlite":
        classes = orm.loads(SQLITE + SCHEMA, registry=registry)
        db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    else:
        classes = orm.loads(SCHEMA, registry=registry)
        try:
            db = await orm.connect(PG_URL, registry=registry, default=False, max_connections=2)
        except orm.DatabaseError as e:
            pytest.skip(f"Postgres not reachable at {PG_URL}: {e}")
    await db.drop_tables()
    await db.create_tables()
    yield db, classes
    await db.drop_tables()
    await db.close()


async def test_a_non_null_columns_group_loads_and_round_trips(world):
    db, classes = world
    Geo, Plain = classes["Geo"], classes["Plain"]
    await Plain.objects.using(db).insert_many([{"id": 1, "g": Geo(lat=1.5)}, {"id": 2, "g": Geo(lat=2.5, lng=3.5)}])
    rows = {p.id: p.g for p in await Plain.objects.using(db).order_by(Plain.id)}
    assert rows == {1: Geo(lat=1.5, lng=None), 2: Geo(lat=2.5, lng=3.5)}


async def test_a_non_null_group_keeps_a_value_whose_middle_level_is_null(world):
    db, classes = world
    Geo, Wrap, Wrapped = classes["Geo"], classes["Wrap"], classes["Wrapped"]
    Tagged, Opt = classes["Tagged"], classes["Opt"]
    await Wrapped.objects.using(db).insert_many([{"id": 1, "w": Wrap(inner=None)}, {"id": 2, "w": Wrap(inner=Geo(lat=1.0))}])
    await Opt.objects.using(db).insert_many([{"id": 1, "t": None}, {"id": 2, "t": Tagged(tag="x", inner=None)}])
    wrapped = {r.id: r.w for r in await Wrapped.objects.using(db).order_by(Wrapped.id)}
    assert wrapped == {1: Wrap(inner=None), 2: Wrap(inner=Geo(lat=1.0, lng=None))}
    opt = {r.id: r.t for r in await Opt.objects.using(db).order_by(Opt.id)}
    assert opt == {1: None, 2: Tagged(tag="x", inner=None)}


async def test_a_non_null_group_refuses_none(world):
    db, classes = world
    Plain, Wrapped = classes["Plain"], classes["Wrapped"]
    with pytest.raises(ValueError, match="not nullable"):
        await Wrapped.objects.using(db).insert(id=1, w=None)
    with pytest.raises(ValueError, match="not nullable"):
        await Plain.objects.using(db).insert(id=1, g=None)
    with pytest.raises(Exception, match="g"):
        await Plain.objects.using(db).insert(id=1)
    assert await Wrapped.objects.using(db).count() == 0
    assert await Plain.objects.using(db).count() == 0


async def test_an_omitted_non_null_group_of_optional_subfields_is_an_empty_value(world):
    db, classes = world
    Wrap, Wrapped = classes["Wrap"], classes["Wrapped"]
    await Wrapped.objects.using(db).insert(id=1)
    assert (await Wrapped.objects.using(db).get(Wrapped.id == 1)).w == Wrap(inner=None)
