"""Arithmetic on a DateTime and a Duration (``datetime.timedelta``): the same microseconds on both databases."""
import datetime as dt
import os

import pytest
import monorm

SCHEMA = '''
model Event {
  id Int @id
  at DateTime
  until DateTime
  span Duration
  late DateTime?
  rest Duration?
}
'''
UTC = dt.timezone.utc
DAY = dt.timedelta(days=1)
US = dt.timedelta(microseconds=1)
AT = [dt.datetime(2024, 1, 1, tzinfo=UTC), dt.datetime(2024, 3, 1, 12, 0, 0, 500000, tzinfo=UTC), dt.datetime(2024, 1, 1, tzinfo=UTC)]
UNTIL = [AT[0] + US, dt.datetime(2024, 3, 1, 12, tzinfo=UTC), AT[2]]
SPAN = [DAY, 10 * US, 11 * US]


@pytest.fixture(params=['postgres', 'sqlite'])
async def world(request):
    reg = monorm.Registry()
    schema = SCHEMA if request.param == 'postgres' else 'datasource db { provider = "sqlite" }\n' + SCHEMA
    classes = monorm.loads(schema, registry=reg)
    db = await monorm.connect('sqlite://:memory:' if request.param == 'sqlite' else os.environ['ORM_TEST_DATABASE_URL'], registry=reg, default=False)
    await db.drop_tables()
    await db.create_tables()
    Event = classes['Event']
    q = Event.objects.using(db)
    await q.insert_many([{'id': i + 1, 'at': AT[i], 'until': UNTIL[i], 'span': SPAN[i]} for i in range(3)])
    try:
        yield q, Event
    finally:
        await db.drop_tables()
        await db.close()


async def column(q, Event, expr):
    return [r.x for r in await q.order_by(Event.id).select(expr.label('x')).all()]


async def test_a_datetime_and_a_duration_add_and_subtract_to_the_microsecond(world):
    q, Event = world
    plus = [AT[0] + DAY, AT[1] + 10 * US, AT[2] + 11 * US]
    assert await column(q, Event, Event.at + Event.span) == plus
    assert await column(q, Event, Event.span + Event.at) == plus
    assert await column(q, Event, Event.at - Event.span) == [AT[0] - DAY, AT[1] - 10 * US, AT[2] - 11 * US]
    assert (await column(q, Event, Event.at + US))[0] == dt.datetime(2024, 1, 1, 0, 0, 0, 1, tzinfo=UTC)
    assert await column(q, Event, US + Event.at) == [a + US for a in AT]
    assert await column(q, Event, Event.at + dt.timedelta(milliseconds=500)) == [a + dt.timedelta(milliseconds=500) for a in AT]


async def test_the_difference_of_two_datetimes_is_a_signed_duration(world):
    q, Event = world
    assert await column(q, Event, Event.until - Event.at) == [US, -dt.timedelta(milliseconds=500), dt.timedelta(0)]
    assert await column(q, Event, Event.at - Event.until) == [-US, dt.timedelta(milliseconds=500), dt.timedelta(0)]
    far = dt.datetime(2024, 1, 1, tzinfo=UTC)
    assert await column(q, Event, Event.at - far) == [dt.timedelta(0), AT[1] - far, dt.timedelta(0)]


async def test_durations_add_subtract_scale_and_round_to_the_nearest_microsecond(world):
    q, Event = world
    assert await column(q, Event, Event.span + Event.span) == [2 * DAY, 20 * US, 22 * US]
    assert await column(q, Event, Event.span - Event.span * 3) == [-2 * DAY, -20 * US, -22 * US]
    assert await column(q, Event, 3 * Event.span) == [3 * DAY, 30 * US, 33 * US]
    # 10 / 3 = 3.33 rounds to 3, 11 / 3 = 3.67 rounds to 4.
    assert await column(q, Event, Event.span / 3) == [DAY / 3, 3 * US, 4 * US]
    assert await column(q, Event, Event.span * 0.3) == [dt.timedelta(microseconds=25_920_000_000), 3 * US, 3 * US]


async def test_a_duration_parameter_takes_part_in_arithmetic(world):
    q, Event = world
    prepared = q.order_by(Event.id).select((Event.at + monorm.param('d')).label('x')).prepare()
    assert [r.x for r in await prepared(d=US)][0] == dt.datetime(2024, 1, 1, 0, 0, 0, 1, tzinfo=UTC)
    assert [r.x for r in await prepared(d=dt.timedelta(milliseconds=500))][0] == dt.datetime(2024, 1, 1, 0, 0, 0, 500000, tzinfo=UTC)
    late = await q.filter(Event.at + Event.span > dt.datetime(2024, 1, 1, 0, 0, 0, 5, tzinfo=UTC)).order_by(Event.id).all()
    assert [r.id for r in late] == [1, 2, 3]


async def test_a_pair_the_table_does_not_list_is_an_error(world):
    q, Event = world
    for expr in (Event.at + Event.until, Event.span * Event.span, Event.span - Event.at, Event.at * 2):
        with pytest.raises(monorm.QueryError, match='is not defined'):
            await q.select(expr.label('x')).all()


async def test_a_null_operand_gives_null(world):
    q, Event = world
    nulls = [None, None, None]
    assert await column(q, Event, Event.at + Event.rest) == nulls
    assert await column(q, Event, Event.late - Event.span) == nulls
    assert await column(q, Event, Event.late - Event.at) == nulls
    assert await column(q, Event, Event.rest + Event.span) == nulls
    assert await column(q, Event, Event.rest * 2) == nulls
    assert await column(q, Event, Event.rest / 2) == nulls


async def test_a_difference_over_a_day_and_over_a_month_is_exact(world):
    q, Event = world
    month = dt.timedelta(days=60, hours=12, microseconds=500_000)
    assert await column(q, Event, Event.at - dt.datetime(2023, 12, 31, tzinfo=UTC)) == [DAY, month + DAY, DAY]
    assert await column(q, Event, Event.at - dt.datetime(2024, 1, 1, tzinfo=UTC)) == [dt.timedelta(0), month, dt.timedelta(0)]


async def test_a_datetime_parameter_on_the_left_of_a_subtraction(world):
    q, Event = world
    prepared = q.order_by(Event.id).select((monorm.param('t', dt.datetime) - Event.at).label('x')).prepare()
    assert [r.x for r in await prepared(t=dt.datetime(2024, 1, 2, tzinfo=UTC))] == [DAY, -(dt.timedelta(days=59, hours=12, microseconds=500_000)), DAY]


async def test_a_float_parameter_scales_a_duration(world):
    q, Event = world
    expected = [dt.timedelta(microseconds=25_920_000_000), 3 * US, 3 * US]
    left = q.order_by(Event.id).select((Event.span * monorm.param('n', float)).label('x')).prepare()
    right = q.order_by(Event.id).select((monorm.param('n', float) * Event.span).label('x')).prepare()
    assert [r.x for r in await left(n=0.3)] == expected
    assert [r.x for r in await right(n=0.3)] == expected


async def test_a_division_by_zero_is_an_error_on_both_databases(world):
    q, Event = world
    for zero in (0, 0.0):
        with pytest.raises(monorm.DatabaseError, match='division by zero'):
            await q.select((Event.span / zero).label('x')).all()


async def test_a_duration_out_of_the_64_bit_range_is_an_error_on_both_databases(world):
    q, Event = world
    big = dt.timedelta(days=106_751_991)  # 9.2e18 microseconds, just under the 64-bit limit
    for expr in (Event.span * 9_000_000_000_000_000_000, 9_000_000_000_000_000_000 * Event.span, Event.span + big, Event.span - (-big), Event.at + big):
        with pytest.raises(monorm.DatabaseError, match='out of'):
            await q.select(expr.label('x')).all()
