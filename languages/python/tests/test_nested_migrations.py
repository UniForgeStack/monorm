"""Nested types on columns storage migrate as plain columns plus the generated null check."""

import pytest

import orm
from orm import Registry, SchemaError
from orm.migrations import Migrations

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
