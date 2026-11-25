"""`hard_delete_setting`: soft delete next to another ORM that keeps a rule and its own triggers.

The fixture table `sd_pick` uses the SQL of the other ORM: a rule that turns a `DELETE` into an `UPDATE`
unless `rules.hard_deletion` is `'on'`, and a trigger that cascades the stamp to its child table."""

import json
import os
import subprocess
import sys
import uuid
from urllib.parse import urlsplit, urlunsplit

import pytest

import monorm
from artifact_caps import has_extension

pytestmark = pytest.mark.skipif(not has_extension("monorm.soft-delete"), reason="requires the soft-delete package")

SCHEMA = """
datasource db {{
 provider = "postgresql"
{key}}}
model Spec {{
 id Int @id @default(autoincrement())
 name String
 guitars Guitar[]
 wires Wire[]
 @@soft_delete.deleted_at
 @@map("sd_spec")
}}
model Guitar {{
 id Int @id @default(autoincrement())
 spec_id Int
 spec Spec @relation(fields: [spec_id], references: [id], onDelete: Cascade)
 straps Strap[]
 @@soft_delete.deleted_at
 @@map("sd_guitar")
}}
model Strap {{
 id Int @id @default(autoincrement())
 guitar_id Int
 guitar Guitar @relation(fields: [guitar_id], references: [id], onDelete: Cascade)
 @@soft_delete.deleted_at
 @@map("sd_strap")
}}
model Wire {{
 id Int @id @default(autoincrement())
 spec_id Int
 spec Spec @relation(fields: [spec_id], references: [id], onDelete: Cascade) @soft_delete.keep_child_rows
 @@map("sd_wire")
}}
model PickItem {{
 id Int @id
 pick_id Int
 @@managed(false)
 @@map("sd_pick_item")
}}
{pick}"""

# Trusted only with the key: it adds no object to a table that another service owns.
PICK = """model Pick {
 id Int @id
 name String
 @@managed(false)
 @@soft_delete.deleted_at
 @@map("sd_pick")
}
"""

KEY = ' hard_delete_setting = "rules.hard_deletion"\n'

# What the other ORM keeps in its own migrations: a table, a rule and a trigger that cascades the stamp.
PICKS = """
CREATE TABLE sd_pick (id integer PRIMARY KEY, name text NOT NULL, deleted_at timestamptz);
CREATE TABLE sd_pick_item (id serial PRIMARY KEY, pick_id integer NOT NULL REFERENCES sd_pick(id), deleted_at timestamptz);
CREATE RULE sd_pick_soft AS ON DELETE TO sd_pick
  WHERE coalesce(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD UPDATE sd_pick SET deleted_at = now() WHERE id = OLD.id;
CREATE FUNCTION sd_pick_cascade() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  UPDATE sd_pick_item SET deleted_at = NEW.deleted_at WHERE pick_id = NEW.id AND deleted_at IS NULL;
  RETURN NULL;
END $$;
CREATE TRIGGER sd_pick_cascade AFTER UPDATE OF deleted_at ON sd_pick FOR EACH ROW
  WHEN (OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL) EXECUTE FUNCTION sd_pick_cascade();
"""
DROP_PICKS = "DROP TABLE IF EXISTS sd_pick_item; DROP TABLE IF EXISTS sd_pick CASCADE; DROP FUNCTION IF EXISTS sd_pick_cascade()"

# The same rule on a MonOrm table (P4), and a rule with no condition (P5).
SPEC_RULE = """
CREATE RULE sd_spec_soft AS ON DELETE TO sd_spec
  WHERE coalesce(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD UPDATE sd_spec SET deleted_at = now() WHERE id = OLD.id AND deleted_at IS NULL
"""
ALWAYS_RULE = "CREATE RULE sd_pick_always AS ON DELETE TO sd_pick DO INSTEAD UPDATE sd_pick SET deleted_at = now() WHERE id = OLD.id"


def schema(key):
    return SCHEMA.format(key=key, pick=PICK if key else "")


async def build(key):
    if not os.environ.get("ORM_TEST_DATABASE_URL"):
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    source = schema(key)
    registry = monorm.Registry()
    models = monorm.loads(source, registry=registry)
    db = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=registry, default=False)
    await db.execute(DROP_PICKS)
    await db.drop_tables()
    await db.create_tables()
    await db.execute(PICKS)

    async def close():
        await db.execute(DROP_PICKS)
        await db.drop_tables()
        await db.close()

    return db, models, close


@pytest.fixture
async def world():
    db, models, close = await build(KEY)
    try:
        yield db, models
    finally:
        await close()


@pytest.fixture
async def plain():
    db, models, close = await build("")
    try:
        yield db, models
    finally:
        await close()


async def alive(db, table):
    return sorted(r["id"] for r in await db.fetch(f"SELECT id FROM {table} ORDER BY id"))


async def stamped(db, table):
    return sorted(r["id"] for r in await db.fetch(f"SELECT id FROM {table} WHERE deleted_at IS NOT NULL ORDER BY id"))


async def seed(db, models):
    await models["Spec"].objects.using(db).insert_many([{"id": i, "name": f"s{i}"} for i in (1, 2, 3)])
    await models["Guitar"].objects.using(db).insert_many([{"spec_id": 1}, {"spec_id": 1}, {"spec_id": 2}])
    await models["Wire"].objects.using(db).insert_many([{"spec_id": 1}, {"spec_id": 2}])
    await db.execute("INSERT INTO sd_pick (id, name) VALUES (1, 'strat'), (2, 'tele'); INSERT INTO sd_pick_item (pick_id) VALUES (1), (2)")


async def test_hard_delete_removes_a_live_row_and_the_next_delete_in_the_transaction_soft_deletes(world):
    db, models = world
    await seed(db, models)
    specs = models["Spec"].objects.using(db)
    events = []
    db.on_query(events.append)
    async with db.transaction():
        assert await specs.filter(models["Spec"].id == 1).hard_delete() == 1
        assert await alive(db, "sd_spec") == [2, 3]
        assert await specs.filter(models["Spec"].id == 2).delete() == 1
        await db.execute("DELETE FROM sd_spec WHERE id = 3")
        assert await alive(db, "sd_spec") == [2, 3]
        assert await stamped(db, "sd_spec") == [2, 3]
    assert events[0].settings.get("rules.hard_deletion") == "on"
    assert all(e.settings.get("rules.hard_deletion") != "on" for e in events[1:])
    # a plain delete outside a transaction never carries the setting
    assert await db.fetch("SELECT current_setting('rules.hard_deletion', true) AS s") in ([{"s": None}], [{"s": ""}])


async def test_a_hard_delete_removes_the_live_rows_of_an_instance_and_of_a_held_statement(world):
    db, models = world
    await seed(db, models)
    Spec = models["Spec"]
    first = await Spec.objects.using(db).filter(Spec.id == 1).first()
    await first.hard_delete()
    assert await alive(db, "sd_spec") == [2, 3]
    held = Spec.objects.using(db).filter(Spec.id == monorm.param("id")).hard_delete().prepare()
    assert await held(id=2) == 1
    assert await alive(db, "sd_spec") == [3]
    await Spec.objects.using(db).filter(Spec.id == 3).delete()
    assert await stamped(db, "sd_spec") == [3]


async def test_a_raw_hard_deletion_setting_removes_a_live_parent_and_its_cascade_children(world):
    db, models = world
    await seed(db, models)
    async with db.transaction():
        await db.execute("SELECT set_config('rules.hard_deletion', 'on', true)")
        await db.execute("DELETE FROM sd_spec WHERE id = 1")
    assert await alive(db, "sd_spec") == [2, 3]
    assert await alive(db, "sd_wire") == [2]
    assert [r["spec_id"] for r in await db.fetch("SELECT spec_id FROM sd_guitar")] == [2]
    assert await stamped(db, "sd_guitar") == []


async def test_without_the_key_a_hard_delete_of_a_live_row_soft_deletes_it(plain):
    db, models = plain
    await seed(db, models)
    assert await models["Spec"].objects.using(db).filter(models["Spec"].id == 1).hard_delete() == 0
    assert await alive(db, "sd_spec") == [1, 2, 3]
    assert await stamped(db, "sd_spec") == [1]
    assert await stamped(db, "sd_guitar") == [1, 2]
    defs = " ".join(r["d"] for r in await db.fetch("SELECT pg_get_triggerdef(oid) AS d FROM pg_trigger WHERE tgrelid = 'sd_spec'::regclass AND NOT tgisinternal"))
    assert "hard_deletion" not in defs and "soft_delete" in defs


async def test_with_the_key_the_delete_and_cascade_triggers_read_the_setting(world):
    db, _ = world
    names = {}
    for table in ("sd_spec", "sd_guitar"):
        defs = await db.fetch(f"SELECT tgname AS n, pg_get_triggerdef(oid) AS d FROM pg_trigger WHERE tgrelid = '{table}'::regclass AND NOT tgisinternal")
        names[table] = {r["n"]: r["d"] for r in defs}
    for table, triggers in names.items():
        skipping = [n for n, d in triggers.items() if "IS DISTINCT FROM 'on'" in d]
        assert skipping, (table, triggers)
    assert any("soft_delete_guard" in n for n in names["sd_guitar"])
    guards = {n: d for n, d in names["sd_guitar"].items() if "guard" in n}
    assert "soft_delete_guard_spec_cascade" in guards
    assert all("IS DISTINCT FROM 'on'" in d for d in guards.values())


async def test_a_moved_and_deleted_row_archives_its_children_only_while_the_setting_is_off(world):
    db, models = world
    await seed(db, models)
    await models["Strap"].objects.using(db).insert_many([{"guitar_id": 1}, {"guitar_id": 2}, {"guitar_id": 3}])
    move = "UPDATE sd_guitar SET spec_id = 3, deleted_at = now() WHERE id = {}"
    async with db.transaction():
        await db.execute("SELECT set_config('rules.hard_deletion', 'on', true)")
        await db.execute(move.format(1))
    # The cascade and its copy for a moved row both skip, so the strap stays live.
    assert await stamped(db, "sd_guitar") == [1]
    assert await stamped(db, "sd_strap") == []
    await db.execute(move.format(2))
    assert await stamped(db, "sd_strap") == [2]


async def test_a_rule_of_the_other_orm_on_a_monorm_table_lets_a_hard_delete_remove_the_row(world):
    db, models = world
    await seed(db, models)
    await db.execute(SPEC_RULE)
    assert await models["Spec"].objects.using(db).filter(models["Spec"].id == 1).hard_delete() >= 0
    assert await alive(db, "sd_spec") == [2, 3]
    await models["Spec"].objects.using(db).filter(models["Spec"].id == 2).delete()
    assert await alive(db, "sd_spec") == [2, 3]
    assert await stamped(db, "sd_spec") == [2]


async def test_the_trigger_of_monorm_breaks_the_hard_delete_path_of_the_rule_when_there_is_no_key(plain):
    db, models = plain
    await seed(db, models)
    await db.execute(SPEC_RULE)
    async with db.transaction():
        await db.execute("SELECT set_config('rules.hard_deletion', 'on', true)")
        await db.execute("DELETE FROM sd_spec WHERE id = 1")
    assert await alive(db, "sd_spec") == [1, 2, 3]
    assert await stamped(db, "sd_spec") == [1]


async def test_an_unmanaged_table_with_a_rule_is_soft_deleted_by_the_rule_and_hard_deleted_with_the_setting(world):
    db, models = world
    await seed(db, models)
    picks = models["Pick"].objects.using(db)
    Pick = models["Pick"]
    # a model with no trigger of MonOrm soft-deletes in the engine, and the trigger of the other ORM cascades the stamp
    assert await picks.filter(Pick.id == 1).delete() == 1
    assert await alive(db, "sd_pick") == [1, 2]
    assert await stamped(db, "sd_pick") == [1]
    assert await stamped(db, "sd_pick_item") == [1]
    await db.execute("DELETE FROM sd_pick_item")
    assert await picks.filter(Pick.id == 1).hard_delete() == 1
    assert await alive(db, "sd_pick") == [2]
    # a live row is removed too: the setting turns the rule off for this statement
    assert await picks.filter(Pick.id == 2).hard_delete() == 1
    assert await alive(db, "sd_pick") == []


async def test_a_raw_delete_under_the_rule_reports_no_row_and_soft_deletes_the_row(world):
    db, models = world
    await seed(db, models)
    assert await db.execute("DELETE FROM sd_pick WHERE id = 2") == 0
    assert await stamped(db, "sd_pick") == [2]
    assert await stamped(db, "sd_pick_item") == [2]
    await db.execute("DROP RULE sd_pick_soft ON sd_pick")
    await db.execute(ALWAYS_RULE)
    assert await db.execute("DELETE FROM sd_pick WHERE id = 1") == 0
    assert await stamped(db, "sd_pick") == [1, 2]


async def test_the_guard_of_a_child_skips_when_the_setting_is_on(world):
    db, models = world
    await seed(db, models)
    await db.execute("UPDATE sd_spec SET deleted_at = now() WHERE id = 3")
    await db.execute("INSERT INTO sd_guitar (spec_id) VALUES (3)")
    assert await stamped(db, "sd_guitar") == [4]
    async with db.transaction():
        await db.execute("SELECT set_config('rules.hard_deletion', 'on', true)")
        await db.execute("INSERT INTO sd_guitar (spec_id) VALUES (3)")
        await db.execute("UPDATE sd_guitar SET spec_id = 3 WHERE id = 1")
    assert await stamped(db, "sd_guitar") == [4]


async def test_an_unmanaged_soft_delete_model_needs_the_key_in_the_datasource():
    with pytest.raises(Exception, match=r"Pick.*unmanaged.*`hard_delete_setting`"):
        monorm.loads(SCHEMA.format(key="", pick=PICK), registry=monorm.Registry())
    monorm.loads(schema(KEY), registry=monorm.Registry())


def objects_of(node, table):
    """The trigger and rule operations that name `table`, at any depth."""
    found = []
    if isinstance(node, dict):
        for key, value in node.items():
            if key in ("create_trigger", "create_rule", "create_function") and isinstance(value, dict) and value.get("table") == table:
                found.append(key)
            found += objects_of(value, table)
    elif isinstance(node, list):
        for value in node:
            found += objects_of(value, table)
    return found


def cli(tmp_path, env, *args):
    return subprocess.run([sys.executable, "-m", "monorm", "--schema", "schema.prisma", "--dir", "migrations", *args], cwd=tmp_path, env=env, capture_output=True, text=True)


async def test_an_unmanaged_soft_delete_table_gives_no_operation_no_drift_and_a_clean_audit(tmp_path):
    if not os.environ.get("ORM_TEST_DATABASE_URL"):
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    # drift compares the whole database, so this test uses a database of its own
    name = f"sd_drift_{uuid.uuid4().hex[:8]}"
    admin = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=monorm.Registry(), default=False)
    await admin.execute(f'CREATE DATABASE "{name}"')
    parts = urlsplit(os.environ["ORM_TEST_DATABASE_URL"])
    url = urlunsplit((parts.scheme, parts.netloc, "/" + name, parts.query, parts.fragment))
    source = schema(KEY)
    registry = monorm.Registry()
    monorm.loads(source, registry=registry)
    db = await monorm.connect(url, registry=registry, default=False)
    try:
        await db.create_tables()
        await db.execute(PICKS)
        (tmp_path / "schema.prisma").write_text(source)
        env = {**os.environ, "MONORM_DATABASE_URL": url}
        done = cli(tmp_path, env, "baseline")
        assert done.returncode == 0, done.stdout + done.stderr
        plan = json.loads((tmp_path / "migrations" / "0001_initial.json").read_text())
        assert objects_of(plan, "sd_pick") == []
        again = cli(tmp_path, env, "makemigrations")
        assert "No changes." in again.stdout, again.stdout + again.stderr
        for command in ("drift", "audit"):
            done = cli(tmp_path, env, command)
            assert done.returncode == 0, done.stdout + done.stderr
        # the other ORM may change its own rule and trigger: MonOrm does not compare them
        await db.execute("DROP RULE sd_pick_soft ON sd_pick; DROP TRIGGER sd_pick_cascade ON sd_pick; DROP FUNCTION sd_pick_cascade()")
        for command in ("drift", "audit"):
            assert cli(tmp_path, env, command).returncode == 0
    finally:
        await db.close()
        await admin.execute(f'DROP DATABASE IF EXISTS "{name}" WITH (FORCE)')
        await admin.close()
