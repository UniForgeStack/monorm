"""Nested types through the Python API: the same scenario on JSON and columns storage, PostgreSQL and SQLite."""

import datetime as dt
import importlib
import json
import pickle
import shutil
from pathlib import Path
import os
import copy
from decimal import Decimal

import pytest

import orm
from orm import DecodeError, _native

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SCHEMA = """
type Geo {
  lat Float
  lng Float?
}

type Address {
  city   String
  floor  Int @default(7)
  big    BigInt
  open   Boolean
  opened DateTime
  geo    Geo?
}

model JShop {
  id      Int      @id
  address Address? @storage(json)
  @@map("jshop")
}

model CShop {
  id      Int      @id
  address Address?
  @@map("cshop")
}
"""

SQLITE = 'datasource db { provider = "sqlite" }\n'
WHEN = dt.datetime(2024, 1, 2, 3, 4, 5, tzinfo=dt.UTC)
LATER = dt.datetime(2025, 6, 7, 8, 9, 10, tzinfo=dt.UTC)


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
    yield db, classes, request.param
    await db.drop_tables()
    await db.close()


@pytest.fixture(params=["JShop", "CShop"])
def shop(request, world):
    db, classes, _ = world
    return classes[request.param].objects.using(db), classes[request.param], classes, db


def address(classes, **over):
    values = {"city": "Oslo", "big": 2**60, "open": True, "opened": WHEN, "geo": classes["Geo"](lat=1.5)}
    values.update(over)
    return classes["Address"](**values)


async def seed(shop):
    q, S, classes, _ = shop
    await q.insert_many(
        [
            {"id": 1, "address": address(classes)},
            {"id": 2, "address": address(classes, city="Bergen", floor=3, geo=classes["Geo"](lat=9.0, lng=2.5))},
            {"id": 3, "address": address(classes, city="Tromso", geo=None)},
            {"id": 4, "address": None},
        ]
    )


async def test_insert_round_trips_values_and_nulls(shop):
    q, S, classes, _ = shop
    await seed(shop)
    rows = {s.id: s.address for s in await q.order_by(S.id)}
    assert rows[1] == address(classes)
    assert rows[1].floor == 7  # the default
    assert rows[1].big == 2**60
    assert rows[1].opened == WHEN
    assert rows[1].geo == classes["Geo"](lat=1.5, lng=None)
    assert rows[2].geo.lng == 2.5
    assert rows[3].geo is None  # the nullable middle level
    assert rows[4] is None


async def test_insert_takes_a_dict_with_defaults_and_rejects_a_bad_one(shop):
    q, S, classes, _ = shop
    one = await q.insert(id=1, address={"city": "A", "big": 1, "open": False, "opened": WHEN})
    assert one.address.floor == 7 and one.address.geo is None
    with pytest.raises(ValueError, match="address.city"):
        await q.insert(id=2, address={"big": 1, "open": False, "opened": WHEN})
    with pytest.raises(TypeError, match="no subfield"):
        await q.insert(id=2, address={"nope": 1})
    with pytest.raises(TypeError, match="expected"):
        await q.insert(id=2, address={"city": 5, "big": 1, "open": False, "opened": WHEN})
    with pytest.raises((ValueError, TypeError), match="timezone"):
        await q.insert(id=2, address=address(classes, opened=dt.datetime(2024, 1, 1)))


async def test_filter_by_subfield_whole_value_and_null(shop):
    q, S, classes, _ = shop
    await seed(shop)
    async def pks(cond):
        return [s.id for s in await q.filter(cond).order_by(S.id)]

    assert await pks(S.address.city == "Oslo") == [1]
    assert await pks(S.address.floor < 7) == [2]
    assert await pks(S.address.geo.lat > 1) == [1, 2]
    assert await pks(S.address.geo.lng == 2.5) == [2]
    assert await pks(S.address.opened == WHEN) == [1, 2, 3]
    assert await pks(S.address.opened > WHEN) == []
    assert await pks(S.address.opened.in_([WHEN])) == [1, 2, 3]
    assert await pks(S.address.opened.not_in([WHEN])) == []
    assert await pks(S.address == None) == [4]  # noqa: E711
    assert await pks(S.address != None) == [1, 2, 3]  # noqa: E711
    assert await pks(S.address.geo == None) == [3, 4]  # noqa: E711  # a null field has a null middle level too
    assert await pks(S.address == address(classes)) == [1]
    # the omitted subfield equals null
    assert await pks(S.address.geo == {"lat": 1.5}) == [1]
    assert await pks(S.address.geo == classes["Geo"](lat=9.0, lng=2.5)) == [2]


async def test_ne_on_a_whole_field_is_three_valued(shop):
    q, S, classes, _ = shop
    await seed(shop)
    got = [s.id for s in await q.filter(S.address != address(classes)).order_by(S.id)]
    assert got == [2, 3]  # the null row matches neither == nor !=


async def test_order_by_a_subfield_and_never_by_a_whole_field(shop):
    q, S, classes, _ = shop
    await seed(shop)
    got = [s.id for s in await q.filter(S.address != None).order_by(S.address.city)]  # noqa: E711
    assert got == [2, 1, 3]
    got = [s.id for s in await q.filter(S.address.geo.lat > 0).order_by(S.address.geo.lat.desc())]
    assert got == [2, 1]
    with pytest.raises(Exception, match="order"):
        await q.order_by(S.address)
    with pytest.raises(Exception, match="order"):
        await q.order_by(S.address.geo)


async def test_select_gives_values_and_scalars(shop):
    q, S, classes, _ = shop
    await seed(shop)
    rows = await q.select(S.id, S.address.city, S.address.big, S.address.geo, S.address).order_by(S.id)
    first = rows[0]
    assert first[1] == "Oslo"
    assert first[2] == 2**60
    assert first[3] == classes["Geo"](lat=1.5)
    assert first[4] == address(classes)
    last = rows[3]
    assert last[1] is None and last[3] is None and last[4] is None
    assert rows[2][3] is None


async def test_leaf_names_are_hidden(shop):
    q, S, classes, _ = shop
    await seed(shop)
    s = await q.get(S.id == 1)
    assert "address_city" not in s.to_dict()
    assert set(s.to_dict()) == {"id", "address"}
    assert "address_city" not in repr(s)
    assert not hasattr(s, "address_city")
    if S.__name__ == "CShop":
        with pytest.raises(TypeError, match="part of 'address'"):
            await q.insert(id=9, address_city="x")
        with pytest.raises(TypeError, match="part of 'address'"):
            await q.filter(S.id == 1).update(address_city="x")


async def test_patch_changes_only_the_named_subfields(shop):
    q, S, classes, _ = shop
    await seed(shop)
    await q.filter(S.id == 1).update(address={"city": "Bergen", "geo": {"lng": 4.0}})
    a = (await q.get(S.id == 1)).address
    assert a.city == "Bergen" and a.floor == 7 and a.big == 2**60
    assert a.geo == classes["Geo"](lat=1.5, lng=4.0)
    await q.filter(S.id == 1).update(address={"floor": 9, "opened": LATER})
    a = (await q.get(S.id == 1)).address
    assert a.floor == 9 and a.opened == LATER and a.city == "Bergen"
    # a null leaf: SQLite deletes the key, PostgreSQL writes JSON null; both read as None
    await q.filter(S.id == 1).update(address={"geo": {"lng": None}})
    assert (await q.get(S.id == 1)).address.geo == classes["Geo"](lat=1.5, lng=None)


async def test_patch_validation(shop):
    q, S, classes, _ = shop
    await seed(shop)
    with pytest.raises(ValueError, match="at least one"):
        await q.filter(S.id == 1).update(address={})
    with pytest.raises(ValueError, match="not nullable"):
        await q.filter(S.id == 1).update(address={"city": None})
    with pytest.raises(TypeError, match="no subfield"):
        await q.filter(S.id == 1).update(address={"zip": "1"})
    with pytest.raises(TypeError, match="expected"):
        await q.filter(S.id == 1).update(address={"floor": "x"})


async def test_replace_sets_the_whole_value_and_resets_omitted_subfields(shop):
    q, S, classes, _ = shop
    await seed(shop)
    await q.filter(S.id == 2).update(address=address(classes, city="Replaced", geo=None))
    a = (await q.get(S.id == 2)).address
    assert a == address(classes, city="Replaced", geo=None)
    assert a.floor == 7  # the value class applies the default
    await q.filter(S.id == 4).update(address=address(classes, city="Filled"))  # replace sets a null field
    assert (await q.get(S.id == 4)).address.city == "Filled"


async def test_null_clears_the_whole_value(shop):
    q, S, classes, _ = shop
    await seed(shop)
    await q.filter(S.id == 1).update(address=None)
    assert (await q.get(S.id == 1)).address is None
    assert await q.filter(S.address == None).count() == 2  # noqa: E711
    await q.filter(S.id == 2).update(address={"geo": None})
    assert (await q.get(S.id == 2)).address.geo is None


async def test_update_many_and_instance_update(shop):
    q, S, classes, _ = shop
    await seed(shop)
    await q.update_many([{"id": 1, "address": address(classes, city="Many")}, {"id": 2, "address": None}])
    assert (await q.get(S.id == 1)).address.city == "Many"
    assert (await q.get(S.id == 2)).address is None
    s = await q.get(S.id == 3)
    await s.update(address={"city": "Inst"})
    assert s.address.city == "Inst"
    assert (await q.get(S.id == 3)).address.city == "Inst"


async def test_load_and_defer_keep_a_group_whole(shop):
    q, S, classes, _ = shop
    await seed(shop)
    only = await q.load(S.address.city).order_by(S.id)
    assert only[0].address == address(classes)  # a group loads whole
    with pytest.raises(orm.NotLoaded):
        only[0].id  # noqa: B018
    only = await q.load(S.address).order_by(S.id)
    assert only[0].address == address(classes)
    assert only[3].address is None


async def test_the_scenario_two_levels_deep_with_a_nullable_middle_level(shop):
    q, S, classes, _ = shop
    await seed(shop)
    await q.filter(S.address.geo.lat == 9.0).update(address={"geo": {"lat": 10.0}})
    assert (await q.get(S.id == 2)).address.geo == classes["Geo"](lat=10.0, lng=2.5)
    await q.filter(S.id == 3).update(address={"geo": classes["Geo"](lat=0.5)})
    assert (await q.get(S.id == 3)).address.geo == classes["Geo"](lat=0.5)
    await q.filter(S.id == 3).update(address={"geo": None})
    assert (await q.get(S.id == 3)).address.geo is None


async def test_value_classes_are_immutable_equal_and_picklable(world):
    db, classes, _ = world
    a = address(classes)
    assert a == address(classes) and hash(a) == hash(address(classes))
    with pytest.raises(AttributeError):
        a.city = "x"
    assert copy.deepcopy(a) == a and copy.copy(a) == a
    assert a.to_dict()["geo"] == {"lat": 1.5, "lng": None}


async def test_a_json_cell_that_breaks_the_type_raises_decode_error_with_the_path(world):
    db, classes, dialect = world
    J = classes["JShop"]
    await J.objects.using(db).insert(id=1, address=address(classes))
    cast = "::jsonb" if dialect == "postgres" else ""
    ok = '"floor":1,"open":true,"opened":"2024-01-01T00:00:00.000000Z"'
    bad = [
        (f'{{"city":"x","big":"12",{ok},"geo":{{"lat":"no"}}}}', "address.geo.lat"),
        (f'{{"city":"x","big":"notanumber",{ok}}}', "address.big"),
        ('{"city":"x","floor":1,"big":"1","open":true,"opened":"yesterday"}', "address.opened"),
        (f'{{"big":"1",{ok}}}', "address.city"),
    ]
    for payload, where in bad:
        await db.execute(f"UPDATE jshop SET address = '{payload}'{cast} WHERE id = 1")
        with pytest.raises(DecodeError) as e:
            await J.objects.using(db).all()
        assert where in str(e.value)
        assert e.value.model == "JShop" and e.value.field == "address"


async def test_json_values_keep_decimal_bigint_and_datetime_exact():
    """Value conversion needs no database: BigInt above 2**53 and Decimal go as text, DateTime as UTC text."""
    registry = orm.Registry()
    classes = orm.loads(
        SQLITE
        + """
type Money {
  amount Decimal
  big    BigInt
  at     DateTime
}
model Wallet {
  id    Int   @id
  money Money @storage(json)
}
""",
        registry=registry,
    )
    nt = classes["Wallet"]._meta.nested["money"].nt
    out = nt.to_json({"amount": Decimal("12345678901234567890.123456789"), "big": 2**62, "at": WHEN}, "Wallet.money")
    assert out == {"amount": "12345678901234567890.123456789", "big": str(2**62), "at": "2024-01-02T03:04:05.000000Z"}
    back = nt.from_json(out, "Wallet", "money")
    assert back.amount == Decimal("12345678901234567890.123456789") and back.big == 2**62 and back.at == WHEN
    # the stored text of either storage decodes: PostgreSQL adds fraction digits, SQLite keeps what was written
    assert nt.from_json({**out, "at": "2024-01-02T03:04:05Z"}, "Wallet", "money").at == WHEN
    with pytest.raises(DecodeError, match="amount"):
        nt.from_json({**out, "amount": "abc"}, "Wallet", "money")


EXAMPLE = Path(__file__).resolve().parents[3] / "examples" / "nested"


def test_a_re_prepared_registry_keeps_its_nested_types(monkeypatch):
    monkeypatch.setattr("orm.model._REPREPARE", True)  # what a composition artifact does
    registry = orm.Registry()
    orm.loads(SCHEMA, registry=registry)
    assert [t["name"] for t in registry.ir()["types"]] == ["Geo", "Address"]


@pytest.mark.skipif(
    bool(json.loads(_native.native_artifact())["extensions"]),
    reason="the checked-in models are the default profile's; an extension artifact embeds its passes",
)
def test_example_models_are_current(tmp_path, monkeypatch):
    shutil.copy(EXAMPLE / "schema.prisma", tmp_path / "schema.prisma")
    monkeypatch.chdir(tmp_path)
    assert orm._native.cli(["generate", "python", "-o", "models.py"]) == 0
    for name in ("models.py", "models.pyi"):
        assert (tmp_path / name).read_text() == (EXAMPLE / name).read_text(), f"run `python -m orm generate` in {EXAMPLE}"


def test_generated_value_classes(monkeypatch):
    monkeypatch.syspath_prepend(str(EXAMPLE.parent))
    models = importlib.import_module("nested.models")
    address = models.Address(city="Oslo", big=1, opened=WHEN, geo=models.Geo(lat=1.5))
    assert address.floor == 7 and address.geo.lat == 1.5 and address.geo.lng is None
    assert models.Address.__module__ == "nested.models"
    assert pickle.loads(pickle.dumps(address)) == address
    assert models.Shop.address.geo.lat._select_name() == "address_geo_lat"
    assert models.AddressInput is dict and models.AddressPatch is dict


def test_nested_types_in_a_non_empty_registry():
    registry = orm.Registry()
    orm.loads(SQLITE + "model Plain { id Int @id }", registry=registry)
    classes = orm.loads(SQLITE + SCHEMA, registry=registry)
    assert classes["CShop"].address.city._select_name() == "address_city"
