"""LIKE on a `char(n)` column compares the text, not the space-padded value (Postgres)."""

import os

import pytest

import orm

URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
SOURCE = 'model Padded {\n  id Int @id\n  k String @db.Char(6)\n  @@map("char_like_padded")\n}'


@pytest.fixture
async def padded():
    registry = orm.Registry()
    Padded = orm.loads(SOURCE, registry=registry)["Padded"]
    try:
        db = await orm.connect(URL, registry=registry, default=False)
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {URL}: {e}")
    await db.drop_tables()
    await db.create_tables()
    qs = Padded.objects.using(db)
    await qs.insert_many([{"id": i, "k": k} for i, k in enumerate(["ab", "abc", "a c"], 1)])
    yield Padded, qs
    await db.drop_tables()
    await db.close()


async def test_like_compares_the_text(padded):
    Padded, qs = padded
    k = Padded.k

    async def ids(cond):
        return sorted(await qs.filter(cond).select(Padded.id).scalars())

    assert await ids(k.endswith("b")) == [1]
    assert await ids(k.like("ab")) == [1]
    assert await ids(k.ilike("%B")) == [1]
    assert await ids(k.contains(" ")) == [3]
    assert await ids(k.startswith("ab")) == [1, 2]
    assert await ids(k.icontains("C")) == [2, 3]
    assert await ids(~k.like("ab")) == [2, 3]
    assert await ids(~k.ilike("%B")) == [2, 3]
    assert "CAST(" in qs.filter(k.like("ab")).sql() and "AS text" in qs.filter(k.like("ab")).sql()
