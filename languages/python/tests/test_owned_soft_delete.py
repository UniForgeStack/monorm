"""Owning relations with soft delete: the sweep, the owned revive and `sweep_owned()`, on PostgreSQL and SQLite."""

import asyncio
from types import SimpleNamespace

import pytest

import monorm
from artifact_caps import has_extension
from conftest import DATABASE_URL

pytestmark = pytest.mark.skipif(not has_extension("monorm.soft-delete"), reason="requires the soft-delete package")

SOFT = '@@soft_delete.deleted_at("deleted")'

SCHEMA = f"""
model Spec {{
  id      BigInt   @id
  body    String
  guitars Guitar[]
  banjos  Banjo[]
  frames  Frame[]
  {SOFT}
  @@map("os_spec")
}}

model Guitar {{
  id      BigInt  @id
  spec_id BigInt?
  spec    Spec?   @relation(fields: [spec_id], references: [id], onDelete: NoAction, owns: true)
  {SOFT}
  @@map("os_guitar")
}}

model Banjo {{
  id      BigInt  @id
  spec_id BigInt?
  spec    Spec?   @relation(fields: [spec_id], references: [id], onDelete: NoAction, owns: true)
  {SOFT}
  @@map("os_banjo")
}}

model Frame {{
  id      BigInt  @id
  spec_id BigInt?
  spec    Spec?   @relation(fields: [spec_id], references: [id], onDelete: NoAction, owns: true)
  @@map("os_frame")
}}
"""

# The target comes first, so one pass of `sweep_owned(repair=True)` meets it before its owner.
CHAIN = f"""
model Detail {{
  id      BigInt   @id
  baskets Basket[]
  {SOFT}
  @@map("os_detail")
}}

model Basket {{
  id        BigInt  @id
  orders    Order[]
  detail_id BigInt
  detail    Detail  @relation(fields: [detail_id], references: [id], onDelete: NoAction, owns: true)
  {SOFT}
  @@map("os_basket")
}}

model Order {{
  id        BigInt @id
  basket_id BigInt
  basket    Basket @relation(fields: [basket_id], references: [id], onDelete: NoAction, owns: true)
  {SOFT}
  @@map("os_order")
}}
"""

APP = """
model AppSpec {
  id     BigInt     @id
  owners AppOwner[]
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("os_app_spec")
}

model AppOwner {
  id      BigInt   @id
  spec_id BigInt?
  spec    AppSpec? @relation(fields: [spec_id], references: [id], onDelete: NoAction, owns: true)
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("os_app_owner")
}
"""

CYCLE = f"""
model Left {{
  id       BigInt  @id
  right_id BigInt?
  right    Right?  @relation("LeftOwnsRight", fields: [right_id], references: [id], onDelete: NoAction, owns: true)
  lefts    Right[] @relation("RightOwnsLeft")
  {SOFT}
  @@map("os_left")
}}

model Right {{
  id      BigInt @id
  left_id BigInt?
  left    Left?  @relation("RightOwnsLeft", fields: [left_id], references: [id], onDelete: NoAction, owns: true)
  rights  Left[] @relation("LeftOwnsRight")
  {SOFT}
  @@map("os_right")
}}
"""

# One owner with two owning columns into two targets, and a soft cascade with a kept child.
MULTI = f"""
model Cover {{
  id   BigInt @id
  kits Kit[]
  {SOFT}
  @@map("os_cover")
}}

model Stand {{
  id   BigInt @id
  kits Kit[]
  {SOFT}
  @@map("os_stand")
}}

model Kit {{
  id       BigInt @id
  cover_id BigInt?
  stand_id BigInt?
  cover    Cover? @relation(fields: [cover_id], references: [id], onDelete: NoAction, owns: true)
  stand    Stand? @relation(fields: [stand_id], references: [id], onDelete: NoAction, owns: true)
  {SOFT}
  @@map("os_kit")
}}
"""

SHELF = f"""
model Shelf {{
  id    BigInt @id
  books Book[]
  memos Memo[]
  {SOFT}
  @@map("os_shelf")
}}

model Book {{
  id       BigInt @id
  shelf_id BigInt
  shelf    Shelf  @relation(fields: [shelf_id], references: [id], onDelete: Cascade)
  {SOFT}
  @@map("os_book")
}}

model Memo {{
  id       BigInt @id
  shelf_id BigInt
  shelf    Shelf  @relation(fields: [shelf_id], references: [id], onDelete: Cascade) @soft_delete.keep_child_rows
  {SOFT}
  @@map("os_memo")
}}
"""

# A chain of rows that own their parent: the repair stamps one level in each pass, and the bound is two passes.
NODE = """
model Node {
  id        BigInt  @id
  parent_id BigInt?
  parent    Node?   @relation("Chain", fields: [parent_id], references: [id], onDelete: NoAction, owns: true)
  children  Node[]  @relation("Chain")
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@map("os_node")
}
"""

SQLITE = 'datasource db {\n  provider = "sqlite"\n}\n'
T1 = "'2020-01-01 00:00:00.000'"
T2 = "'2020-01-02 00:00:00.000'"
T3 = "'2020-01-03 00:00:00.000'"


def url_of(kind):
    return "sqlite://:memory:" if kind == "sqlite" else DATABASE_URL


async def open_db(kind, schema, header=""):
    registry = monorm.Registry()
    source = (SQLITE if kind == "sqlite" else header) + schema
    m = monorm.loads(source, registry=registry)
    try:
        db = await monorm.connect(url_of(kind), registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    now = "now()" if kind == "postgres" else "strftime('%Y-%m-%d %H:%M:%f','now')"
    return SimpleNamespace(db=db, m=m, kind=kind, now=now, registry=registry)


async def close(env):
    await env.db.drop_tables()
    await env.db.close()


@pytest.fixture(params=["sqlite", "postgres"])
async def env(request):
    e = await open_db(request.param, SCHEMA)
    yield e
    await close(e)


@pytest.fixture(params=["sqlite", "postgres"])
async def chain(request):
    e = await open_db(request.param, CHAIN)
    yield e
    await close(e)


@pytest.fixture(params=["sqlite", "postgres"])
async def multi(request):
    e = await open_db(request.param, MULTI)
    yield e
    await close(e)


@pytest.fixture(params=["sqlite", "postgres"])
async def shelf(request):
    e = await open_db(request.param, SHELF)
    yield e
    await close(e)


@pytest.fixture
async def pg():
    e = await open_db("postgres", SCHEMA)
    yield e
    await close(e)


async def archived(db, table):
    return [r["id"] for r in await db.fetch(f"SELECT id FROM {table} WHERE deleted IS NOT NULL ORDER BY id")]


async def stamp(db, table, key):
    return (await db.fetch(f"SELECT deleted FROM {table} WHERE id = {key}"))[0]["deleted"]


async def seed(e, guitars=(), banjos=(), specs=(1, 2)):
    m, db = e.m, e.db
    await m["Spec"].objects.using(db).insert_many([{"id": i, "body": f"s{i}"} for i in specs])
    if guitars:
        await m["Guitar"].objects.using(db).insert_many([{"id": g, "spec_id": s} for g, s in guitars])
    if banjos:
        await m["Banjo"].objects.using(db).insert_many([{"id": g, "spec_id": s} for g, s in banjos])


async def delete(e, model, *keys):
    M = e.m[model]
    return await M.objects.using(e.db).filter(M.id.in_(list(keys))).delete()


async def undelete(e, model, *keys):
    M = e.m[model]
    return await M.objects.using(e.db).deleted_only().filter(M.id.in_(list(keys))).undelete()


async def test_the_last_soft_deleted_owner_archives_the_owned_row_with_its_stamp(env):
    await seed(env, guitars=[(1, 1), (2, 2)])
    await delete(env, "Guitar", 1)
    assert await archived(env.db, "os_spec") == [1]
    assert await stamp(env.db, "os_spec", 1) == await stamp(env.db, "os_guitar", 1)
    assert await archived(env.db, "os_guitar") == [1]


async def test_a_shared_owned_row_stays_live_until_its_last_owner_is_archived(env):
    await seed(env, guitars=[(1, 1), (2, 1)])
    await delete(env, "Guitar", 1)
    assert await archived(env.db, "os_spec") == []
    await delete(env, "Guitar", 2)
    assert await archived(env.db, "os_spec") == [1]
    assert await stamp(env.db, "os_spec", 1) == await stamp(env.db, "os_guitar", 2)


async def test_owners_of_two_models_share_the_owned_row(env):
    await seed(env, guitars=[(1, 1)], banjos=[(1, 1)])
    await delete(env, "Guitar", 1)
    assert await archived(env.db, "os_spec") == []
    await delete(env, "Banjo", 1)
    assert await archived(env.db, "os_spec") == [1]


async def test_all_owners_archived_in_one_statement_archive_the_shared_row(env):
    await seed(env, guitars=[(1, 1), (2, 1), (3, 2)])
    await delete(env, "Guitar", 1, 2)
    assert await archived(env.db, "os_spec") == [1]
    assert await archived(env.db, "os_guitar") == [1, 2]


async def test_a_key_move_and_an_archive_in_one_statement_release_the_old_row_only(env):
    db = env.db
    await seed(env, guitars=[(1, 1)])
    await db.execute(f"UPDATE os_guitar SET spec_id = 2, deleted = {env.now} WHERE id = 1")
    assert await archived(db, "os_spec") == [1]
    report = await sweep()(db)
    assert report == {"Spec": [{"id": 2}]}


def sweep():
    from monorm_soft_delete import sweep_owned

    async def run(db, **kw):
        return await sweep_owned(db=db, **kw)

    return run


async def test_an_undelete_with_the_same_stamp_revives_the_owned_row(env):
    await seed(env, guitars=[(1, 1), (2, 1)])
    await delete(env, "Guitar", 1, 2)
    assert await archived(env.db, "os_spec") == [1]
    await undelete(env, "Guitar", 1)
    assert await archived(env.db, "os_spec") == []
    assert await archived(env.db, "os_guitar") == [2]


async def test_an_undelete_with_another_stamp_keeps_the_row_while_another_owner_is_live(env):
    db = env.db
    await seed(env, guitars=[(1, 1), (2, 1)])
    await db.execute(f"UPDATE os_spec SET deleted = {T1} WHERE id = 1")
    await db.execute(f"UPDATE os_guitar SET deleted = {T2} WHERE id = 1")
    await db.execute("UPDATE os_guitar SET deleted = NULL WHERE id = 1")
    assert await archived(db, "os_spec") == [1]
    assert await archived(db, "os_guitar") == []


async def test_an_undelete_of_the_only_live_owner_revives_the_row_whatever_its_stamp(env):
    db = env.db
    await seed(env, guitars=[(1, 1), (2, 1)])
    await db.execute(f"UPDATE os_spec SET deleted = {T1} WHERE id = 1")
    await db.execute(f"UPDATE os_guitar SET deleted = {T2} WHERE id = 1")
    await db.execute(f"UPDATE os_guitar SET deleted = {T3} WHERE id = 2")
    await db.execute("UPDATE os_guitar SET deleted = NULL WHERE id = 1")
    assert await archived(db, "os_spec") == []
    assert await archived(db, "os_guitar") == [2]


async def test_the_sweep_and_the_revive_go_through_a_two_level_chain(chain):
    db, m = chain.db, chain.m
    await m["Detail"].objects.using(db).insert_many([{"id": 1}])
    await m["Basket"].objects.using(db).insert_many([{"id": 1, "detail_id": 1}])
    await m["Order"].objects.using(db).insert_many([{"id": 1, "basket_id": 1}])
    await delete(chain, "Order", 1)
    assert await archived(db, "os_basket") == [1]
    assert await archived(db, "os_detail") == [1]
    assert await stamp(db, "os_detail", 1) == await stamp(db, "os_order", 1)
    await undelete(chain, "Order", 1)
    assert await archived(db, "os_basket") == []
    assert await archived(db, "os_detail") == []


async def test_a_two_model_cycle_ends_on_postgresql():
    e = await open_db("postgres", CYCLE)
    try:
        db, m = e.db, e.m
        await m["Left"].objects.using(db).insert_many([{"id": 1, "right_id": None}])
        await m["Right"].objects.using(db).insert_many([{"id": 1, "left_id": 1}])
        await db.execute("UPDATE os_left SET right_id = 1 WHERE id = 1")
        await delete(e, "Left", 1)
        assert await archived(db, "os_left") == [1]
        assert await archived(db, "os_right") == [1]
    finally:
        await close(e)


@pytest.mark.parametrize("kind", ["sqlite", "postgres"])
async def test_application_mode_adds_no_sweep_and_the_target_stays_live(kind):
    e = await open_db(kind, APP)
    try:
        db, m = e.db, e.m
        await m["AppSpec"].objects.using(db).insert_many([{"id": 1}])
        await m["AppOwner"].objects.using(db).insert_many([{"id": 1, "spec_id": 1}])
        await m["AppOwner"].objects.using(db).filter(m["AppOwner"].id == 1).delete()
        assert await archived(db, "os_app_owner") == [1]
        assert await archived(db, "os_app_spec") == []
        if kind == "sqlite":
            names = [r["name"] for r in await db.fetch("SELECT name FROM sqlite_master WHERE type = 'trigger'")]
        else:
            names = [r["tgname"] for r in await db.fetch("SELECT tgname FROM pg_trigger WHERE NOT tgisinternal AND tgrelid IN ('os_app_spec'::regclass, 'os_app_owner'::regclass)")]
        assert not [n for n in names if "soft_delete" in n], names
    finally:
        await close(e)


async def test_skip_triggers_of_soft_delete_still_raises():
    # A skippable extension must be in use: only then does `skip_triggers()` check the names.
    extra = 'model Clock {\n  id Int @id\n  @@timestamps.updated_at("changed")\n  @@map("os_clock")\n}\n'
    e = await open_db("postgres", SCHEMA + extra)
    try:
        with monorm.skip_triggers("timestamps"):
            pass
        with pytest.raises(ValueError, match="no skippable triggers for soft-delete"):
            monorm.skip_triggers("soft-delete")
    finally:
        await close(e)


async def test_raw_sql_update_and_delete_give_the_result_of_the_model_call(env):
    db = env.db
    await seed(env, guitars=[(1, 1), (2, 2), (3, 3)], specs=(1, 2, 3))
    await delete(env, "Guitar", 1)
    await db.execute(f"UPDATE os_guitar SET deleted = {env.now} WHERE id = 2")
    await db.execute("DELETE FROM os_guitar WHERE id = 3")
    assert await archived(db, "os_spec") == [1, 2, 3]
    assert await archived(db, "os_guitar") == [1, 2, 3]
    await db.execute("UPDATE os_guitar SET deleted = NULL WHERE id IN (1, 2)")
    assert await archived(db, "os_spec") == [3]


async def test_a_live_owned_row_goes_with_the_hard_delete_of_an_owner_without_soft_delete(env):
    db = env.db
    await seed(env, specs=(1, 2))
    await env.m["Frame"].objects.using(db).insert_many([{"id": 1, "spec_id": 1}])
    await db.execute("DELETE FROM os_frame WHERE id = 1")
    remaining = [r["id"] for r in await db.fetch("SELECT id FROM os_spec ORDER BY id")]
    live = await archived(db, "os_spec")
    # PostgreSQL deletes the live row; SQLite soft-deletes it first (the rewrite of the delete fires before the owner trigger).
    assert (remaining, live) == (([2], []) if env.kind == "postgres" else ([1, 2], [1])), (remaining, live)


async def test_a_soft_owner_of_a_row_without_soft_delete_is_a_schema_error():
    source = """
model Plain {
  id     BigInt @id
  owners Own[]
}
model Own {
  id       BigInt @id
  plain_id BigInt?
  plain    Plain? @relation(fields: [plain_id], references: [id], onDelete: NoAction, owns: true)
  @@soft_delete.deleted_at("deleted")
}
"""
    with pytest.raises(Exception, match="keeps the Plain rows"):
        monorm.loads(source, registry=monorm.Registry())


async def test_two_concurrent_co_owner_deletes_archive_the_shared_row(pg):
    db, m = pg.db, pg.m
    other = await monorm.connect(DATABASE_URL, registry=pg.registry, default=False)
    try:
        await seed(pg, guitars=[(1, 1), (2, 1)])
        release = asyncio.Event()
        first_done = asyncio.Event()

        async def first():
            async with db.transaction():
                await db.execute(f"UPDATE os_guitar SET deleted = {pg.now} WHERE id = 1")
                first_done.set()
                await release.wait()

        async def second():
            await first_done.wait()
            async with other.transaction():
                await other.execute(f"UPDATE os_guitar SET deleted = {pg.now} WHERE id = 2")

        t1 = asyncio.create_task(first())
        t2 = asyncio.create_task(second())
        await asyncio.sleep(0.5)
        assert not t2.done(), "the second delete must wait for the lock on the shared row"
        release.set()
        await asyncio.wait_for(asyncio.gather(t1, t2), 10)
        assert await archived(db, "os_spec") == [1]
    finally:
        await other.close()


async def test_a_new_owner_waits_for_the_sweep_that_holds_the_lock_of_the_target(pg):
    db = pg.db
    other = await monorm.connect(DATABASE_URL, registry=pg.registry, default=False)
    try:
        await seed(pg, guitars=[(1, 1)])
        release = asyncio.Event()
        locked = asyncio.Event()

        async def sweep_owner():
            async with db.transaction():
                await db.execute(f"UPDATE os_guitar SET deleted = {pg.now} WHERE id = 1")
                locked.set()
                await release.wait()

        async def new_owner():
            await locked.wait()
            async with other.transaction():
                await other.execute("INSERT INTO os_banjo (id, spec_id) VALUES (1, 1)")

        t1 = asyncio.create_task(sweep_owner())
        t2 = asyncio.create_task(new_owner())
        await asyncio.sleep(0.5)
        assert not t2.done(), "the foreign key check of a new owner must wait for the lock on the target"
        release.set()
        await asyncio.wait_for(asyncio.gather(t1, t2), 10)
    finally:
        await other.close()


async def test_sweep_owned_reports_a_live_row_whose_owners_are_all_archived(env):
    db = env.db
    await seed(env, specs=(1, 2, 3))
    await db.execute(f"INSERT INTO os_guitar (id, spec_id, deleted) VALUES (1, 1, {T1})")
    await db.execute(f"INSERT INTO os_banjo (id, spec_id, deleted) VALUES (1, 1, {T1}), (2, 3, {T1})")
    await db.execute("INSERT INTO os_guitar (id, spec_id) VALUES (2, 3)")
    assert await sweep()(db) == {"Spec": [{"id": 1}]}
    assert await archived(db, "os_spec") == []


async def test_sweep_owned_of_a_clean_database_is_empty(env):
    await seed(env, guitars=[(1, 1)])
    assert await sweep()(env.db) == {}
    assert await sweep()(env.db, repair=True) == {}


async def test_sweep_owned_repair_archives_the_row_and_a_second_report_is_empty(env):
    db = env.db
    await seed(env, specs=(1, 2))
    await db.execute(f"INSERT INTO os_guitar (id, spec_id, deleted) VALUES (1, 1, {T1})")
    assert await sweep()(db, repair=True) == {"Spec": [{"id": 1}]}
    assert await archived(db, "os_spec") == [1]
    assert await sweep()(db) == {}
    assert await sweep()(db, repair=True) == {}


async def test_sweep_owned_repair_leaves_a_row_that_got_a_live_owner_after_the_report(env):
    db = env.db
    await seed(env, specs=(1,))
    await db.execute(f"INSERT INTO os_guitar (id, spec_id, deleted) VALUES (1, 1, {T1})")
    assert await sweep()(db) == {"Spec": [{"id": 1}]}
    await db.execute("INSERT INTO os_guitar (id, spec_id) VALUES (2, 1)")
    assert await sweep()(db, repair=True) == {}
    assert await archived(db, "os_spec") == []


async def test_sweep_owned_repair_of_a_chain_settles_through_the_triggers_in_one_pass(chain):
    db = chain.db
    await db.execute("INSERT INTO os_detail (id) VALUES (1)")
    await db.execute("INSERT INTO os_basket (id, detail_id) VALUES (1, 1)")
    await db.execute(f"INSERT INTO os_order (id, basket_id, deleted) VALUES (1, 1, {T1})")
    assert await sweep()(db) == {"Basket": [{"id": 1}]}
    assert await sweep()(db, repair=True) == {"Basket": [{"id": 1}]}
    assert await archived(db, "os_basket") == [1]
    assert await archived(db, "os_detail") == [1]
    assert await sweep()(db) == {}


@pytest.mark.parametrize("kind", ["sqlite", "postgres"])
async def test_sweep_owned_repair_needs_two_passes_where_no_trigger_stamps_the_next_level(kind):
    e = await open_db(kind, CHAIN.replace(SOFT, '@@soft_delete.deleted_at("deleted", mode: "application")'))
    try:
        db = e.db
        await db.execute("INSERT INTO os_detail (id) VALUES (1)")
        await db.execute("INSERT INTO os_basket (id, detail_id) VALUES (1, 1)")
        await db.execute(f"INSERT INTO os_order (id, basket_id, deleted) VALUES (1, 1, {T1})")
        assert await sweep()(db) == {"Basket": [{"id": 1}]}
        assert await sweep()(db, repair=True) == {"Basket": [{"id": 1}], "Detail": [{"id": 1}]}
        assert await archived(db, "os_detail") == [1]
        assert await sweep()(db) == {}
    finally:
        await close(e)


async def test_sweep_owned_models_filter_and_a_model_that_is_no_target(chain):
    db = chain.db
    await db.execute("INSERT INTO os_detail (id) VALUES (1)")
    await db.execute("INSERT INTO os_basket (id, detail_id) VALUES (1, 1)")
    await db.execute(f"INSERT INTO os_order (id, basket_id, deleted) VALUES (1, 1, {T1})")
    assert await sweep()(db, models=["Detail"]) == {}
    assert await sweep()(db, models=[chain.m["Basket"]]) == {"Basket": [{"id": 1}]}
    with pytest.raises(ValueError, match="Order"):
        await sweep()(db, models=["Order"])


async def test_sweep_owned_refuses_a_role_that_is_not_a_superuser_or_a_bypass_role():
    header = 'datasource db {\n  provider = "postgresql"\n  bypass_roles = ["os_bypass"]\n}\n'
    e = await open_db("postgres", SCHEMA, header)
    db = e.db
    try:
        await db.execute("DROP ROLE IF EXISTS os_plain")
        await db.execute("DROP ROLE IF EXISTS os_member")
        await db.execute("DROP ROLE IF EXISTS os_bypass")
        await db.execute("CREATE ROLE os_plain NOLOGIN")
        await db.execute("CREATE ROLE os_bypass NOLOGIN")
        await db.execute("CREATE ROLE os_member NOLOGIN IN ROLE os_bypass")
        await db.execute("GRANT ALL ON ALL TABLES IN SCHEMA public TO os_bypass")
        await seed(e, specs=(1,))
        await db.execute(f"INSERT INTO os_guitar (id, spec_id, deleted) VALUES (1, 1, {T1})")
        async with db.transaction():
            await db.execute("SET LOCAL ROLE os_plain")
            with pytest.raises(PermissionError, match="superuser or a role of bypass_roles"):
                await sweep()(db)
        async with db.transaction():
            await db.execute("SET LOCAL ROLE os_member")
            assert await sweep()(db) == {"Spec": [{"id": 1}]}
    finally:
        await db.execute("DROP OWNED BY os_plain, os_member, os_bypass")
        await db.execute("DROP ROLE IF EXISTS os_plain, os_member, os_bypass")
        await close(e)


async def test_the_revive_cascade_undeletes_the_children_with_the_stamp_of_the_parent_and_skips_a_kept_child(shelf):
    db, m = shelf.db, shelf.m
    await m["Shelf"].objects.using(db).insert_many([{"id": 1}])
    await m["Book"].objects.using(db).insert_many([{"id": 1, "shelf_id": 1}, {"id": 2, "shelf_id": 1}])
    await m["Memo"].objects.using(db).insert_many([{"id": 1, "shelf_id": 1}])
    await db.execute(f"UPDATE os_book SET deleted = {T1} WHERE id = 2")
    await db.execute(f"UPDATE os_shelf SET deleted = {T2} WHERE id = 1")
    assert await archived(db, "os_book") == [1, 2]
    assert await archived(db, "os_memo") == [], "the cascade leaves the kept child"
    assert await stamp(db, "os_book", 2) != await stamp(db, "os_shelf", 1), "a row that is deleted already keeps its own stamp"
    await db.execute("UPDATE os_shelf SET deleted = NULL WHERE id = 1")
    assert await archived(db, "os_shelf") == []
    assert await archived(db, "os_book") == [2], "only the row that the parent stamped comes back"
    assert await archived(db, "os_memo") == []


async def test_one_owner_with_two_owning_columns_sweeps_and_revives_both_targets(multi):
    db, m = multi.db, multi.m
    await m["Cover"].objects.using(db).insert_many([{"id": 1}, {"id": 2}])
    await m["Stand"].objects.using(db).insert_many([{"id": 1}])
    await m["Kit"].objects.using(db).insert_many([{"id": 1, "cover_id": 1, "stand_id": 1}, {"id": 2, "cover_id": 2, "stand_id": None}, {"id": 3, "cover_id": 2, "stand_id": None}])
    await delete(multi, "Kit", 1, 2)
    assert await archived(db, "os_cover") == [1], "cover 2 keeps its live owner"
    assert await archived(db, "os_stand") == [1]
    assert await stamp(db, "os_cover", 1) == await stamp(db, "os_kit", 1) == await stamp(db, "os_stand", 1)
    await delete(multi, "Kit", 3)
    assert await archived(db, "os_cover") == [1, 2]
    await undelete(multi, "Kit", 1)
    assert await archived(db, "os_cover") == [2]
    assert await archived(db, "os_stand") == []
    await db.execute("UPDATE os_kit SET deleted = NULL WHERE id = 3")
    assert await archived(db, "os_cover") == [], "the only live owner revives the cover whatever its stamp"


async def test_sweep_owned_repair_raises_at_the_bound_and_rolls_every_pass_back():
    e = await open_db("postgres", NODE)
    try:
        db = e.db
        await db.execute("INSERT INTO os_node (id, parent_id) VALUES (4, NULL), (3, 4), (2, 3)")
        await db.execute(f"INSERT INTO os_node (id, parent_id, deleted) VALUES (1, 2, {T1})")
        assert await sweep()(db) == {"Node": [{"id": 2}]}
        with pytest.raises(RuntimeError, match="did not settle"):
            await sweep()(db, repair=True)
        assert await archived(db, "os_node") == [1], "the first pass is rolled back with the others"
    finally:
        await close(e)
