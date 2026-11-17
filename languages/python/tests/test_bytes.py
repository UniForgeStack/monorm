"""The `Bytes` scalar: values, operators and refusals on PostgreSQL and SQLite."""

import os

import pytest

import monorm
from monorm import QueryError

URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
SCHEMA = 'model Blob {\n  id Int @id\n  data Bytes?\n  tag String @default("t")\n  @@map("bytes_blob")\n}'
EVERY_BYTE = bytes(range(256))


@pytest.fixture(params=["sqlite", "postgres"])
async def blob(request):
    registry = monorm.Registry()
    source = ('datasource db {\n  provider = "sqlite"\n}\n' if request.param == "sqlite" else "") + SCHEMA
    Blob = monorm.loads(source, registry=registry)["Blob"]
    try:
        db = await monorm.connect("sqlite://:memory:" if request.param == "sqlite" else URL, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    yield Blob, Blob.objects.using(db), db
    await db.drop_tables()
    await db.close()


async def test_bytes_round_trip(blob):
    Blob, qs, _ = blob
    rows = [b"", b"\x00", EVERY_BYTE, b"caf\xc3\xa9", None]
    await qs.insert_many([{"id": i, "data": v} for i, v in enumerate(rows, 1)])
    got = {r.id: r.data for r in await qs.order_by(Blob.id)}
    assert got == dict(enumerate(rows, 1))
    assert all(type(v) is bytes for v in got.values() if v is not None)


async def test_bytearray_and_memoryview_are_accepted_and_read_back_as_bytes(blob):
    Blob, qs, _ = blob
    await qs.insert_many([{"id": 1, "data": bytearray(b"ab")}, {"id": 2, "data": memoryview(b"cd")}])
    assert [r.data for r in await qs.order_by(Blob.id)] == [b"ab", b"cd"]


async def test_a_value_that_is_not_bytes_is_a_type_error(blob):
    Blob, qs, _ = blob
    for bad in ("text", 5, [1, 2], {"a": 1}):
        with pytest.raises(TypeError, match="expected bytes"):
            await qs.insert_many([{"id": 1, "data": bad}])


async def test_equality_in_and_null_checks(blob):
    Blob, qs, _ = blob
    await qs.insert_many([{"id": 1, "data": b"a"}, {"id": 2, "data": b"b"}, {"id": 3, "data": b"\x00"}, {"id": 4, "data": None}])

    async def ids(cond):
        return sorted(await qs.filter(cond).select(Blob.id).scalars())

    d = Blob.data
    assert await ids(d == b"a") == [1]
    assert await ids((d != b"a").and_not_null()) == [2, 3]
    assert await ids(d.in_([b"a", b"\x00"])) == [1, 3]
    assert await ids(d.not_in([b"a", b"\x00"]).and_not_null()) == [2]
    assert await ids(d.is_null()) == [4]
    assert await ids(d.is_not_null()) == [1, 2, 3]


async def test_bulk_update_and_update_write_bytes(blob):
    Blob, qs, _ = blob
    await qs.insert_many([{"id": i, "data": b"x"} for i in (1, 2, 3)])
    assert await qs.filter(Blob.id.in_([1, 2])).update(data=b"\x01\x02") == 2
    assert [r.data for r in await qs.order_by(Blob.id)] == [b"\x01\x02", b"\x01\x02", b"x"]
    row = await qs.get(Blob.id == 3)
    await row.update(data=EVERY_BYTE)
    assert (await qs.get(Blob.id == 3)).data == EVERY_BYTE


async def test_order_range_like_and_aggregates_are_refused(blob):
    Blob, qs, _ = blob
    d = Blob.data
    await qs.insert_many([{"id": 1, "data": b"a"}, {"id": 2, "data": None}])
    for cond in (d < b"a", d >= b"a", d.like("a%"), d.startswith("a")):
        with pytest.raises(QueryError, match="Bytes"):
            await qs.filter(cond).select(Blob.id).scalars()
    with pytest.raises(QueryError, match="Bytes"):
        await qs.order_by(d)
    for fn in (monorm.func.max, monorm.func.min, monorm.func.sum, monorm.func.avg):
        with pytest.raises(QueryError, match="Bytes"):
            await qs.select(fn(d)).scalars()
    assert await qs.select(monorm.func.count(d)).scalars() == [1]


async def test_a_string_bytea_field_is_still_a_bytea_column_and_warns():
    registry = monorm.Registry()
    schema = 'datasource db {\n  provider = "postgresql"\n}\nmodel Old {\n  id Int @id\n  raw String? @db.ByteA\n  @@map("bytes_old")\n}'
    with pytest.warns(monorm.SchemaWarning, match="String @db.ByteA` is deprecated"):
        Old = monorm.loads(schema, registry=registry)["Old"]
    try:
        db = await monorm.connect(URL, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    try:
        await db.drop_tables()
        await db.create_tables()
        assert await db._fetch_text("SELECT data_type FROM information_schema.columns WHERE table_name = 'bytes_old' AND column_name = 'raw'") == [("bytea",)]
        assert Old.objects.using(db) is not None
    finally:
        await db.drop_tables()
        await db.close()
