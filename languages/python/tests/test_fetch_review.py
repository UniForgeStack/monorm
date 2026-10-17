"""`db.fetch` and error details: stale plans, int2/float4 arrays, parameter types, SQLSTATE of NOT NULL and CHECK."""

import datetime

import pytest
import orm
from conftest import DATABASE_URL


@pytest.fixture
async def lite():
    registry = orm.Registry()
    orm.loads('datasource db {\n  provider = "sqlite"\n}\nmodel Person {\n  id Int @id\n}\n', registry=registry)
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    yield db
    await db.close()


@pytest.fixture
async def single():
    """A Postgres pool with one connection, so every call shares one statement cache."""
    try:
        db = await orm.connect(DATABASE_URL, registry=orm.Registry(), default=False, max_connections=1)
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable: {e}")
    await db.execute("DROP TABLE IF EXISTS fx_stale")
    yield db
    await db.execute("DROP TABLE IF EXISTS fx_stale")
    await db.close()


async def test_postgres_a_fetch_after_a_schema_change_prepares_again(single):
    db = single
    await db.execute("CREATE TABLE fx_stale (id int)")
    await db.execute("INSERT INTO fx_stale VALUES (1)")
    sql = "SELECT * FROM fx_stale WHERE id = $1"
    assert await db.fetch(sql, 1) == [{"id": 1}]
    await db.execute("ALTER TABLE fx_stale ADD COLUMN extra int DEFAULT 5")
    with pytest.raises(orm.DatabaseError) as e:
        await db.fetch(sql, 1)
    assert e.value.sqlstate == "0A000"
    assert await db.fetch(sql, 1) == [{"id": 1, "extra": 5}]
    assert await db.fetch(sql, 1) == [{"id": 1, "extra": 5}]


async def test_postgres_fetch_reads_int2_and_float4_arrays(single):
    [row] = await single.fetch("SELECT ARRAY[1::int2, NULL] AS a, ARRAY[1.5::float4] AS b")
    assert row == {"a": [1, None], "b": [1.5]}


async def test_postgres_fetch_parameters_of_bool_float_date_and_list(single):
    day = datetime.date(2026, 1, 2)
    [row] = await single.fetch(
        "SELECT $1::bool AS b, $2::float8 AS f, $3::date AS d, $4::jsonb AS l", True, 1.5, day, [1, "x"]
    )
    assert row == {"b": True, "f": 1.5, "d": day, "l": [1, "x"]}


async def test_sqlite_fetch_parameters_of_bool_float_date_and_list(lite):
    day = datetime.date(2026, 1, 2)
    [row] = await lite.fetch("SELECT ? AS b, ? AS f, ? AS d, ? AS l", True, 1.5, day, [1, "x"])
    assert row == {"b": 1, "f": 1.5, "d": "2026-01-02", "l": '[1,"x"]'}


async def test_sqlite_not_null_check_and_unique_have_a_sqlstate(lite):
    await lite.execute(
        "CREATE TABLE fx (id integer PRIMARY KEY, n text NOT NULL, c integer CHECK (c > 0), u text UNIQUE)"
    )
    with pytest.raises(orm.IntegrityError) as e:
        await lite.execute("INSERT INTO fx (id, n) VALUES (1, NULL)")
    assert e.value.sqlstate == "23502"
    with pytest.raises(orm.IntegrityError) as e:
        await lite.execute("INSERT INTO fx (id, n, c) VALUES (1, 'a', 0)")
    assert e.value.sqlstate == "23514"
    await lite.execute("INSERT INTO fx (id, n, u) VALUES (1, 'a', 'x')")
    with pytest.raises(orm.IntegrityError) as e:
        await lite.execute("INSERT INTO fx (id, n, u) VALUES (2, 'a', 'x')")
    assert (e.value.sqlstate, e.value.constraint) == ("23505", None)


async def test_postgres_not_null_and_check_have_a_sqlstate(single):
    await single.execute("DROP TABLE IF EXISTS fx_stale")
    await single.execute("CREATE TABLE fx_stale (id int NOT NULL, c int CONSTRAINT fx_c CHECK (c > 0))")
    with pytest.raises(orm.IntegrityError) as e:
        await single.execute("INSERT INTO fx_stale (id) VALUES (NULL)")
    assert e.value.sqlstate == "23502"
    with pytest.raises(orm.IntegrityError) as e:
        await single.execute("INSERT INTO fx_stale (id, c) VALUES (1, 0)")
    assert (e.value.sqlstate, e.value.constraint) == ("23514", "fx_c")
