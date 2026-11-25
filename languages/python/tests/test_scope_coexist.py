"""`@@scope.by` next to another ORM: a prefix, a bypass value, the role check flag and unmanaged tables.

The fixture tables use the SQL of the other ORM: a `tenant_scope` policy that reads `tenant.shop` and
`tenant.bypass = 'on'`, and a trigger that fills the shop. They run as roles that are not superusers."""

import json
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
 scope_prefix = "tenant"
 scope_bypass = {{ setting = "tenant.bypass", value = "on" }}
 bypass_roles = ["{admin}"]
 scope_role_check = {check}
}}
model Order {{
 id Int @id @default(autoincrement())
 shop_id Int
 total Int
 @@scope.by(shop: shop_id)
 @@map("co_orders")
}}
model Note {{
 id Int @id @default(autoincrement())
 user_id Int
 @@scope.by(user: user_id)
 @@map("co_notes")
}}
model Guitar {{
 id Int @id
 shop_id Int
 name String
 @@managed(false)
 @@scope.by(shop: shop_id)
 @@map("co_guitars")
}}
"""

# What the other ORM keeps in its own migrations: a table, a policy and a fill trigger.
GUITARS = """
CREATE TABLE co_guitars (id serial PRIMARY KEY, shop_id integer NOT NULL, name text NOT NULL);
ALTER TABLE co_guitars ENABLE ROW LEVEL SECURITY;
ALTER TABLE co_guitars FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_scope ON co_guitars USING (
  current_setting('tenant.bypass', true) = 'on'
  OR shop_id::text = ANY(string_to_array(current_setting('tenant.shop', true), ','))
);
CREATE FUNCTION co_guitars_fill() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.shop_id IS NULL THEN NEW.shop_id := NULLIF(current_setting('tenant.shop', true), '')::integer; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER co_guitars_fill BEFORE INSERT ON co_guitars FOR EACH ROW EXECUTE FUNCTION co_guitars_fill();
"""


def url_as(role):
    parts = urlsplit(os.environ["ORM_TEST_DATABASE_URL"])
    host = parts.hostname + (f":{parts.port}" if parts.port else "")
    return urlunsplit((parts.scheme, f"{role}:pw@{host}", parts.path, parts.query, parts.fragment))


async def build(check):
    if not os.environ.get("ORM_TEST_DATABASE_URL"):
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    tag = uuid.uuid4().hex[:8]
    names = {k: f"co_{k}_{tag}" for k in ("app", "admin", "boss")}
    source = SCHEMA.format(check=check, **names)
    registry = monorm.Registry()
    models = monorm.loads(source, registry=registry)
    owner = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=registry, default=False)
    await owner.execute("DROP TABLE IF EXISTS co_guitars; DROP FUNCTION IF EXISTS co_guitars_fill()")
    await owner.drop_tables()
    for role in names.values():
        await owner.execute(f"DROP ROLE IF EXISTS {role}")
        await owner.execute(f"CREATE ROLE {role} LOGIN PASSWORD 'pw'")
    await owner.execute(f"GRANT {names['admin']} TO {names['boss']}")
    await owner.create_tables()
    await owner.execute(GUITARS)
    for role in names.values():
        await owner.execute(f"GRANT ALL ON ALL TABLES IN SCHEMA public TO {role}")
        await owner.execute(f"GRANT ALL ON ALL SEQUENCES IN SCHEMA public TO {role}")
    opened = []

    async def connect_as(kind):
        db = await monorm.connect(url_as(names[kind]), registry=registry, default=False)
        opened.append(db)
        return db

    async def close():
        for db in opened:
            await db.close()
        await owner.execute("DROP TABLE IF EXISTS co_guitars; DROP FUNCTION IF EXISTS co_guitars_fill()")
        await owner.drop_tables()
        for role in names.values():
            if await owner.fetch(f"SELECT 1 FROM pg_roles WHERE rolname = '{role}'"):
                await owner.execute(f"DROP OWNED BY {role}")
                await owner.execute(f"DROP ROLE {role}")
        await owner.close()

    return owner, connect_as, models, names, source, close


@pytest.fixture
async def world():
    owner, connect_as, models, names, source, close = await build("false")
    try:
        yield owner, connect_as, models, names, source
    finally:
        await close()


@pytest.fixture
async def checked():
    owner, connect_as, models, names, source, close = await build("true")
    try:
        yield owner, connect_as, models, names, source
    finally:
        await close()


class Raw:
    """Raw SQL in a transaction that sets `tenant.shop` by hand, as the other ORM does."""

    def __init__(self, db, shop):
        self.db, self.shop, self.tx = db, shop, None

    async def __aenter__(self):
        self.tx = self.db.transaction()
        await self.tx.__aenter__()
        await self.db.execute(f"SELECT set_config('tenant.shop', '{self.shop}', true)")
        return self.db

    async def __aexit__(self, *exc):
        return await self.tx.__aexit__(*exc)


async def seed(owner, models):
    with monorm.scope(shop=1):
        await models["Order"].objects.using(owner).insert_many([{"total": 10}, {"total": 11}])
    with monorm.scope(shop=2):
        await models["Order"].objects.using(owner).insert(total=20)
    await owner.execute("INSERT INTO co_guitars (shop_id, name) VALUES (1, 'strat'), (2, 'tele'), (3, 'les paul')")


async def test_a_scope_reads_one_shop_from_a_monorm_table_and_a_fixture_table(world):
    owner, connect_as, models, _, _ = world
    await seed(owner, models)
    app = await connect_as("app")
    with monorm.scope(shop=1):
        assert sorted(o.total for o in await models["Order"].objects.using(app).all()) == [10, 11]
        assert [g.name for g in await models["Guitar"].objects.using(app).all()] == ["strat"]
    with monorm.scope(shop=2):
        assert [o.total for o in await models["Order"].objects.using(app).all()] == [20]
    assert await app.fetch("SELECT * FROM co_orders") == []
    assert await app.fetch("SELECT * FROM co_guitars") == []


async def test_unscoped_with_no_key_opens_every_key_with_the_bypass_value(world):
    owner, connect_as, models, _, _ = world
    await seed(owner, models)
    app = await connect_as("app")
    events = []
    app.on_query(events.append)
    with monorm.unscoped():
        assert await models["Order"].objects.using(app).without_defaults().count() == 3
        assert await models["Guitar"].objects.using(app).without_defaults().count() == 3
    assert events[0].settings.get("tenant.bypass") == "on"
    assert "monorm.bypass" not in events[0].settings


async def test_the_scope_value_goes_to_the_renamed_setting(world):
    _, connect_as, models, _, _ = world
    app = await connect_as("app")
    events = []
    app.on_query(events.append)
    with monorm.scope(shop=1):
        await models["Order"].objects.using(app).all()
    assert events[0].settings.get("tenant.shop") == "1"
    assert "scope.shop" not in events[0].settings


async def test_a_raw_list_setting_reads_both_shops_from_both_tables(world):
    owner, connect_as, models, _, _ = world
    await seed(owner, models)
    app = await connect_as("app")
    async with Raw(app, "1,2"):
        assert sorted(r["total"] for r in await app.fetch("SELECT total FROM co_orders")) == [10, 11, 20]
        assert sorted(r["name"] for r in await app.fetch("SELECT name FROM co_guitars")) == ["strat", "tele"]
    async with Raw(app, "1"):
        assert sorted(r["total"] for r in await app.fetch("SELECT total FROM co_orders")) == [10, 11]


async def test_an_insert_with_no_shop_takes_it_from_the_setting_and_a_list_gives_no_fill(world):
    owner, connect_as, _, _, _ = world
    app = await connect_as("app")
    async with Raw(app, 7):
        await app.execute("INSERT INTO co_orders (total) VALUES (1)")
        await app.execute("INSERT INTO co_guitars (name) VALUES ('jazzmaster')")
    assert [r["shop_id"] for r in await owner.fetch("SELECT shop_id FROM co_orders")] == [7]
    assert [r["shop_id"] for r in await owner.fetch("SELECT shop_id FROM co_guitars")] == [7]
    with pytest.raises(Exception, match="null value|row-level security"):
        async with Raw(app, "7,8"):
            await app.execute("INSERT INTO co_orders (total) VALUES (2)")


async def test_a_scope_list_sends_comma_text_and_reads_every_listed_shop(world):
    owner, connect_as, models, _, _ = world
    await seed(owner, models)
    app = await connect_as("app")
    with monorm.scope(shop=[1, 2]):
        assert sorted(o.total for o in await models["Order"].objects.using(app).all()) == [10, 11, 20]
        async with app.transaction():
            assert (await app.fetch("SELECT current_setting('tenant.shop', true) AS s"))[0]["s"] == "1,2"
            assert sorted(r["total"] for r in await app.fetch("SELECT total FROM co_orders")) == [10, 11, 20]
            assert sorted(r["name"] for r in await app.fetch("SELECT name FROM co_guitars")) == ["strat", "tele"]
    with monorm.scope(shop=(1, 3)):
        assert sorted(o.total for o in await models["Order"].objects.using(app).all()) == [10, 11]


async def test_a_held_read_keeps_one_statement_for_a_value_and_one_for_a_list(world):
    owner, connect_as, models, _, _ = world
    await seed(owner, models)
    app = await connect_as("app")
    q = models["Order"].objects.using(app).order_by(models["Order"].total).prepare()
    with monorm.scope(shop=2):
        assert [o.total for o in await q()] == [20]
    with monorm.scope(shop=[1, 2]):
        assert [o.total for o in await q()] == [10, 11, 20]
    with monorm.scope(shop=1):
        assert [o.total for o in await q()] == [10, 11]


async def test_a_write_under_a_scope_list_needs_a_cell_that_is_a_member(world):
    owner, connect_as, models, _, _ = world
    app = await connect_as("app")
    orders = models["Order"].objects.using(app)
    with monorm.scope(shop=[1, 2]):
        await orders.insert(shop_id=2, total=5)
        await orders.insert_many([{"shop_id": 1, "total": 6}, {"shop_id": 2, "total": 7}])
        with pytest.raises(monorm.ScopeMissing, match="shop"):
            await orders.insert(total=8)
        with pytest.raises(Exception, match="shop"):
            await orders.insert(shop_id=3, total=9)
    assert sorted((r["shop_id"], r["total"]) for r in await owner.fetch("SELECT shop_id, total FROM co_orders")) == [(1, 6), (2, 5), (2, 7)]


async def test_a_scope_list_with_an_empty_list_or_an_item_that_holds_a_comma_raises(world):
    for bad in ([], [""], ["1,2"], [1, None]):
        with pytest.raises(ValueError):
            monorm.scope(shop=bad)


async def test_a_scope_list_raises_at_scope_when_no_datasource_sets_the_prefix():
    registry = monorm.Registry()
    monorm.loads(
        """
        datasource db { provider = "sqlite" }
        model Order {
         id Int @id @default(autoincrement())
         shop_id Int
         @@scope.by(shop: shop_id)
        }
        """,
        registry=registry,
    )
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        monorm.scope(shop=1)
        with pytest.raises(ValueError, match="scope_prefix"):
            monorm.scope(shop=[1, 2])
    finally:
        await db.close()


async def test_two_permissive_policies_are_ored_with_the_policy_of_the_other_orm(world):
    owner, connect_as, models, _, _ = world
    await seed(owner, models)
    await owner.execute("CREATE POLICY tenant_scope ON co_orders USING (shop_id = 2)")
    app = await connect_as("app")
    with monorm.scope(shop=1):
        assert sorted(o.total for o in await models["Order"].objects.using(app).without_defaults()) == [10, 11, 20]
    async with Raw(app, 1):
        assert sorted(r["total"] for r in await app.fetch("SELECT total FROM co_orders")) == [10, 11, 20]
    names = {r["policyname"] for r in await owner.fetch("SELECT policyname FROM pg_policies WHERE tablename = 'co_orders'")}
    assert names == {"monorm_scope", "tenant_scope"}


async def test_unscoped_with_some_keys_raises_with_the_bypass_value(world):
    owner, connect_as, models, _, _ = world
    app = await connect_as("app")
    with monorm.unscoped("shop"):
        with pytest.raises(ValueError, match="opens only some scope keys.*user"):
            await models["Order"].objects.using(app).without_defaults().count()
    with monorm.unscoped("shop", "user"):
        assert await models["Order"].objects.using(app).without_defaults().count() == 0


async def test_with_the_role_check_off_a_role_outside_bypass_roles_opens_the_bypass(world):
    owner, connect_as, models, _, _ = world
    await seed(owner, models)
    app, boss = await connect_as("app"), await connect_as("boss")
    with monorm.unscoped():
        assert await models["Order"].objects.using(app).without_defaults().count() == 3
        assert await models["Order"].objects.using(boss).without_defaults().count() == 3
    assert "pg_roles" not in (await owner.fetch("SELECT pg_get_expr(polqual, polrelid) AS q FROM pg_policy WHERE polname = 'monorm_scope' AND polrelid = 'co_orders'::regclass"))[0]["q"]


async def test_with_the_role_check_on_only_a_member_of_bypass_roles_opens_the_bypass(checked):
    owner, connect_as, models, _, _ = checked
    await seed(owner, models)
    app, boss = await connect_as("app"), await connect_as("boss")
    with monorm.unscoped():
        assert await models["Order"].objects.using(app).without_defaults().count() == 0
        assert await models["Order"].objects.using(boss).without_defaults().count() == 3
    # the policy of the other ORM on its own table is not changed by the flag
    with monorm.unscoped():
        assert await models["Guitar"].objects.using(app).without_defaults().count() == 3


def objects_of(node, table):
    """The policy, trigger and row security operations that name `table`, at any depth."""
    found = []
    if isinstance(node, dict):
        for key, value in node.items():
            if key in ("create_policy", "create_trigger", "set_row_security") and value.get("table") == table:
                found.append(key)
            found += objects_of(value, table)
    elif isinstance(node, list):
        for value in node:
            found += objects_of(value, table)
    return found


def write_schema(tmp_path, source):
    (tmp_path / "schema.prisma").write_text(source)
    return {**os.environ, "MONORM_DATABASE_URL": os.environ["ORM_TEST_DATABASE_URL"]}


def cli(tmp_path, env, *args):
    return subprocess.run([sys.executable, "-m", "monorm", "--schema", "schema.prisma", "--dir", "migrations", *args], cwd=tmp_path, env=env, capture_output=True, text=True)


async def test_an_unmanaged_scoped_table_gives_no_operation_no_drift_and_a_clean_audit(tmp_path):
    if not os.environ.get("ORM_TEST_DATABASE_URL"):
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    # drift compares the whole database, so this test uses a database of its own
    name = f"co_drift_{uuid.uuid4().hex[:8]}"
    admin = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=monorm.Registry(), default=False)
    await admin.execute(f'CREATE DATABASE "{name}"')
    parts = urlsplit(os.environ["ORM_TEST_DATABASE_URL"])
    url = urlunsplit((parts.scheme, parts.netloc, "/" + name, parts.query, parts.fragment))
    source = SCHEMA.format(check="false", admin="postgres")
    registry = monorm.Registry()
    monorm.loads(source, registry=registry)
    db = await monorm.connect(url, registry=registry, default=False)
    try:
        await db.create_tables()
        await db.execute(GUITARS)
        env = write_schema(tmp_path, source)
        env["MONORM_DATABASE_URL"] = url
        done = cli(tmp_path, env, "baseline")
        assert done.returncode == 0, done.stdout + done.stderr
        plan = json.loads((tmp_path / "migrations" / "0001_initial.json").read_text())
        assert objects_of(plan, "co_guitars") == []
        again = cli(tmp_path, env, "makemigrations")
        assert "No changes." in again.stdout, again.stdout + again.stderr
        drift = cli(tmp_path, env, "drift")
        assert drift.returncode == 0, drift.stdout + drift.stderr
        audit = cli(tmp_path, env, "audit")
        assert audit.returncode == 0, audit.stdout + audit.stderr
        # the other ORM may change its own table and policy: MonOrm does not compare them
        await db.execute("DROP POLICY tenant_scope ON co_guitars; DROP FUNCTION co_guitars_fill() CASCADE")
        assert cli(tmp_path, env, "drift").returncode == 0
        assert cli(tmp_path, env, "audit").returncode == 0
    finally:
        await db.close()
        await admin.execute(f'DROP DATABASE IF EXISTS "{name}" WITH (FORCE)')
        await admin.close()


def test_an_unmanaged_scoped_model_needs_a_key_in_the_datasource():
    source = SCHEMA.format(check="false", admin="a").replace(' scope_prefix = "tenant"\n', "").replace(' scope_bypass = { setting = "tenant.bypass", value = "on" }\n', "")
    with pytest.raises(Exception, match=r"Guitar.*unmanaged.*`scope_prefix` or `scope_bypass`"):
        monorm.loads(source, registry=monorm.Registry())
