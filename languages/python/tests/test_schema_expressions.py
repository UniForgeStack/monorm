"""Native checks and index filters: the warning path, and a bad row rejected by the database."""

import warnings
from contextlib import asynccontextmanager
from datetime import datetime, timezone
from types import SimpleNamespace

import pytest

import monorm
from conftest import DATABASE_URL
from monorm import _native

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
    registry = monorm.Registry()
    classes = monorm.loads(source(provider, f"pl12_stock_{name}", check, extra), registry=registry)
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
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
        with pytest.raises(monorm.IntegrityError):
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
            with pytest.raises(monorm.IntegrityError):
                await objects.insert(**{**ROW, **bad})
        assert await objects.count() == 1


def test_a_plain_string_check_warns_with_file_line_column_and_names_raw(tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text(source("sqlite", "t", '"qty > 0"'))
    with pytest.warns(monorm.SchemaWarning, match=r"schema\.prisma:10:11: warning: .*raw\(") as caught:
        monorm.load(schema, registry=monorm.Registry())
    assert len(caught) == 1


def test_generate_python_raises_the_schema_warnings(tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text(source("sqlite", "t", '"qty > 0"'))
    with pytest.warns(monorm.SchemaWarning, match=r"schema\.prisma:10:11: warning: .*raw\(") as caught:
        _native.generate_python(str(schema))
    assert len(caught) == 1


def test_a_native_or_raw_check_does_not_warn():
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        monorm.loads(source("sqlite", "t1", "qty > 0"), registry=monorm.Registry())
        monorm.loads(source("sqlite", "t2", 'raw("qty > 0")'), registry=monorm.Registry())


def test_native_adoption_survives_registry_ir_and_unrelated_snapshots(tmp_path):
    from monorm.migrations import Migrations

    def registry(check, extra=""):
        reg = monorm.Registry()
        monorm.loads(source("sqlite", "stock", check, extra), registry=reg)
        return reg

    old = registry('raw("((qty > 0))")')
    initial = Migrations(tmp_path, old).make()
    assert initial is not None
    native = registry("qty > 0")
    assert not Migrations(tmp_path, native).plan()
    assert Migrations(tmp_path, native).plan().down == []
    unrelated = registry("qty > 0", "  note String?\n")
    migration = Migrations(tmp_path, unrelated).make("unrelated")
    assert migration is not None
    assert "CHECK (((qty > 0)))" in migration.up_sql
    assert not Migrations(tmp_path, unrelated).plan()


@pytest.mark.parametrize("provider", PROVIDERS)
async def test_offline_native_conversion_preserves_live_rules_and_later_migrations(provider, tmp_path):
    import subprocess
    import sys

    from monorm.migrations import Migrations, Migrator, pull

    dialect, url = PROVIDERS[provider]
    path = tmp_path / "schema.prisma"
    legacy = (
        f'datasource db {{\n provider = "{dialect}"\n}}\n'
        'model Item {\n id Int @id\n qty Int @map("Qty")\n'
        ' @@map("conversion_item")\n'
        ' @@check(raw("((\\"Qty\\" > 0))"))\n'
        ' @@index([sql("abs( \\"Qty\\" )")], where: raw("(\\"Qty\\" > 0)"))\n}\n'
    )
    path.write_text(legacy)
    migrations = Migrations(tmp_path / "migrations", path)
    first = migrations.make()
    assert first is not None
    connection_registry = monorm.Registry()
    monorm.loads(f'datasource db {{ provider = "{dialect}" }}\n', registry=connection_registry)
    db = await monorm.connect(url, registry=connection_registry, default=False, max_connections=1)
    if provider == "postgres":
        await db.execute("CREATE SCHEMA pl12_conversion; SET search_path = pl12_conversion")
    runner = Migrator(db, migrations)
    try:
        await runner.upgrade()
        await db.execute('INSERT INTO conversion_item (id, "Qty") VALUES (1, 2), (2, 3)')
        converted = subprocess.run(
            [sys.executable, "-m", "monorm", "--schema", str(path), "--dir", str(migrations.directory), "fmt", "--native"],
            capture_output=True, text=True,
        )
        assert converted.returncode == 0, converted.stderr
        assert "@@check((((qty > 0))))" in path.read_text()
        assert not migrations.plan()
        assert migrations.plan().down == []
        assert await runner.upgrade() == []
        with pytest.raises(monorm.IntegrityError):
            await db.execute('INSERT INTO conversion_item (id, "Qty") VALUES (3, -1)')
        # Shadow verification must accept the native form of the existing rules.
        pulled = await pull(db)
        assert pulled.differences == [], (pulled.schema, pulled.differences)
        fresh_registry = monorm.Registry()
        monorm.loads(pulled.schema, registry=fresh_registry)
        fresh = await monorm.connect(url, registry=fresh_registry, default=False, max_connections=1)
        try:
            if provider == "postgres":
                await fresh.execute("CREATE SCHEMA pl12_fresh; SET search_path = pl12_fresh")
            await fresh.create_tables()
            await fresh.execute('INSERT INTO conversion_item (id, "Qty") VALUES (1, 2), (2, 3)')
            with pytest.raises(monorm.IntegrityError):
                await fresh.execute('INSERT INTO conversion_item (id, "Qty") VALUES (3, -1)')
            assert not (await pull(fresh)).differences
        finally:
            if provider == "postgres":
                await fresh.execute("SET search_path = public; DROP SCHEMA pl12_fresh CASCADE")
            await fresh.close()
        pulled_path = tmp_path / "pulled.prisma"
        pulled.write(pulled_path)
        baseline = Migrator(db, Migrations(tmp_path / "baseline", pulled_path))
        assert not (await runner.drift()).steps
        path.write_text(path.read_text().replace(" qty Int", " note String?\n qty Int"))
        unrelated = migrations.make("unrelated")
        assert unrelated is not None
        await runner.upgrade()
        assert not migrations.plan()
        path.write_text(path.read_text().replace("qty > 0", "qty > 1"))
        real = migrations.make("stricter")
        assert real is not None and real.down_sql
        await runner.upgrade()
        with pytest.raises(monorm.IntegrityError):
            await db.execute('INSERT INTO conversion_item (id, "Qty") VALUES (3, 1)')
        await runner.downgrade()
        await db.execute('INSERT INTO conversion_item (id, "Qty") VALUES (3, 1)')
        assert await db._fetch_text("SELECT count(*) FROM conversion_item") == [("3",)]
        await runner.downgrade(target="zero")
        await db.execute('CREATE TABLE conversion_item (id INTEGER PRIMARY KEY, "Qty" INTEGER NOT NULL CHECK ("Qty" > 0))')
        pulled = await pull(db)
        pulled.write(pulled_path)
        await baseline.baseline()
        assert not (await baseline.drift()).steps
    finally:
        if provider == "postgres":
            await db.execute("SET search_path = public; DROP SCHEMA pl12_conversion CASCADE")
        await db.close()
