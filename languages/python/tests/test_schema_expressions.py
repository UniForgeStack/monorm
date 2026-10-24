"""Native checks and index filters: the warning path, and a bad row rejected by the database."""

import warnings
from contextlib import asynccontextmanager
from datetime import datetime, timezone
from types import SimpleNamespace

import pytest

import orm
from conftest import DATABASE_URL
from orm import _native

PROVIDERS = {
    "postgres": ("postgresql", DATABASE_URL),
    "sqlite": ("sqlite", "sqlite://:memory:"),
}


def source(provider, table, check, extra=""):
    return (
        f'datasource db {{\n  provider = "{provider}"\n}}\n'
        f'model Stock {{\n  id Int @id @default(autoincrement())\n  qty Int\n  reserved Int\n  name String\n{extra}'
        f'  @@map("{table}")\n  @@check({check})\n}}\n'
    )


@asynccontextmanager
async def opened(name, check, extra=""):
    provider, url = PROVIDERS[name]
    registry = orm.Registry()
    classes = orm.loads(source(provider, f"pl12_stock_{name}", check, extra), registry=registry)
    try:
        db = await orm.connect(url, registry=registry, default=False)
    except orm.DatabaseError as e:
        pytest.skip(f"{name} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    try:
        yield SimpleNamespace(db=db, model=classes["Stock"])
    finally:
        await db.drop_tables()
        await db.close()


@pytest.fixture(params=PROVIDERS)
async def stock(request):
    async with opened(request.param, "reserved <= qty & qty >= 0") as opened_stock:
        yield opened_stock


async def test_a_native_check_rejects_a_bad_row(stock):
    objects = stock.model.objects.using(stock.db)
    await objects.insert(qty=5, reserved=2, name="ok")
    for bad in ({"qty": 1, "reserved": 2}, {"qty": -1, "reserved": -2}):
        with pytest.raises(orm.IntegrityError):
            await objects.insert(name="bad", **bad)
    assert await objects.count() == 1


UTC = timezone.utc
ROW = {"qty": 1, "reserved": 0, "name": "n"}
# check, a row it accepts, a row it rejects (None: it rejects none). Each once gave a different answer on SQLite.
SAME_ON_BOTH = {
    "datetime literal": ('seen > "2024-01-01T00:00:00Z"', {"seen": datetime(2024, 1, 1, 12, tzinfo=UTC)}, {"seen": datetime(2023, 12, 31, 12, tzinfo=UTC)}),
    "empty suffix": ('name.endswith("")', {"name": "abc"}, None),
    "escaped ilike": (r'name.ilike("50\\%")', {"name": "50%"}, {"name": "500"}),
}


@pytest.mark.parametrize("case", SAME_ON_BOTH)
@pytest.mark.parametrize("provider", PROVIDERS)
async def test_a_native_check_gives_the_same_answer_on_both_dialects(provider, case):
    check, good, bad = SAME_ON_BOTH[case]
    async with opened(provider, check, "  seen DateTime?\n") as stock:
        objects = stock.model.objects.using(stock.db)
        await objects.insert(**{**ROW, **good})
        if bad:
            with pytest.raises(orm.IntegrityError):
                await objects.insert(**{**ROW, **bad})
        assert await objects.count() == 1


def test_a_plain_string_check_warns_with_file_line_column_and_names_raw(tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text(source("sqlite", "t", '"qty > 0"'))
    with pytest.warns(orm.SchemaWarning, match=r"schema\.prisma:10:11: warning: .*raw\(") as caught:
        orm.load(schema, registry=orm.Registry())
    assert len(caught) == 1


def test_generate_python_raises_the_schema_warnings(tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text(source("sqlite", "t", '"qty > 0"'))
    with pytest.warns(orm.SchemaWarning, match=r"schema\.prisma:10:11: warning: .*raw\(") as caught:
        _native.generate_python(str(schema))
    assert len(caught) == 1


def test_a_native_or_raw_check_does_not_warn():
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        orm.loads(source("sqlite", "t1", "qty > 0"), registry=orm.Registry())
        orm.loads(source("sqlite", "t2", 'raw("qty > 0")'), registry=orm.Registry())
