"""Nested types on columns storage migrate as plain columns plus the generated null check."""

import contextlib
import datetime
import json
import os

import pytest

import orm
from orm import Registry, SchemaError
from orm.migrations import Migrations, Migrator

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

V1 = """
type Geo {
  lat Float
  lng Float?
}

type Address {
  city String
  geo   Geo
}

model Shop {
  id      Int      @id
  address Address?
}
"""

CHECK = (
    'CHECK (("address_city" IS NULL AND "address_geo_lat" IS NULL AND "address_geo_lng" IS NULL)'
    ' OR ("address_city" IS NOT NULL AND "address_geo_lat" IS NOT NULL))'
)


def models(source: str) -> Registry:
    reg = Registry()
    orm.loads(source, registry=reg)
    return reg


def up(tmp_path, source: str) -> list[str]:
    return [s.sql for s in Migrations(tmp_path, models(source)).plan().up]


def test_first_migration_creates_the_leaf_columns_and_the_null_check(tmp_path):
    migs = Migrations(tmp_path, models(V1))
    m = migs.make()
    assert m is not None
    for column in (
        '"address_city" varchar',
        '"address_geo_lat" double precision',
        '"address_geo_lng" double precision',
    ):
        assert column in m.up_sql
    assert CHECK in m.up_sql
    assert '"address_city" varchar NOT NULL' not in m.up_sql  # a null field leaves every column null
    assert migs.make() is None


def test_adding_a_subfield_adds_a_column_and_extends_the_check(tmp_path):
    Migrations(tmp_path, models(V1)).make()
    sql = up(tmp_path, V1.replace("  city String\n", "  city String\n  zip  String\n"))
    assert 'ALTER TABLE "shop" ADD COLUMN "address_zip" varchar' in sql
    assert any("DROP CONSTRAINT" in s for s in sql) and any(
        "ADD CONSTRAINT" in s and '"address_zip" IS NOT NULL' in s for s in sql
    )
    # a required subfield on a nullable field is added as nullable: old null rows have no value
    assert not any('"address_zip" varchar NOT NULL' in s for s in sql)


def test_removing_a_subfield_drops_its_column(tmp_path):
    Migrations(tmp_path, models(V1)).make()
    sql = up(tmp_path, V1.replace("  lng Float?\n", ""))
    assert 'ALTER TABLE "shop" DROP COLUMN "address_geo_lng"' in sql


def test_renamed_from_on_a_subfield_renames_its_column(tmp_path):
    Migrations(tmp_path, models(V1)).make()
    sql = up(tmp_path, V1.replace("  lng Float?", '  longitude Float? @renamed_from("lng")'))
    assert 'ALTER TABLE "shop" RENAME COLUMN "address_geo_lng" TO "address_geo_longitude"' in sql
    assert not any("DROP COLUMN" in s for s in sql)


def test_renamed_from_on_the_field_and_a_parent_subfield_renames_every_column(tmp_path):
    Migrations(tmp_path, models(V1)).make()
    renamed = V1.replace("  geo   Geo", '  place Geo @renamed_from("geo")').replace("  address Address?", '  where   Address? @renamed_from("address")')
    sql = up(tmp_path, renamed)
    for old, new in (("address_city", "where_city"), ("address_geo_lat", "where_place_lat"), ("address_geo_lng", "where_place_lng")):
        assert f'ALTER TABLE "shop" RENAME COLUMN "{old}" TO "{new}"' in sql
    assert not any("DROP COLUMN" in s or "ADD COLUMN" in s for s in sql)


def test_renamed_from_with_a_new_prefix_renames_the_default_named_columns(tmp_path):
    Migrations(tmp_path, models(V1)).make()
    prefixed = V1.replace("  address Address?", '  address Address? @storage(columns, prefix: "a_")')
    sql = up(tmp_path, prefixed)
    assert 'ALTER TABLE "shop" RENAME COLUMN "address_city" TO "a_city"' not in sql  # no hint: a drop and an add
    hinted = V1.replace("  address Address?", '  addr Address? @storage(columns, prefix: "a_") @renamed_from("address")')
    sql = up(tmp_path, hinted)
    assert 'ALTER TABLE "shop" RENAME COLUMN "address_city" TO "a_city"' in sql
    assert 'ALTER TABLE "shop" RENAME COLUMN "address_geo_lat" TO "a_geo_lat"' in sql


def test_a_second_make_after_a_nested_change_finds_nothing(tmp_path):
    Migrations(tmp_path, models(V1)).make()
    changed = V1.replace("  city String\n", "  city String\n  zip  String\n")
    assert Migrations(tmp_path, models(changed)).make() is not None
    assert Migrations(tmp_path, models(changed)).make() is None


def test_a_null_field_without_a_required_subfield_is_a_schema_error():
    with pytest.raises(SchemaError, match="no required subfield"):
        models("type T {\n  a Int?\n}\n\nmodel M {\n  id Int @id\n  t T?\n}\n")


def test_a_second_make_after_a_removed_subfield_finds_nothing(tmp_path):
    Migrations(tmp_path, models(V1)).make()
    changed = V1.replace("  lng Float?\n", "")
    assert Migrations(tmp_path, models(changed)).make() is not None
    assert Migrations(tmp_path, models(changed)).make() is None


def test_a_second_make_after_a_check_change_finds_nothing(tmp_path):
    Migrations(tmp_path, models(V1)).make()
    changed = V1.replace("  lat Float\n", "  lat Float?\n")  # only the generated check changes
    assert Migrations(tmp_path, models(changed)).make() is not None
    assert Migrations(tmp_path, models(changed)).make() is None


def test_a_second_make_on_sqlite_finds_nothing(tmp_path):
    source = 'datasource db { provider = "sqlite" }\n' + V1
    assert Migrations(tmp_path, models(source)).make() is not None
    assert Migrations(tmp_path, models(source)).make() is None


def test_adding_a_defaulted_leaf_to_a_nullable_group_fills_only_present_rows(tmp_path):
    Migrations(tmp_path, models(V1)).make()
    sql = up(tmp_path, V1.replace("  city String\n", '  city String\n  zip  String @default("0")\n'))
    add = next(i for i, s in enumerate(sql) if 'ADD COLUMN "address_zip"' in s)
    assert "DEFAULT" not in sql[add]  # a default would fill the rows whose group is null
    fill = next(i for i, s in enumerate(sql) if s.startswith('UPDATE "shop" SET "address_zip" = '))
    assert "'0'" in sql[fill] and '"address_city" IS NOT NULL' in sql[fill]
    check = next(i for i, s in enumerate(sql) if "ADD CONSTRAINT" in s and '"address_zip" IS NOT NULL' in s)
    assert add < fill < check


def test_a_snapshot_without_root_required_gives_no_migration(tmp_path):
    source = V1.replace("  address Address?\n", "  address Address\n")
    migs = Migrations(tmp_path, models(source))
    assert migs.make() is not None
    assert "root_required" not in next(tmp_path.glob("*/snapshot.json")).read_text()  # the snapshot holds database tables only
    assert up(tmp_path, source) == []


JSON_V1 = """
type Geo {
  lat Float
}

type Address {
  city String
  geo  Geo
}

model Shop {
  id      Int     @id
  address Address @storage(json)
}
"""
JSON_V2 = JSON_V1.replace("  lat Float\n", "  lat Float\n  alt Float @default(0)\n").replace(
    "  city String\n", '  city String\n  zip  String @default("0")\n'
)
SQLITE = 'datasource db { provider = "sqlite" }\n'


def test_a_new_required_json_subfield_with_a_default_fills_the_rows_that_lack_it(tmp_path):
    Migrations(tmp_path, models(JSON_V1)).make()
    sql = up(tmp_path, JSON_V2)
    assert not any("ADD COLUMN" in s for s in sql)  # the column is the same; only its rows change
    fills = [s for s in sql if s.startswith('UPDATE "shop"')]
    assert len(fills) == 2
    zip_fill = next(s for s in fills if "'zip'" in s)
    assert "jsonb_set(\"address\", ARRAY['zip'], '\"0\"'::jsonb)" in zip_fill
    assert "jsonb_typeof(\"address\") = 'object' AND COALESCE(jsonb_typeof((\"address\") -> 'zip'), 'null') = 'null'" in zip_fill
    alt_fill = next(s for s in fills if "'alt'" in s)
    assert "ARRAY['geo', 'alt']" in alt_fill and "\"address\" #> ARRAY['geo']" in alt_fill


def test_a_new_required_json_subfield_fills_on_sqlite(tmp_path):
    Migrations(tmp_path, models(SQLITE + JSON_V1)).make()
    sql = up(tmp_path, SQLITE + JSON_V2)
    assert len(sql) == 1
    assert """json_set("address", '$."geo"."alt"', json('0'))""" in sql[0]
    assert """json_type("address", '$."geo"') = 'object' AND COALESCE(json_type("address", '$."geo"."alt"'), 'null') = 'null'""" in sql[0]


def test_a_new_required_json_subfield_without_a_default_is_an_error(tmp_path):
    Migrations(tmp_path, models(JSON_V1)).make()
    with pytest.raises(SchemaError, match=r"shop\.address\.zip: a required subfield added to a json field needs a @default"):
        up(tmp_path, JSON_V1.replace("  city String\n", "  city String\n  zip  String\n"))


def test_a_new_nullable_json_subfield_changes_no_rows(tmp_path):
    Migrations(tmp_path, models(JSON_V1)).make()
    assert up(tmp_path, JSON_V1.replace("  city String\n", "  city String\n  zip  String?\n")) == []


def test_a_new_required_subfield_in_a_json_list_is_an_error(tmp_path):
    source = JSON_V1.replace("Address @storage", "Address[] @storage")
    Migrations(tmp_path, models(source)).make()
    with pytest.raises(SchemaError, match="added inside a list can't be filled by a migration"):
        up(tmp_path, source.replace("  city String\n", '  city String\n  zip  String @default("0")\n'))


def test_a_second_make_after_a_json_backfill_finds_nothing(tmp_path):
    Migrations(tmp_path, models(JSON_V1)).make()
    assert Migrations(tmp_path, models(JSON_V2)).make() is not None
    assert Migrations(tmp_path, models(JSON_V2)).make() is None
    assert '"alt"' in sorted(tmp_path.glob("*/snapshot.json"))[-1].read_text()


DIALECTS = ["postgres", "sqlite"]
DROP = "DROP TABLE IF EXISTS shop; DROP TABLE IF EXISTS orm_migrations"


@contextlib.asynccontextmanager
async def database(dialect, registry):
    url = "sqlite://:memory:" if dialect == "sqlite" else PG_URL
    try:
        db = await orm.connect(url, registry=registry, default=False, max_connections=2)
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {PG_URL}: {e}")
    try:
        await db.execute(DROP)
        yield db
    finally:
        await db.execute(DROP)
        await db.close()


async def migrate(db, tmp_path, source):
    migs = Migrations(tmp_path, models(source))
    assert migs.make() is not None
    await Migrator(db, migs).upgrade()


async def insert_addresses(db, rows):
    values = ", ".join(f"({i}, '{json.dumps(value)}')" for i, value in rows)
    await db.execute(f"INSERT INTO shop (id, address) VALUES {values}")


async def addresses(db):
    return {
        r["id"]: json.loads(r["address"]) if isinstance(r["address"], str) else r["address"]
        for r in await db.fetch("SELECT id, address FROM shop")
    }


@pytest.mark.parametrize("dialect", DIALECTS)
async def test_applying_the_backfill_keeps_present_keys_and_fills_missing_ones(tmp_path, dialect):
    prefix = SQLITE if dialect == "sqlite" else ""
    async with database(dialect, models(prefix + JSON_V1)) as db:
        await migrate(db, tmp_path, prefix + JSON_V1)
        await insert_addresses(
            db,
            [
                (1, {"city": "A", "geo": {"lat": 1}}),
                (2, {"city": "B", "zip": "9", "geo": {"lat": 2, "alt": 5}}),
                (3, {"city": "C"}),
            ],
        )
        await migrate(db, tmp_path, prefix + JSON_V2)
        assert await addresses(db) == {
            1: {"city": "A", "zip": "0", "geo": {"lat": 1, "alt": 0}},
            2: {"city": "B", "zip": "9", "geo": {"lat": 2, "alt": 5}},
            3: {"city": "C", "zip": "0"},  # no geo object: nothing to put alt in
        }


@pytest.mark.parametrize("dialect", DIALECTS)
async def test_applying_the_backfill_fills_a_key_that_holds_a_json_null(tmp_path, dialect):
    prefix = SQLITE if dialect == "sqlite" else ""
    nullable = JSON_V1.replace("  city String\n", "  city String\n  zip  String?\n")
    required = JSON_V1.replace("  city String\n", '  city String\n  zip  String @default("z")\n')
    async with database(dialect, models(prefix + nullable)) as db:
        await migrate(db, tmp_path, prefix + nullable)
        geo = {"lat": 1}
        await insert_addresses(
            db,
            [(1, {"city": "A", "geo": geo}), (2, {"city": "B", "zip": None, "geo": geo}), (3, {"city": "C", "zip": "9", "geo": geo})],
        )
        await migrate(db, tmp_path, prefix + required)
        got = await addresses(db)
        assert [got[i]["zip"] for i in (1, 2, 3)] == ["z", "z", "9"]


@pytest.mark.parametrize("dialect", DIALECTS)
async def test_applying_the_backfill_skips_a_null_or_missing_middle_level(tmp_path, dialect):
    prefix = SQLITE if dialect == "sqlite" else ""
    optional = JSON_V1.replace("  geo  Geo\n", "  geo  Geo?\n")
    deeper = optional.replace("  lat Float\n", "  lat Float\n  alt Float @default(0)\n")
    async with database(dialect, models(prefix + optional)) as db:
        await migrate(db, tmp_path, prefix + optional)
        await insert_addresses(
            db,
            [(1, {"city": "A", "geo": {"lat": 1}}), (2, {"city": "B", "geo": None}), (3, {"city": "C"})],
        )
        await migrate(db, tmp_path, prefix + deeper)
        assert await addresses(db) == {
            1: {"city": "A", "geo": {"lat": 1, "alt": 0}},
            2: {"city": "B", "geo": None},
            3: {"city": "C"},
        }


COLUMNS_V1 = """
type Address {
  city String
  zip  String @default("z")
}

model Shop {
  id      Int      @id
  address Address?
}
"""


@pytest.mark.parametrize("dialect", DIALECTS)
async def test_a_downgrade_refills_a_required_leaf_it_re_adds(tmp_path, dialect):
    prefix = SQLITE if dialect == "sqlite" else ""
    source, removed = prefix + COLUMNS_V1, prefix + COLUMNS_V1.replace('  zip  String @default("z")\n', "")
    async with database(dialect, models(source)) as db:
        await migrate(db, tmp_path, source)
        await db.execute("INSERT INTO shop (id, address_city, address_zip) VALUES (1, 'A', 'q'), (2, NULL, NULL)")
        await migrate(db, tmp_path, removed)
        await Migrator(db, Migrations(tmp_path, models(removed))).downgrade()
        rows = {r["id"]: (r["address_city"], r["address_zip"]) for r in await db.fetch("SELECT * FROM shop")}
        assert rows == {1: ("A", "z"), 2: (None, None)}


@pytest.mark.parametrize("dialect", DIALECTS)
async def test_applying_a_renamed_subfield_keeps_the_data(tmp_path, dialect):
    prefix = SQLITE if dialect == "sqlite" else ""
    source = prefix + V1
    renamed = prefix + V1.replace("  lng Float?", '  longitude Float? @renamed_from("lng")')
    async with database(dialect, models(source)) as db:
        await migrate(db, tmp_path, source)
        await db.execute(
            "INSERT INTO shop (id, address_city, address_geo_lat, address_geo_lng) VALUES (1, 'A', 1.5, 2.5), (2, NULL, NULL, NULL)"
        )
        await migrate(db, tmp_path, renamed)
        rows = {r["id"]: (r["address_city"], r["address_geo_longitude"]) for r in await db.fetch("SELECT * FROM shop")}
        assert rows == {1: ("A", 2.5), 2: (None, None)}


STORAGE = """
type Geo {
  lat Float
  lng Float?
}

type Everything {
  s     String
  i     Int
  big   BigInt
  f     Float
  b     Boolean
%(dec)s  at    DateTime
  day   DateTime @db.Date
  uid   String @db.Uuid
  extra Json?
  geo   Geo?
}

model Shop {
  id   Int         @id
  data Everything? %(storage)s
}
"""
ALL_LEAVES = ["s", "i", "big", "f", "b", "dec", "at", "day", "uid", "extra", "geo_lat", "geo_lng"]
UID = "6f1f4e0a-3c2b-4f6e-9d57-1a2b3c4d5e6f"
EXPECTED = {
    1: {
        "s": "x", "i": 7, "big": "9007199254740993", "f": 1.5, "b": True, "dec": "1234.56",
        "at": "2026-01-02T03:04:05.123456Z", "day": "2026-01-03", "uid": UID,
        "extra": {"k": [1, 2]}, "geo": {"lat": 1.5, "lng": 2.5},
    },
    2: {
        "s": "y", "i": -1, "big": "-9007199254740993", "f": 0.25, "b": False, "dec": "0.10",
        "at": "2026-12-31T23:59:59.000000Z", "day": "2026-12-31", "uid": UID,
        "extra": None, "geo": None,
    },
    3: None,
}


def storage(dialect, where):
    prefix = SQLITE if dialect == "sqlite" else ""
    dec = "" if dialect == "sqlite" else "  dec   Decimal\n"  # SQLite has no exact decimal
    return prefix + STORAGE % {"dec": dec, "storage": "@storage(json)" if where == "json" else ""}


def leaves(dialect):
    return [leaf for leaf in ALL_LEAVES if dialect != "sqlite" or leaf != "dec"]


def expected(dialect):
    return {i: None if doc is None else {k: v for k, v in doc.items() if dialect != "sqlite" or k != "dec"} for i, doc in EXPECTED.items()}


def moment(dialect, text):
    return text if dialect == "sqlite" else text + "+00"


async def insert_columns(db, dialect):
    dec = "" if dialect == "sqlite" else "1234.56, "
    dec2 = "" if dialect == "sqlite" else "0.10, "
    rows = {
        1: "'x', 7, 9007199254740993, 1.5, TRUE, %s'%s', '2026-01-03', '%s', '{\"k\":[1,2]}', 1.5, 2.5"
        % (dec, moment(dialect, "2026-01-02 03:04:05.123456"), UID),
        2: "'y', -1, -9007199254740993, 0.25, FALSE, %s'%s', '2026-12-31', '%s', NULL, NULL, NULL"
        % (dec2, moment(dialect, "2026-12-31 23:59:59"), UID),
        3: ", ".join(["NULL"] * len(leaves(dialect))),
    }
    cols = ", ".join(f"data_{leaf}" for leaf in leaves(dialect))
    await db.execute(f"INSERT INTO shop (id, {cols}) VALUES " + ", ".join(f"({i}, {v})" for i, v in rows.items()))


def plain(value):
    """A value as a test compares it; a DateTime without a fraction and with a zero one are the same instant."""
    if isinstance(value, str) and value[:1] in "{[":
        return json.loads(value)
    if hasattr(value, "utcoffset") and value.utcoffset() is not None:
        return value.astimezone(datetime.timezone.utc).strftime("%Y-%m-%d %H:%M:%S.%f").removesuffix(".000000")
    if isinstance(value, str) and value.endswith(".000000"):
        return value.removesuffix(".000000")
    return value if value is None or isinstance(value, (bool, int, float, dict, list)) else str(value)


async def column_rows(db, dialect):
    rows = await db.fetch("SELECT * FROM shop")
    return {r["id"]: {leaf: plain(r[f"data_{leaf}"]) for leaf in leaves(dialect)} for r in rows}


async def json_rows(db):
    return {r["id"]: plain(r["data"]) for r in await db.fetch("SELECT id, data FROM shop")}


def as_columns(doc, dialect):
    """The leaf columns that hold the JSON value `doc`: a DateTime column has no `T` and no `Z`."""
    if doc is None:
        return {leaf: None for leaf in leaves(dialect)}
    geo = doc["geo"] or {"lat": None, "lng": None}
    row = {k: v for k, v in doc.items() if k != "geo"} | {"geo_lat": geo["lat"], "geo_lng": geo["lng"]}
    row["at"] = row["at"].replace("T", " ").rstrip("Z").removesuffix(".000000")
    row["big"] = int(row["big"])
    return row


@pytest.mark.parametrize("dialect", DIALECTS)
async def test_a_columns_to_json_change_copies_every_leaf_type_and_down_reverses_it(tmp_path, dialect):
    async with database(dialect, models(storage(dialect, "columns"))) as db:
        await migrate(db, tmp_path, storage(dialect, "columns"))
        await insert_columns(db, dialect)
        before = await column_rows(db, dialect)
        await migrate(db, tmp_path, storage(dialect, "json"))
        assert await json_rows(db) == expected(dialect)
        await Migrator(db, Migrations(tmp_path, models(storage(dialect, "json")))).downgrade()
        assert await column_rows(db, dialect) == before


@pytest.mark.parametrize("dialect", DIALECTS)
async def test_a_json_to_columns_change_copies_every_leaf_type_and_down_reverses_it(tmp_path, dialect):
    docs = expected(dialect)
    async with database(dialect, models(storage(dialect, "json"))) as db:
        await migrate(db, tmp_path, storage(dialect, "json"))
        values = ", ".join(f"({i}, " + ("NULL" if d is None else "'" + json.dumps(d) + "'") + ")" for i, d in docs.items())
        await db.execute(f"INSERT INTO shop (id, data) VALUES {values}")
        await migrate(db, tmp_path, storage(dialect, "columns"))
        assert await column_rows(db, dialect) == {i: as_columns(d, dialect) for i, d in docs.items()}
        await Migrator(db, Migrations(tmp_path, models(storage(dialect, "columns")))).downgrade()
        assert await json_rows(db) == docs


@pytest.mark.parametrize("dialect", DIALECTS)
def test_a_second_make_after_a_storage_change_finds_nothing(tmp_path, dialect):
    for where in ("columns", "json", "columns"):
        assert Migrations(tmp_path, models(storage(dialect, where))).make() is not None
        assert Migrations(tmp_path, models(storage(dialect, where))).make() is None
