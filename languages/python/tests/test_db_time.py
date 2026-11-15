"""Native clocks across storage and language boundaries."""
import datetime as dt
import os

import pytest
import monorm
from monorm import _native

SCHEMA = '''
type Clock {
  at DateTime @db.Time
  missing DateTime? @db.Time
}
type Clocks {
  times DateTime[] @db.Time
}
model ListClock {
  id Int @id
  clocks Clocks @storage(json)
}
model TimeRow {
  id Int @id
  at DateTime @unique @db.Time
  zone DateTime? @db.Timetz
  clock Clock? @storage(json)
  columns Clock?
  refs TimeRef[]
}
model TimeCopy {
  id Int @id
  at DateTime @db.Time
}
model TimeRef {
  id Int @id
  at DateTime @db.Time
  parent TimeRow @relation(fields: [at], references: [at])
}
'''
WHEN = dt.time(7, 8, 9, 123456)

@pytest.fixture(params=['postgres', 'sqlite'])
async def world(request):
    reg = monorm.Registry()
    schema = SCHEMA
    if request.param == 'sqlite':
        schema = 'datasource db { provider = "sqlite" }\n' + schema
    else:
        schema = schema.replace('  zone DateTime?', '  times DateTime[] @db.Time\n  zones DateTime[] @db.Timetz\n  zone DateTime?')
    classes = monorm.loads(schema, registry=reg)
    db = await monorm.connect('sqlite://:memory:' if request.param == 'sqlite' else os.environ['ORM_TEST_DATABASE_URL'], registry=reg, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        yield db, classes, request.param
    finally:
        await db.drop_tables()
        await db.close()

async def test_native_clock_round_trip(world):
    db, classes, kind = world
    M, Ref, Clock = classes['TimeRow'], classes['TimeRef'], classes['Clock']
    q = M.objects.using(db)
    clock = Clock(at=WHEN, missing=None)
    extra = {'times': [WHEN, None], 'zones': [WHEN, None]} if kind == 'postgres' else {}
    async with db.transaction():
        if kind == 'postgres':
            await db.execute("SET LOCAL TIME ZONE 'Asia/Tehran'")
        row = await q.insert(id=1, at=WHEN, zone=WHEN, clock=clock, columns=clock, **extra)
        assert row.at == WHEN and row.zone == WHEN
        assert row.clock.at == WHEN and row.columns.at == WHEN and row.clock.missing is None
        if kind == 'postgres':
            assert row.times == [WHEN, None] and row.zones == [WHEN, None]
            assert await q.select(M.zones).scalar() == [WHEN, None]
            assert await q.filter(M.zones == [WHEN, None]).count() == 1
            stored = (await db.fetch('SELECT zone::text AS zone FROM "timerow"'))[0]
            assert stored['zone'] == '07:08:09.123456+00'
        listed = await classes['ListClock'].objects.using(db).insert(id=1, clocks=classes['Clocks'](times=[WHEN, dt.time(0)]))
        assert listed.clocks.times == (WHEN, dt.time(0))
        await q.filter(M.id == 1).update(clock={"at": M.at})
        assert (await q.get(M.id == 1)).clock.at == WHEN
        assert (await q.select(M.clock.at).scalar()) == WHEN
        assert await q.filter(M.zone == WHEN).count() == 1
        assert await q.filter(M.at == WHEN).count() == 1
        assert await q.filter(M.clock.at == WHEN).count() == 1
        await Ref.objects.using(db).insert(id=1, at=WHEN)
        related = await Ref.objects.using(db).load(Ref.parent).get(Ref.id == 1)
        assert related.parent.at == WHEN
        rows = await q.load(M.refs)
        assert rows[0].refs.cached[0].at == WHEN
        if kind == 'postgres':
            await db.execute("UPDATE timerow SET zone = '00:30:00+02'::timetz, zones = ARRAY['00:30:00+02'::timetz,NULL] WHERE id = 1")
            assert await q.filter(M.zone == dt.time(22, 30)).count() == 1
            assert await q.filter(M.zones == [dt.time(22, 30), None]).count() == 1
        await row.update(zone=None)
        assert row.zone is None

async def test_native_clock_bulk_writes(world):
    db, classes, kind = world
    M, C = classes['TimeRow'], classes['TimeCopy']
    q = M.objects.using(db)
    times = [dt.time(0), WHEN, dt.time(23, 59, 59, 999999)]
    def cells(i, t):
        return {'id': i, 'at': t, 'zone': t, **({'times': [t, None], 'zones': [t, None]} if kind == 'postgres' else {})}
    async with db.transaction():
        if kind == 'postgres':
            await db.execute("SET LOCAL TIME ZONE 'Asia/Tehran'")
        await q.insert_many([cells(i, t) for i, t in enumerate(times, 1)])
        if kind == 'postgres':
            await C.objects.using(db).insert_many([{'id': i, 'at': t} for i, t in enumerate(times, 11)], copy=True)
            with pytest.raises(monorm.QueryError, match='writes through SQL'):
                await q.insert_many([cells(21, WHEN)], copy=True)
        rows = await q.filter(M.id < 10).order_by(M.id).all()
        assert [r.at for r in rows] == times and [r.zone for r in rows] == times
        if kind == 'postgres':
            assert [r.zones for r in rows] == [[t, None] for t in times]
            assert [r.times for r in rows] == [[t, None] for t in times]
            assert [r.at for r in await C.objects.using(db).order_by(C.id).all()] == times
            stored = await db.fetch('SELECT zone::text AS zone FROM "timerow" WHERE id = 3')
            assert stored[0]['zone'] == '23:59:59.999999+00'

async def test_a_nested_clock_of_24_hours_is_a_range_error(world):
    db, classes, kind = world
    if kind == 'sqlite':
        pytest.skip('PostgreSQL stores 24:00:00')
    M = classes['TimeRow']
    q = M.objects.using(db)
    async with db.transaction():
        await db.execute("INSERT INTO timerow (id, at, times, zones) VALUES (1, '24:00:00', ARRAY[]::time[], ARRAY[]::timetz[])")
        await q.filter(M.id == 1).update(clock={'at': M.at})
        stored = (await db.fetch('SELECT clock::text AS clock FROM "timerow"'))[0]['clock']
        assert '24:00:00' in stored
        with pytest.raises(monorm.DecodeError, match='24:00:00'):
            await q.select(M.clock).scalar()

async def test_raw_clock_edges(world):
    db, _, kind = world
    if kind == 'sqlite':
        assert (await db.fetch("SELECT '07:08:09.123456' AS clock"))[0]['clock'] == '07:08:09.123456'
        return
    row = (await db.fetch("SELECT '07:08:09.123456'::time AS clock, '00:30:00+02'::timetz AS east, '23:30:00-02'::timetz AS west, ARRAY['07:08:09.123456'::time,NULL] AS times, ARRAY['00:30:00+02'::timetz,NULL] AS zones, NULL::time AS missing"))[0]
    assert row == {'clock': WHEN, 'east': dt.time(22,30), 'west': dt.time(1,30), 'times': [WHEN,None], 'zones':[dt.time(22,30),None], 'missing':None}
    with pytest.raises(monorm.DatabaseError, match='24:00:00.*not representable'):
        await db.fetch("SELECT '24:00:00'::time")
    with pytest.raises(monorm.DatabaseError, match='24:00:00.*not representable'):
        await db.fetch("SELECT ARRAY['24:00:00'::timetz]")

def test_native_clock_codegen(tmp_path):
    schema = tmp_path / 'schema.prisma'
    schema.write_text(SCHEMA)
    _, stub = _native.generate_python(str(schema))
    assert 'from datetime import time' in stub
    assert 'ColumnRef[time]' in stub
    classes = monorm.loads(SCHEMA, registry=monorm.Registry())
    assert classes['TimeRow']._meta.fields['at'].type_name == 'time'
