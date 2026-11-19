"""Value conversion of every column type, on a schema loaded at runtime (`loads()`)
with a registry of its own."""

import os
import re
import uuid
from datetime import date, datetime, timezone
from decimal import Decimal

import pytest

import monorm

SCHEMA = """
model Doc {
  id      String    @id @default(dbgenerated("gen_random_uuid()")) @db.Uuid
  ref     String?   @db.Uuid
  extra   Json?
  data    Json      @default("{\\"a\\": 1}")
  day     DateTime? @db.Date
  at      DateTime?
  big     BigInt?
  small   Int?
  ratio   Float?
  amount  Decimal?  @db.Decimal(20, 6)
  flag    Boolean?
  note    String?
  ids     BigInt[]   @default([])
  days    DateTime[] @default([]) @db.Date
  @@map("value_docs")
}
"""

UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")


@pytest.fixture
async def docs():
    registry = monorm.Registry()
    Doc = monorm.loads(SCHEMA, registry=registry)["Doc"]
    db = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=registry, default=False, max_connections=2)
    try:
        await db.drop_tables()
        await db.create_tables()
        yield Doc, db, Doc.objects.using(db)
    finally:
        await db.drop_tables()
        await db.close()


async def test_every_column_type_round_trips(docs):
    Doc, _, objects = docs
    at = datetime(2026, 1, 2, 3, 4, 5, 678000, tzinfo=timezone.utc)
    doc = await objects.insert(
        data={"list": [1, 2.5, "x", None, {"deep": True}]},
        day=date(2026, 3, 4),
        at=at,
        big=9007199254740993,
        small=-7,
        ratio=0.25,
        amount=Decimal("123456789.123456"),
        flag=True,
        ids=[1, 2, 3],
        days=[date(2026, 1, 1)],
    )
    assert UUID_RE.match(str(doc.id))
    assert doc.data == {"list": [1, 2.5, "x", None, {"deep": True}]}
    assert doc.day == date(2026, 3, 4)
    assert doc.at == at
    assert doc.big == 9007199254740993
    assert doc.small == -7
    assert doc.ratio == 0.25
    assert doc.amount == Decimal("123456789.123456")
    assert doc.flag is True
    assert doc.ids == [1, 2, 3]
    assert doc.days == [date(2026, 1, 1)]
    # the defaults and NULLs
    empty = await objects.insert()
    assert empty.data == {"a": 1}
    assert [empty.day, empty.at, empty.big, empty.amount, empty.flag] == [None] * 5
    # filters by every type
    async def one(q):
        return await objects.filter(q).count()

    assert await one(Doc.id == doc.id) == 1
    assert await one(Doc.big == 9007199254740993) == 1
    assert await one(Doc.at == at) == 1
    assert await one(Doc.day == date(2026, 3, 4)) == 1
    assert await one(Doc.amount > 100) == 1
    assert await one(Doc.ids.has(2)) == 1
    assert await one(Doc.data == {"a": 1}) == 1


@pytest.mark.case("values::fetched_rows_decode_every_type_nulls_in_every_column_through_the_packed_rows")
async def test_fetched_rows_decode_every_type_nulls_in_every_column(docs):
    Doc, db, objects = docs
    at = datetime(2026, 5, 6, 7, 8, 9, 123456, tzinfo=timezone.utc)
    await objects.insert_many(
        [
            {
                "data": {"list": [1, 2.5, "é", None, {"deep": True}], "s": "日本", "__proto__": 7},
                "day": date(2026, 3, 4),
                "at": at,
                "ref": "12345678-1234-1234-1234-123456789abc",
                "extra": {"nested": True},
                "big": -9007199254740993,
                "small": 2147483647,
                "ratio": -0.5,
                "amount": Decimal("-0.000001"),
                "flag": False,
                "note": "naïve ☃ text",
                "ids": [1, 2],
                "days": [date(2026, 1, 1), date(2026, 1, 2)],
            },
            {"note": "", "flag": True, "at": datetime(2026, 1, 1, tzinfo=timezone.utc)},
            {"note": "ASCII text"},
            {},
        ]
    )
    # PostgreSQL allows NULL array elements even though model writes reject them.
    await db.execute("UPDATE value_docs SET ids = ARRAY[1::bigint, NULL, 2::bigint] WHERE note = 'naïve ☃ text'")
    rows = await objects.all()
    assert len(rows) == 4

    def by_note(note):
        return next(r for r in rows if r.note == note)

    none, blank, full = by_note(None), by_note(""), by_note("naïve ☃ text")
    # Cover SQL NULL separately from JSON null and empty arrays.
    for r in (none, blank):
        assert [r.day, r.big, r.small, r.ratio, r.amount, r.ref, r.extra] == [None] * 7
        assert [r.ids, r.days, r.data] == [[], [], {"a": 1}]
        assert UUID_RE.match(str(r.id))
    assert none.note is None
    assert none.at is None
    assert none.flag is None
    assert blank.flag is True
    assert blank.at == datetime(2026, 1, 1, tzinfo=timezone.utc)
    assert by_note("ASCII text").note == "ASCII text"
    assert str(full.ref) == "12345678-1234-1234-1234-123456789abc"
    assert full.extra == {"nested": True}
    assert blank.note == ""
    assert full.data["list"] == [1, 2.5, "é", None, {"deep": True}]
    assert full.data["s"] == "日本"
    assert full.data["__proto__"] == 7
    assert full.day == date(2026, 3, 4)
    assert full.at == at  # the microseconds survive
    assert full.big == -9007199254740993
    assert full.small == 2147483647
    assert full.ratio == -0.5
    assert isinstance(full.amount, Decimal) and full.amount == Decimal("-0.000001")
    assert full.flag is False
    assert full.note == "naïve ☃ text"
    assert full.ids == [1, None, 2]
    assert full.days == [date(2026, 1, 1), date(2026, 1, 2)]


# Python ints are exact, so the number-precision cases of TypeScript (`2 ** 60`, an invalid Date) have no twin.
@pytest.mark.parametrize(
    ("values", "error", "message"),
    [
        ({"big": 1.5}, TypeError, "cannot be interpreted as an integer"),
        ({"big": 2**70}, OverflowError, "too large"),
        ({"big": "1"}, TypeError, "cannot be interpreted as an integer"),
        ({"small": 2**40}, OverflowError, "out of range"),
        ({"ratio": "1"}, TypeError, "must be real number"),
        ({"flag": 1}, TypeError, "not an instance of 'bool'"),
        ({"at": "2026-01-01"}, TypeError, "expected a timezone-aware datetime"),
        ({"at": datetime(2026, 1, 1)}, TypeError, "expected a timezone-aware datetime"),
        ({"id": "not-a-uuid"}, TypeError, "invalid UUID"),
        ({"amount": float("inf")}, TypeError, "finite decimal"),
        ({"ids": 1}, TypeError, "expected a list"),
        ({"data": lambda: 1}, TypeError, "JSON serializable"),
    ],
)
async def test_wrong_value_types_are_type_errors(docs, values, error, message):
    _, _, objects = docs
    with pytest.raises(error, match=message):
        await objects.insert(**values)
