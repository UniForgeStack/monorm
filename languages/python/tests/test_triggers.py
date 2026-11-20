"""`@@trigger` with the pgtrigger names: deferred checks, transition tables, change helpers and skippable triggers."""

import os

import pytest

import monorm
from monorm import QueryError

MODEL = """
model TgItem {
  id Int @id @default(autoincrement())
  title String
  stock Int
  updated_at DateTime @default(now())
  version Int @default(0)
  @@map("tg_items")
  @@trigger(bump, when: after, operation: [update], condition: [any_change(title, stock)], func: "%s")
  @@trigger(shout, when: %s, operation: [insert], skippable: true, func: "%s")
}
"""

BODIES = {
    "postgresql": (
        "after",
        "BEGIN UPDATE tg_items SET version = version + 1 WHERE id = NEW.id; RETURN NULL; END;",
        "before",
        "BEGIN NEW.title := upper(NEW.title); RETURN NEW; END;",
    ),
    "sqlite": ("after", "UPDATE tg_items SET version = version + 1 WHERE id = NEW.id", "after", "UPDATE tg_items SET title = upper(title) WHERE id = NEW.id"),
}


def schema(provider):
    _, bump, when, shout = BODIES[provider]
    return MODEL % (bump, when, shout)


POSTGRES_ONLY = """
model TgLog {
  id Int @id @default(autoincrement())
  note String
  @@map("tg_log")
}
model TgStock {
  id Int @id @default(autoincrement())
  stock Int
  @@map("tg_stock")
  @@trigger(nonnegative, when: after, operation: [insert, update], deferrable: deferred, func: "BEGIN IF (SELECT stock FROM tg_stock WHERE id = NEW.id) < 0 THEN RAISE EXCEPTION 'negative stock'; END IF; RETURN NULL; END;")
  @@trigger(batch, referencing(new: n), when: after, operation: [insert], level: statement, func: "BEGIN INSERT INTO tg_log (note) SELECT 'inserted ' || count(*) FROM n; RETURN NULL; END;")
}
"""


def url(provider):
    if provider == "sqlite":
        return "sqlite://:memory:"
    if not (u := os.environ.get("ORM_TEST_DATABASE_URL")):
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    return u


async def open_database(provider, extra="", datasource=""):
    registry = monorm.Registry()
    source = f'datasource db {{\n provider = "{provider}"\n{datasource}}}\n{schema(provider)}{extra}'
    models = monorm.loads(source, registry=registry)
    db = await monorm.connect(url(provider), registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    return db, models


async def close(db):
    await db.drop_tables()
    await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_any_change_fires_and_an_updated_at_only_update_does_not(provider):
    db, m = await open_database(provider)
    try:
        Item = m["TgItem"]
        await Item.objects.using(db).insert(title="A", stock=1)
        await db.execute("UPDATE tg_items SET updated_at = '2001-02-03T00:00:00'")
        assert (await db.fetch("SELECT version FROM tg_items"))[0]["version"] == 0
        await db.execute("UPDATE tg_items SET title = 'b'")
        assert (await db.fetch("SELECT version FROM tg_items"))[0]["version"] == 1
        await db.execute("UPDATE tg_items SET stock = stock")
        assert (await db.fetch("SELECT version FROM tg_items"))[0]["version"] == 1
    finally:
        await close(db)


async def test_a_deferred_check_fails_at_commit():
    db, m = await open_database("postgresql", POSTGRES_ONLY)
    try:
        Stock = m["TgStock"]
        async with db.transaction():
            row = await Stock.objects.using(db).insert(stock=-1)
            await Stock.objects.using(db).filter(Stock.id == row.id).update(stock=5)
        assert (await db.fetch("SELECT stock FROM tg_stock"))[0]["stock"] == 5
        with pytest.raises(monorm.DatabaseError, match="negative stock"):
            async with db.transaction():
                await Stock.objects.using(db).insert(stock=-1)
        assert len(await db.fetch("SELECT id FROM tg_stock")) == 1
    finally:
        await close(db)


async def test_a_statement_trigger_reads_the_transition_table():
    db, m = await open_database("postgresql", POSTGRES_ONLY)
    try:
        await m["TgStock"].objects.using(db).insert_many([{"stock": 1}, {"stock": 2}, {"stock": 3}])
        assert [r["note"] for r in await db.fetch("SELECT note FROM tg_log")] == ["inserted 3"]
    finally:
        await close(db)


async def test_a_skippable_trigger_skips_under_a_bypass_role_and_not_outside_it():
    if not os.environ.get("ORM_TEST_DATABASE_URL"):
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    db, m = await open_database("postgresql", datasource=' bypass_roles = ["postgres"]\n')
    try:
        Item = m["TgItem"]
        await Item.objects.using(db).insert(title="plain", stock=1)
        with monorm.skip_triggers("shout"):
            await Item.objects.using(db).insert(title="skipped", stock=1)
        await db.execute("DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'monorm_tg_plain') THEN CREATE ROLE monorm_tg_plain; END IF; END $$")
        await db.execute("GRANT ALL ON ALL TABLES IN SCHEMA public TO monorm_tg_plain; GRANT ALL ON ALL SEQUENCES IN SCHEMA public TO monorm_tg_plain")
        async with db.transaction():
            await db.execute("SET LOCAL SESSION AUTHORIZATION monorm_tg_plain")
            with monorm.skip_triggers("shout"):
                await Item.objects.using(db).insert(title="refused", stock=1)
        titles = [r["title"] for r in await db.fetch("SELECT title FROM tg_items ORDER BY id")]
        assert titles == ["PLAIN", "skipped", "REFUSED"]
    finally:
        await close(db)


async def test_a_skippable_trigger_without_bypass_roles_cannot_be_skipped():
    db, m = await open_database("postgresql")
    try:
        with monorm.skip_triggers("shout"):
            await m["TgItem"].objects.using(db).insert(title="held", stock=1)
        assert [r["title"] for r in await db.fetch("SELECT title FROM tg_items")] == ["HELD"]
    finally:
        await close(db)


async def test_a_skip_block_raises_on_sqlite_and_referencing_is_refused():
    db, m = await open_database("sqlite")
    try:
        with pytest.raises(QueryError, match="sqlite has none"):
            with monorm.skip_triggers("shout"):
                await m["TgItem"].objects.using(db).insert(title="x", stock=1)
    finally:
        await close(db)
    source = (
        'datasource db { provider = "sqlite" }\n'
        'model T { id Int @id\n @@trigger(t, referencing(new: n), when: after, operation: [insert], func: "SELECT 1") }'
    )
    with pytest.raises(Exception, match="referencing"):
        monorm.loads(source, registry=monorm.Registry())


def audit(path):
    import subprocess
    import sys

    env = {**os.environ, "MONORM_DATABASE_URL": os.environ["ORM_TEST_DATABASE_URL"]}
    return subprocess.run([sys.executable, "-m", "monorm", "--schema", "schema.prisma", "audit"], cwd=path, env=env, capture_output=True, text=True)


async def test_audit_lists_an_unknown_trigger_and_fails_on_a_disabled_one(tmp_path):
    if not os.environ.get("ORM_TEST_DATABASE_URL"):
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    db, _ = await open_database("postgresql")
    try:
        (tmp_path / "schema.prisma").write_text(f'datasource db {{\n provider = "postgresql"\n}}\n{schema("postgresql")}')
        await db.execute("CREATE FUNCTION tg_foreign() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$")
        await db.execute("CREATE TRIGGER tg_foreign_trigger BEFORE UPDATE ON tg_items FOR EACH ROW EXECUTE FUNCTION tg_foreign()")
        listed = audit(tmp_path)
        assert listed.returncode == 0, listed.stdout + listed.stderr
        assert "tg_items.bump: installed" in listed.stdout and "tg_items.tg_foreign_trigger: unknown" in listed.stdout, listed.stdout
        await db.execute("ALTER TABLE tg_items DISABLE TRIGGER bump")
        off = audit(tmp_path)
        assert off.returncode == 1 and "tg_items.bump: disabled" in off.stdout, off.stdout
    finally:
        await db.execute("DROP TABLE IF EXISTS tg_items CASCADE; DROP FUNCTION IF EXISTS tg_foreign()")
        await close(db)
