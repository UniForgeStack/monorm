"""A Duration column: whole microseconds, a ``datetime.timedelta`` in Python."""
import datetime as dt
import os

import pytest
import monorm
from monorm import _native

SCHEMA = '''
model Job {
  id Int @id
  timeout Duration?
}
'''
DAY = dt.timedelta(days=1)
VALUES = [
    dt.timedelta(0),
    dt.timedelta(microseconds=1),
    dt.timedelta(microseconds=-1),
    dt.timedelta(hours=1, minutes=30),
    dt.timedelta(days=999, microseconds=123456),
    dt.timedelta(days=-3, seconds=-7),
]


@pytest.fixture(params=['postgres', 'sqlite'])
async def world(request):
    reg = monorm.Registry()
    schema = SCHEMA if request.param == 'postgres' else 'datasource db { provider = "sqlite" }\n' + SCHEMA
    classes = monorm.loads(schema, registry=reg)
    db = await monorm.connect('sqlite://:memory:' if request.param == 'sqlite' else os.environ['ORM_TEST_DATABASE_URL'], registry=reg, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        yield db, classes['Job'], request.param
    finally:
        await db.drop_tables()
        await db.close()


async def fill(q):
    await q.insert_many([{'id': i, 'timeout': v} for i, v in enumerate(VALUES, 1)] + [{'id': 7, 'timeout': None}])


async def test_a_duration_round_trips_and_orders(world):
    db, Job, _ = world
    q = Job.objects.using(db)
    row = await q.insert(id=100, timeout=dt.timedelta(days=1, hours=2, microseconds=3))
    assert row.timeout == dt.timedelta(days=1, hours=2, microseconds=3)
    await q.filter(Job.id == 100).delete()
    await fill(q)
    rows = await q.filter(Job.id < 10).order_by(Job.id).all()
    assert [r.timeout for r in rows] == VALUES + [None]
    ordered = await q.filter(Job.timeout.is_not_null()).order_by(Job.timeout).select(Job.timeout).all()
    assert [r.timeout for r in ordered] == sorted(VALUES)
    await q.filter(Job.id == 1).update(timeout=dt.timedelta(minutes=5))
    assert (await q.get(Job.id == 1)).timeout == dt.timedelta(minutes=5)


async def test_comparison_between_and_in_work_on_durations(world):
    db, Job, _ = world
    q = Job.objects.using(db)
    await fill(q)
    async def ids(cond):
        return sorted(r.id for r in await q.filter(cond).all())
    assert await ids(Job.timeout > dt.timedelta(0)) == [2, 4, 5]
    assert await ids(Job.timeout <= dt.timedelta(0)) == [1, 3, 6]
    assert await ids(Job.timeout == dt.timedelta(microseconds=-1)) == [3]
    assert await ids((Job.timeout != dt.timedelta(0)).and_not_null()) == [2, 3, 4, 5, 6]
    assert await ids(Job.timeout.between(dt.timedelta(microseconds=-1), dt.timedelta(microseconds=1))) == [1, 2, 3]
    assert await ids(Job.timeout.in_([dt.timedelta(hours=1, minutes=30), dt.timedelta(0)])) == [1, 4]


async def test_sum_avg_min_max_return_durations(world):
    db, Job, _ = world
    q = Job.objects.using(db)
    await fill(q)
    small = q.filter(Job.id.in_([2, 3, 4]))
    assert await small.select(monorm.func.sum(Job.timeout)).scalar() == dt.timedelta(hours=1, minutes=30)
    assert await small.select(monorm.func.min(Job.timeout)).scalar() == dt.timedelta(microseconds=-1)
    assert await small.select(monorm.func.max(Job.timeout)).scalar() == dt.timedelta(hours=1, minutes=30)
    two = q.filter(Job.id.in_([2, 3, 4]) & (Job.timeout < dt.timedelta(hours=1)))
    assert await two.select(monorm.func.avg(Job.timeout)).scalar() == dt.timedelta(0)
    assert await q.filter(Job.id == 7).select(monorm.func.sum(Job.timeout)).scalar() is None


async def test_a_cursor_pages_through_durations(world):
    db, Job, _ = world
    q = Job.objects.using(db)
    await fill(q)
    qs = q.filter(Job.timeout.is_not_null()).order_by(Job.timeout.asc(nulls='last'), Job.id)
    seen, cursor = [], None
    while True:
        page = await qs.paginate(first=2, after=cursor)
        seen += [r.timeout for r in page.items]
        if not page.has_next:
            break
        cursor = page.next_cursor
    assert seen == sorted(VALUES)


async def test_a_duration_beyond_64_bit_microseconds_is_an_overflow_error(world):
    db, Job, _ = world
    with pytest.raises(OverflowError):
        await Job.objects.using(db).insert(id=1, timeout=dt.timedelta(days=200_000_000))


async def test_a_postgres_interval_reads_months_as_30_days(world):
    db, Job, kind = world
    if kind == 'sqlite':
        pytest.skip('only PostgreSQL has months in an interval')
    await db.execute("""INSERT INTO "job" (id, timeout) VALUES (1, '1 month 2 days'), (2, '1 year'), (3, '-1 microsecond')""")
    rows = await Job.objects.using(db).order_by(Job.id).all()
    assert [r.timeout for r in rows] == [32 * DAY, 360 * DAY, dt.timedelta(microseconds=-1)]
    stored = (await db.fetch('SELECT timeout::text AS t FROM "job" WHERE id = 3'))[0]['t']
    assert stored == '-00:00:00.000001'


def test_a_duration_field_has_the_duration_type(tmp_path):
    schema = tmp_path / 'schema.prisma'
    schema.write_text(SCHEMA)
    _, stub = _native.generate_python(str(schema))
    assert 'ColumnRef[timedelta' in stub
    classes = monorm.loads(SCHEMA, registry=monorm.Registry())
    assert classes['Job']._meta.fields['timeout'].type_name == 'duration'


def test_a_duration_has_the_text_form_of_the_rust_core():
    import json
    import pathlib
    from monorm._duration import format_duration, parse_duration

    vectors = json.loads((pathlib.Path(__file__).parents[2] / 'test-vectors/duration-text.json').read_text())
    for micros, text in vectors['roundtrip']:
        value = dt.timedelta(microseconds=int(micros))
        assert format_duration(value) == text
        assert parse_duration(text) == value
    for text, micros in vectors['parse_only']:
        assert parse_duration(text) == dt.timedelta(microseconds=int(micros)), text
    for bad in vectors['refused']:
        with pytest.raises(ValueError):
            parse_duration(bad)


async def test_a_duration_in_a_union_branch_and_a_prepared_parameter_filter(world):
    db, Job, kind = world
    q = Job.objects.using(db)
    await fill(q)
    low = q.select(Job.id).filter(Job.timeout < dt.timedelta(0))
    high = q.select(Job.id).filter(Job.timeout > dt.timedelta(hours=1))
    assert [r.id for r in await low.union_all(high).order_by('id')] == [3, 4, 5, 6]
    lt = q.filter(Job.timeout < monorm.param('v')).order_by(Job.id).prepare()
    assert [r.id for r in await lt(v=dt.timedelta(0))] == [3, 6]


async def test_postgres_refuses_a_prepared_in_list_and_copy_on_a_duration(world):
    db, Job, kind = world
    if kind != 'postgres':
        pytest.skip('the write template only exists on PostgreSQL')
    q = Job.objects.using(db)
    with pytest.raises(monorm.QueryError, match='write_sql'):
        await q.filter(Job.timeout.in_(monorm.param('v'))).prepare()(v=[dt.timedelta(0)])
    with pytest.raises(monorm.QueryError, match='copy=True'):
        await q.insert_many([{'id': 1, 'timeout': dt.timedelta(0)}], copy=True)
