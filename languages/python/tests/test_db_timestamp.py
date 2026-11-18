"""`@db.Timestamp` writes and filters keep the UTC wall clock under any session zone."""
import datetime as dt
import os

import pytest
import monorm
from monorm import param

SCHEMA = '''
model StampRow {
  id Int @id
  at DateTime @unique @db.Timestamp(6)
  maybe DateTime? @db.Timestamp(6)
  ats DateTime[] @db.Timestamp(6)
  refs StampRef[]
}
model StampCopy {
  id Int @id
  at DateTime @db.Timestamp(6)
  maybe DateTime? @db.Timestamp(6)
}
type StampAddr {
  city String
  opened DateTime
}
model StampDoc {
  id Int @id
  addr StampAddr? @storage(json)
  src DateTime @db.Timestamp(6)
}
model StampRef {
  id Int @id
  at DateTime @db.Timestamp(6)
  parent StampRow @relation(fields: [at], references: [at])
}
'''
WHEN = dt.datetime(2024, 1, 2, 7, 8, 9, 123456, tzinfo=dt.timezone.utc)
NAIVE = '2024-01-02 07:08:09.123456'
ZONES = ['Asia/Tehran', 'Pacific/Honolulu']


@pytest.fixture
async def world():
    reg = monorm.Registry()
    classes = monorm.loads(SCHEMA, registry=reg)
    db = await monorm.connect(os.environ['ORM_TEST_DATABASE_URL'], registry=reg, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        yield db, classes
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize('zone', ZONES)
async def test_timestamp_writes_keep_the_utc_wall_clock_under_a_session_zone(world, zone):
    db, classes = world
    M, Ref = classes['StampRow'], classes['StampRef']
    q = M.objects.using(db)
    later = WHEN + dt.timedelta(days=1)
    async with db.transaction():
        await db.execute(f"SET LOCAL TIME ZONE '{zone}'")
        row = await q.insert(id=1, at=WHEN, maybe=WHEN, ats=[WHEN, later])
        assert row.at == WHEN and row.ats == [WHEN, later]
        stored = (await db.fetch('SELECT at::text AS at, maybe::text AS maybe, ats::text AS ats FROM "stamprow" WHERE id = 1'))[0]
        assert stored['at'] == NAIVE and stored['maybe'] == NAIVE
        assert stored['ats'] == '{"2024-01-02 07:08:09.123456","2024-01-03 07:08:09.123456"}'
        assert await q.filter(M.at == WHEN).count() == 1
        assert await q.filter(M.at.in_([WHEN, later])).count() == 1
        assert await q.filter(M.ats == [WHEN, later]).count() == 1
        await q.filter(M.id == 1).update(at=later, maybe=None, ats=[later])
        assert (await db.fetch('SELECT at::text AS at FROM "stamprow"'))[0]['at'] == '2024-01-03 07:08:09.123456'
        assert (await q.get(M.id == 1)).at == later
        await q.insert(id=1, at=later, maybe=WHEN, ats=[]).on_conflict(M.id, update=True)
        assert (await q.get(M.id == 1)).maybe == WHEN
        assert (await db.fetch('SELECT maybe::text AS maybe FROM "stamprow"'))[0]['maybe'] == NAIVE
        await Ref.objects.using(db).insert(id=1, at=later)
        related = await Ref.objects.using(db).load(Ref.parent).get(Ref.id == 1)
        assert related.parent.at == later
        rows = await q.load(M.refs)
        assert rows[0].refs.cached[0].at == later


@pytest.mark.parametrize('zone', ZONES)
async def test_timestamp_bulk_writes_keep_the_utc_wall_clock_under_a_session_zone(world, zone):
    db, classes = world
    M, C = classes['StampRow'], classes['StampCopy']
    q = M.objects.using(db)
    stamps = [dt.datetime(2024, 1, 1, tzinfo=dt.timezone.utc), WHEN, dt.datetime(2024, 12, 31, 23, 59, 59, 999999, tzinfo=dt.timezone.utc)]
    async with db.transaction():
        await db.execute(f"SET LOCAL TIME ZONE '{zone}'")
        await q.insert_many([{'id': i, 'at': s, 'maybe': s, 'ats': [s]} for i, s in enumerate(stamps, 1)])
        await C.objects.using(db).insert_many([{'id': i, 'at': s, 'maybe': s} for i, s in enumerate(stamps, 1)], copy=True)
        rows = await q.filter(M.id < 10).order_by(M.id).all()
        assert [r.at for r in rows] == stamps and [r.maybe for r in rows] == stamps
        assert [r.ats for r in rows] == [[s] for s in stamps]
        stored = await db.fetch('SELECT at::text AS at FROM "stamprow" WHERE id < 10 ORDER BY id')
        assert [r['at'] for r in stored] == ['2024-01-01 00:00:00', NAIVE, '2024-12-31 23:59:59.999999']
        copied = await db.fetch('SELECT at::text AS at, maybe::text AS maybe FROM "stampcopy" ORDER BY id')
        assert [r['at'] for r in copied] == [r['maybe'] for r in copied] == ['2024-01-01 00:00:00', NAIVE, '2024-12-31 23:59:59.999999']


@pytest.mark.parametrize('zone', ZONES)
async def test_timestamp_prepared_statements_keep_the_utc_wall_clock_under_a_session_zone(world, zone):
    db, classes = world
    M = classes['StampRow']
    q = M.objects.using(db)
    later = WHEN + dt.timedelta(days=1)
    async with db.transaction():
        await db.execute(f"SET LOCAL TIME ZONE '{zone}'")
        await q.insert(id=param('i'), at=param('a'), maybe=param('a'), ats=[]).prepare()(i=1, a=WHEN)
        await q.insert_many(param('rows', fields=(M.id, M.at, M.ats))).prepare()(rows=[{'id': 2, 'at': later, 'ats': [later]}])
        stored = await db.fetch('SELECT at::text AS at, maybe::text AS maybe, ats::text AS ats FROM "stamprow" ORDER BY id')
        assert [r['at'] for r in stored] == [NAIVE, '2024-01-03 07:08:09.123456']
        assert stored[0]['maybe'] == NAIVE and stored[1]['ats'] == '{"2024-01-03 07:08:09.123456"}'
        assert [r.id for r in await q.filter(M.at == param('a')).prepare()(a=WHEN)] == [1]
        assert [r.id for r in await q.filter(M.at.in_(param('v'))).order_by(M.id).prepare()(v=[WHEN, later])] == [1, 2]
        await q.filter(M.id == 1).update(at=param('a')).prepare()(a=later + dt.timedelta(days=1))
        assert (await db.fetch('SELECT at::text AS at FROM "stamprow" WHERE id = 1'))[0]['at'] == '2024-01-04 07:08:09.123456'


@pytest.mark.parametrize('zone', ZONES)
async def test_timestamp_nested_expression_and_prepared_keys_and_rows_keep_the_utc_wall_clock_under_a_session_zone(world, zone):
    db, classes = world
    M, Ref, Doc = classes['StampRow'], classes['StampRef'], classes['StampDoc']
    q = M.objects.using(db)
    later = WHEN + dt.timedelta(days=1)
    async with db.transaction():
        await db.execute(f"SET LOCAL TIME ZONE '{zone}'")
        await q.insert_many([{'id': 1, 'at': WHEN, 'ats': []}, {'id': 2, 'at': later, 'ats': []}])
        await Ref.objects.using(db).insert_many([{'id': 1, 'at': WHEN}, {'id': 2, 'at': later}])
        await Doc.objects.using(db).insert(id=1, addr={'city': 'Oslo', 'opened': later}, src=WHEN)
        await Doc.objects.using(db).filter(Doc.id == 1).update(addr={'opened': Doc.src})
        assert (await Doc.objects.using(db).get(Doc.id == 1)).addr.opened == WHEN
        rows = await q.load(M.refs).order_by(M.id).prepare()()
        assert [[c.id for c in r.refs.cached] for r in rows] == [[1], [2]]
        edit = q.update_many(param('rows', fields=(M.id, M.maybe))).prepare()
        await edit(rows=[{'id': 1, 'maybe': later + dt.timedelta(days=1)}, {'id': 2, 'maybe': later + dt.timedelta(days=2)}])
        stored = await db.fetch('SELECT maybe::text AS maybe FROM "stamprow" ORDER BY id')
        assert [r['maybe'] for r in stored] == ['2024-01-04 07:08:09.123456', '2024-01-05 07:08:09.123456']
