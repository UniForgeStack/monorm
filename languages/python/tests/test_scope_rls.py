"""`@@scope.by` in the database: row-level security on PostgreSQL, tested as roles that are not superusers."""

import os
import subprocess
import sys
import uuid
from urllib.parse import urlsplit, urlunsplit

import pytest

import monorm
from artifact_caps import has_extension

pytestmark = pytest.mark.skipif(not has_extension("monorm.scope"), reason="requires the scope package")

SCHEMA = """
datasource db {{
 provider = "postgresql"
 bypass_roles = ["{admin}"]
 exempt_roles = ["{auditor}"]
}}
model Order {{
 id Int @id @default(autoincrement())
 shop_id Int
 total Int
 @@scope.by(shop: shop_id)
 @@map("rls_orders")
}}
"""


def url_as(role):
    parts = urlsplit(os.environ["ORM_TEST_DATABASE_URL"])
    host = parts.hostname + (f":{parts.port}" if parts.port else "")
    return urlunsplit((parts.scheme, f"{role}:pw@{host}", parts.path, parts.query, parts.fragment))


@pytest.fixture
async def world():
    if not os.environ.get("ORM_TEST_DATABASE_URL"):
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    tag = uuid.uuid4().hex[:8]
    names = {k: f"rls_{k}_{tag}" for k in ("app", "admin", "boss", "auditor")}
    registry = monorm.Registry()
    models = monorm.loads(SCHEMA.format(**names), registry=registry)
    owner = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=registry, default=False)
    await owner.drop_tables()
    for role in names.values():
        await owner.execute(f"DROP ROLE IF EXISTS {role}")
        await owner.execute(f"CREATE ROLE {role} LOGIN PASSWORD 'pw'")
    await owner.execute(f"GRANT {names['admin']} TO {names['boss']}")
    await owner.create_tables()
    for role in names.values():
        await owner.execute(f"GRANT ALL ON ALL TABLES IN SCHEMA public TO {role}")
        await owner.execute(f"GRANT ALL ON ALL SEQUENCES IN SCHEMA public TO {role}")
    opened = []

    async def connect_as(kind):
        db = await monorm.connect(url_as(names[kind]), registry=registry, default=False)
        opened.append(db)
        return db

    try:
        yield owner, connect_as, models["Order"], names
    finally:
        for db in opened:
            await db.close()
        await owner.drop_tables()
        for role in names.values():
            await owner.execute(f"DROP OWNED BY {role}")
            await owner.execute(f"DROP ROLE {role}")
        await owner.close()


class Raw:
    """Raw SQL runs with no scope of its own, so the helper sets the setting for one transaction."""

    def __init__(self, db, shop):
        self.db, self.shop, self.tx = db, shop, None

    async def __aenter__(self):
        self.tx = self.db.transaction()
        await self.tx.__aenter__()
        await self.db.execute(f"SELECT set_config('scope.shop', '{self.shop}', true)")
        return self.db

    async def __aexit__(self, *exc):
        return await self.tx.__aexit__(*exc)


async def seed(owner, Order):
    with monorm.scope(shop=1):
        await Order.objects.using(owner).insert_many([{"total": 10}, {"total": 11}])
    with monorm.scope(shop=2):
        await Order.objects.using(owner).insert(total=20)


async def test_the_policy_and_the_flags_are_on_the_table(world):
    owner, _, _, names = world
    flags = await owner.fetch("SELECT relrowsecurity AS on, relforcerowsecurity AS forced FROM pg_class WHERE relname = 'rls_orders'")
    assert (flags[0]["on"], flags[0]["forced"]) == (True, True)
    policies = {r["policyname"] for r in await owner.fetch("SELECT policyname FROM pg_policies WHERE tablename = 'rls_orders'")}
    assert policies == {"monorm_scope", f"monorm_scope_exempt_{names['auditor']}"}


async def test_a_role_with_no_scope_sees_and_writes_nothing(world):
    owner, connect_as, Order, _ = world
    await seed(owner, Order)
    app = await connect_as("app")
    assert await app.fetch("SELECT * FROM rls_orders") == []
    with pytest.raises(Exception, match="row-level security|null value"):
        await app.execute("INSERT INTO rls_orders (shop_id, total) VALUES (1, 1)")


async def test_a_scope_shows_its_rows_and_the_orm_equals_a_raw_read(world):
    owner, connect_as, Order, _ = world
    await seed(owner, Order)
    app = await connect_as("app")
    with monorm.scope(shop=1):
        orm = sorted(r.total for r in await Order.objects.using(app).all())
    async with Raw(app, 1):
        raw = sorted(r["total"] for r in await app.fetch("SELECT total FROM rls_orders"))
    assert orm == raw == [10, 11]
    with monorm.scope(shop=2):
        assert [r.total for r in await Order.objects.using(app).all()] == [20]


async def test_the_database_refuses_a_write_for_another_scope(world):
    owner, connect_as, Order, _ = world
    app = await connect_as("app")
    async with Raw(app, 1):
        await app.execute("INSERT INTO rls_orders (shop_id, total) VALUES (1, 1)")
    for sql in ("UPDATE rls_orders SET shop_id = 2", "INSERT INTO rls_orders (shop_id, total) VALUES (2, 1)"):
        with pytest.raises(Exception, match="row-level security"):
            async with Raw(app, 1):
                await app.execute(sql)


async def test_the_fill_trigger_takes_a_null_column_from_the_setting(world):
    owner, connect_as, _, _ = world
    app = await connect_as("app")
    async with Raw(app, 7):
        await app.execute("INSERT INTO rls_orders (total) VALUES (1)")
    assert [r["shop_id"] for r in await owner.fetch("SELECT shop_id FROM rls_orders")] == [7]


async def test_unscoped_opens_the_rows_only_for_a_bypass_role(world):
    owner, connect_as, Order, _ = world
    await seed(owner, Order)
    app, boss = await connect_as("app"), await connect_as("boss")
    with monorm.unscoped("shop"):
        assert await Order.objects.using(app).without_defaults().count() == 0
        assert await Order.objects.using(boss).without_defaults().count() == 3
    assert await Order.objects.using(boss).without_defaults().count() == 0


async def test_an_exempt_role_reads_every_row_and_writes_none(world):
    owner, connect_as, Order, _ = world
    await seed(owner, Order)
    auditor = await connect_as("auditor")
    assert len(await auditor.fetch("SELECT * FROM rls_orders")) == 3
    with pytest.raises(Exception, match="row-level security"):
        await auditor.execute("INSERT INTO rls_orders (shop_id, total) VALUES (1, 1)")


async def test_the_setting_read_is_one_initplan(world):
    owner, connect_as, _, _ = world
    app = await connect_as("app")
    async with Raw(app, 1):
        plan = "\n".join(r["QUERY PLAN"] for r in await app.fetch("EXPLAIN SELECT * FROM rls_orders"))
    assert "InitPlan" in plan, plan
    assert "current_setting" not in plan.split("InitPlan")[0], plan


async def test_audit_names_a_dropped_policy_and_a_forced_flag_that_is_off(world, tmp_path):
    owner, _, _, names = world
    (tmp_path / "schema.prisma").write_text(SCHEMA.format(**names))
    env = {**os.environ, "MONORM_DATABASE_URL": os.environ["ORM_TEST_DATABASE_URL"]}

    def audit():
        return subprocess.run([sys.executable, "-m", "monorm", "--schema", "schema.prisma", "audit"], cwd=tmp_path, env=env, capture_output=True, text=True)

    same = audit()
    assert same.returncode == 0, same.stdout + same.stderr
    await owner.execute(f"DROP POLICY monorm_scope_exempt_{names['auditor']} ON rls_orders")
    await owner.execute("ALTER TABLE rls_orders NO FORCE ROW LEVEL SECURITY")
    drift = audit()
    assert drift.returncode == 1, drift.stdout + drift.stderr
    lines = [line.strip() for line in drift.stdout.splitlines()]
    assert f"- create policy monorm_scope_exempt_{names['auditor']} on rls_orders" in lines, drift.stdout
    assert "- enable row level security on rls_orders, forced" in lines, drift.stdout
    assert "superuser" in drift.stdout
