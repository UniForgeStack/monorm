"""Presence and uniqueness are independent constraints on query results."""

import pytest

import orm


@pytest.fixture
async def required_queries():
    registry = orm.Registry()
    Note = orm.loads('''
        datasource db { provider = "sqlite" }
        model Note {
          id Int @id
          n Int?
        }
    ''', registry=registry)["Note"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await Note.objects.using(db).insert_many([{"id": 1, "n": None}, {"id": 2, "n": 5}])
        yield db, Note
    finally:
        await db.close()


async def test_optional_and_required_model_reads(required_queries):
    db, Note = required_queries
    qs = Note.objects.using(db)
    empty = qs.filter(Note.id == -1)
    assert await empty.get() is None
    assert await empty.first() is None
    assert await empty.last() is None
    assert await empty.all() == []
    assert await empty == []
    required = empty.require()
    for read in [required.get, required.first, required.last, required.all]:
        with pytest.raises(Note.DoesNotExist):
            await read()
    for _ in range(2):
        with pytest.raises(Note.DoesNotExist):
            await required
    with pytest.raises(Note.DoesNotExist):
        _ = [row async for row in required]
    # The modifier is immutable, and conditions added after it retain presence checks.
    assert await empty.get() is None
    with pytest.raises(Note.DoesNotExist):
        await qs.require().filter(Note.id == -1).get()
    assert (await qs.require().get(Note.id == 1)).id == 1
    assert (await qs.get(Note.id == 1)).id == 1
    assert (await qs.order_by(Note.id).require().first()).id == 1
    assert (await qs.order_by(Note.id).require().last()).id == 2
    assert len(await qs.require().all()) == 2
    for query in [qs, qs.require()]:
        with pytest.raises(Note.MultipleObjectsReturned):
            await query.get()
    assert await required.count() == 0
    assert await required.exists() is False


async def test_optional_and_required_selected_rows(required_queries):
    db, Note = required_queries
    qs = Note.objects.using(db).select(Note.n)
    empty = qs.filter(Note.id == -1)
    assert await empty.get() is None
    assert await empty.first() is None
    assert await empty.all() == []
    for read in [empty.require().get, empty.require().first, empty.require().all]:
        with pytest.raises(orm.DoesNotExist):
            await read()
    with pytest.raises(orm.DoesNotExist):
        await empty.require()
    assert await empty.get() is None
    for query in [qs, qs.require()]:
        with pytest.raises(orm.MultipleObjectsReturned):
            await query.get()
    assert await qs.require().get(Note.id == 1) == (None,)
    assert await qs.order_by(Note.id).require().first() == (None,)
    assert await qs.order_by(Note.id).require().all() == [(None,), (5,)]
    assert await Note.objects.using(db).require().filter(Note.id == 1).select(Note.n).get() == (None,)
    assert await qs.filter(Note.id == 1).require().scalar() is None
    with pytest.raises(orm.DoesNotExist):
        await empty.require().scalar()
    with pytest.raises(orm.DoesNotExist):
        await empty.require().scalars()


async def test_required_prepared_queries(required_queries):
    db, Note = required_queries
    qs = Note.objects.using(db).filter(Note.id == orm.param("id"))
    optional = qs.prepare()
    assert await optional.get(id=-1) is None
    assert await optional.first(id=-1) is None
    assert await optional(id=-1) == []
    for required in [optional.require(), qs.require().prepare()]:
        assert (await required.get(id=1)).id == 1
        assert (await required.first(id=1)).id == 1
        assert len(await required.all(id=1)) == 1
        assert len(await required(id=1)) == 1
        for read in [required.get, required.first, required.all, required]:
            with pytest.raises(Note.DoesNotExist):
                await read(id=-1)
    multiple = Note.objects.using(db).prepare()
    for query in [multiple, multiple.require()]:
        with pytest.raises(Note.MultipleObjectsReturned):
            await query.get()
    assert await optional.get(id=-1) is None
