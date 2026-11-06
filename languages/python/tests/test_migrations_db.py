"""Migrations and extension types against Postgres.

These run in a database of their own: migrations create and drop extensions, which
are per database, and the blog tables of the other tests depend on some of them.
"""

import asyncio
import contextlib
import re
import uuid
from urllib.parse import urlsplit, urlunsplit

import pytest
from conftest import DATABASE_URL
from test_migrations import V1, V2, models

import monorm
from monorm import Registry
from monorm.migrations import MigrationError, Migrations, Migrator


def v1() -> Registry:
    return models(V1)


def v2() -> Registry:
    return models(V2)


_url = urlsplit(DATABASE_URL)
MIGRATIONS_DB = _url.path.lstrip("/") + "_migrations"
MIGRATIONS_URL = urlunsplit(_url._replace(path="/" + MIGRATIONS_DB))


@pytest.fixture(scope="module", autouse=True)
async def migrations_database():
    """Creates the database these tests use, once."""
    try:
        admin = await monorm.connect(DATABASE_URL, max_connections=1, default=False, registry=Registry())
    except monorm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {DATABASE_URL}: {e}")
    try:
        if not await admin._fetch_text(f"SELECT 1 FROM pg_database WHERE datname = '{MIGRATIONS_DB}'"):
            await admin.execute(f'CREATE DATABASE "{MIGRATIONS_DB}"')
    finally:
        await admin.close()


async def connect(reg: Registry) -> monorm.Database:
    try:
        return await monorm.connect(MIGRATIONS_URL, max_connections=2, default=False, registry=reg)
    except monorm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {MIGRATIONS_URL}: {e}")


async def reset(db: monorm.Database) -> None:
    await db.execute(
        "DROP TABLE IF EXISTS books, authors, monorm_migrations CASCADE;DROP FUNCTION IF EXISTS books_touch() CASCADE"
    )


async def scalar(db: monorm.Database, sql: str) -> str | None:
    rows = await db._fetch_text(sql)
    return rows[0][0] if rows else None


@pytest.mark.case("migrations::upgrade_and_downgrade_round_trip")
async def test_upgrade_downgrade_round_trip(tmp_path):
    reg1, reg2 = v1(), v2()
    db = await connect(reg2)
    await reset(db)
    try:
        Migrations(tmp_path, reg1).make()
        migrator = Migrator(db, Migrations(tmp_path, reg2))
        assert [m.name for m in await migrator.upgrade()] == ["0001_initial"]
        await db.execute(
            "INSERT INTO authors (email) VALUES ('Ann@Example.com');"
            "INSERT INTO books (author_id, title) VALUES (1, 'Dune')"
        )

        Migrations(tmp_path, reg2).make("v2")
        assert [s.applied for s in await migrator.status()] == [True, False]
        await migrator.upgrade(allow_destructive=True)  # varchar to citext

        Author, Book = reg2.get("Author"), reg2.get("Book")
        # renamed column kept its data; citext compares case-insensitively
        book = await Book.objects.using(db).get(Book.name == "Dune")
        assert await Author.objects.using(db).filter(Author.email == "ann@example.COM").count() == 1
        # trigger
        before = book.updated_at
        await book.update(pages=10)
        assert book.updated_at > before
        # constraints raise IntegrityError
        with pytest.raises(monorm.IntegrityError):
            await book.update(pages=-1)
        with pytest.raises(monorm.IntegrityError):
            await Author.objects.using(db).insert(email="ANN@example.com")
        with pytest.raises(monorm.IntegrityError):
            await Book.objects.using(db).insert(author_id=1, name="Dune")

        # back to zero and up again: the down migrations are valid DDL too
        assert [m.name for m in await migrator.downgrade(target="zero", allow_destructive=True)] == ["0002_v2", "0001_initial"]
        assert await scalar(db, "SELECT to_regclass('books')::text") is None
        assert len(await migrator.upgrade()) == 2
        assert await migrator.upgrade() == []
        assert [m.name for m in await migrator.downgrade()] == ["0002_v2"]
        assert (
            await scalar(
                db,
                "SELECT data_type::text FROM information_schema.columns "
                "WHERE table_name = 'books' AND column_name = 'title'",
            )
            == "character varying"
        )
    finally:
        await reset(db)
        await db.close()


@pytest.mark.case("migrations::an_edited_applied_migration_is_refused")
async def test_edited_applied_migration_is_refused(tmp_path):
    db = await connect(v1())
    await reset(db)
    try:
        migs = Migrations(tmp_path, v1())
        m = migs.make()
        migrator = Migrator(db, migs)
        await migrator.upgrade()
        (m.path / "up.sql").write_text(m.up_sql + "\n-- edited\n")
        Migrations(tmp_path, v2()).make()
        with pytest.raises(MigrationError, match="changed after it was applied"):
            await migrator.upgrade()
    finally:
        await reset(db)
        await db.close()


@pytest.mark.case("migrations::a_failed_migration_rolls_back")
async def test_failed_migration_rolls_back(tmp_path):
    db = await connect(v1())
    await reset(db)
    try:
        migs = Migrations(tmp_path, v1())
        m = migs.make()
        (m.path / "up.sql").write_text(m.up_sql + "\nSELECT no_such_function();\n")
        with pytest.raises(monorm.DatabaseError):
            await Migrator(db, migs).upgrade()
        assert await scalar(db, "SELECT to_regclass('authors')::text") is None
        assert [s.applied for s in await Migrator(db, migs).status()] == [False]
    finally:
        await reset(db)
        await db.close()


async def test_extension_types_at_runtime():
    reg = models("""
        model Doc {
          id        String                    @id @default(dbgenerated("gen_random_uuid()")) @db.Uuid
          email     String                    @db.Citext
          attrs     Json?
          embedding Unsupported("vector(3)")?

          @@map("ext_docs")
        }
    """)
    Doc = reg.get("Doc")

    db = await connect(reg)
    try:
        await db.execute("DROP TABLE IF EXISTS ext_docs")
        try:
            await db.create_tables()
        except monorm.DatabaseError as e:
            pytest.skip(f"extension not installed: {e}")
        await db.create_tables()  # idempotent
        d = await Doc.objects.using(db).insert(
            email="Bob@X.io", attrs={"tags": ["a", "b"], "n": 1, "ok": True, "x": None}, embedding=[1, 2.5, 3]
        )
        assert isinstance(d.id, uuid.UUID)
        assert d.attrs == {"tags": ["a", "b"], "n": 1, "ok": True, "x": None}
        assert d.embedding == [1, 2.5, 3]
        got = await Doc.objects.using(db).get(Doc.email == "bob@x.IO")
        assert got == d and got.embedding == [1, 2.5, 3]
        assert await Doc.objects.using(db).filter(Doc.id == str(d.id)).exists()
        assert await Doc.objects.using(db).filter(Doc.email.in_(["BOB@x.io"])).count() == 1
        await got.update(embedding=[0, 0, 1], attrs=[1, 2])
        assert got.embedding == [0, 0, 1] and got.attrs == [1, 2]
        await Doc.objects.using(db).insert(email="no@x.io")
        assert await Doc.objects.using(db).filter(Doc.embedding == None).count() == 1  # noqa: E711
    finally:
        await db.execute("DROP TABLE IF EXISTS ext_docs")
        await db.close()


ENUM_V1 = """
enum Status {
  draft
  published
}

model Doc {
  id      BigInt   @id @default(autoincrement())
  status  Status   @default(draft)
  history Status[] @default([draft])

  @@map("docs")
}
"""
# a value added in the middle and one at the end
ENUM_V2 = ENUM_V1.replace("  draft\n  published\n", "  draft\n  review\n  published\n  archived\n")
# `review` removed again: the type is recreated
ENUM_V3 = ENUM_V1.replace("  draft\n  published\n", "  draft\n  published\n  archived\n")


async def test_enum_migrations(tmp_path):
    regs = [models(v) for v in (ENUM_V1, ENUM_V2, ENUM_V3)]
    db = await connect(regs[2])

    async def drop():
        await db.execute("DROP TABLE IF EXISTS docs, monorm_migrations CASCADE; DROP TYPE IF EXISTS status, status_old")

    await drop()
    try:
        for i, reg in enumerate(regs):
            Migrations(tmp_path, reg).make(f"v{i + 1}")
        migrator = Migrator(db, Migrations(tmp_path, regs[2]))
        await migrator.upgrade(target="0002")
        labels = "SELECT string_agg(enumlabel, ',' ORDER BY enumsortorder) FROM pg_enum JOIN pg_type t ON t.oid = enumtypid WHERE typname = 'status'"
        assert await scalar(db, labels) == "draft,review,published,archived"
        await db.execute(
            "INSERT INTO docs (status, history) VALUES ('published', '{draft,published}'), ('archived', DEFAULT)"
        )
        await migrator.upgrade()
        assert await scalar(db, labels) == "draft,published,archived"
        Doc = regs[2].get("Doc")
        Status = regs[2].get_enum("Status")
        docs = await Doc.objects.using(db).order_by(Doc.id)
        assert [(d.status, d.history) for d in docs] == [
            (Status.published, [Status.draft, Status.published]),
            (Status.archived, [Status.draft]),
        ]
        # a row still using a value that a migration removes makes it fail (and roll back)
        await migrator.downgrade()  # back to v2: recreated with review again
        await db.execute("UPDATE docs SET status = 'review' WHERE id = 1")
        with pytest.raises(monorm.DatabaseError, match="invalid input value for enum"):
            await migrator.upgrade()
        assert await scalar(db, labels) == "draft,review,published,archived"
        await db.execute("UPDATE docs SET status = 'draft'")
        await migrator.downgrade(target="zero", allow_destructive=True)
        assert await scalar(db, "SELECT to_regtype('status')::text") is None
    finally:
        await drop()
        await db.close()


GATE_SQLITE = 'datasource db { provider = "sqlite" }\n'
GATE_KEEP = 'model Keep {\n  id BigInt @id @default(autoincrement())\n\n  @@map("keep")\n}\n'
GATE_SHOP = 'model Shop {\n  id   BigInt @id @default(autoincrement())\n  name String\n  note String?\n\n  @@map("shop")\n}\n'
GATE_TABLES = ("shop", "venue", "keep", "ghost_done", "after_tx", "half", "extra", "monorm_migrations")
DIALECTS = ["postgres", "sqlite"]


@contextlib.asynccontextmanager
async def gate_database(dialect):
    """A database, and the schema-file prefix of its dialect."""
    prefix = GATE_SQLITE if dialect == "sqlite" else ""
    reg = models(prefix + GATE_KEEP)
    if dialect == "sqlite":
        db = await monorm.connect("sqlite://:memory:", max_connections=2, default=False, registry=reg)
    else:
        db = await connect(reg)
    drop = "; ".join(f"DROP TABLE IF EXISTS {t}" for t in GATE_TABLES)
    try:
        await db.execute(drop)
        yield db, prefix
    finally:
        await db.execute(drop)
        await db.close()


async def gate_apply(db, tmp_path, source):
    """Writes the next migration for `source` and applies it (no flag: it must not be destructive)."""
    migs = Migrations(tmp_path, models(source))
    migration = migs.make()
    assert migration is not None
    await Migrator(db, migs).upgrade()
    return migration


def gate_next(tmp_path, source):
    """Writes the next migration for `source` without applying it."""
    migs = Migrations(tmp_path, models(source))
    migration = migs.make()
    assert migration is not None
    return migs, migration


async def gate_has_table(db, dialect, table):
    sql = (
        f"SELECT name FROM sqlite_master WHERE name = '{table}'"
        if dialect == "sqlite"
        else f"SELECT tablename FROM pg_tables WHERE tablename = '{table}'"
    )
    return len(await db.fetch(sql)) > 0


def gate_refusal(lead, step, table):
    """The refusal text: what `lead` and `step` do, the table, and the flag."""
    return re.escape(f"{lead} {step}, and {table} has rows; pass --allow-destructive (Python: allow_destructive=True")


async def gate_rows(db, table):
    return len(await db.fetch(f"SELECT * FROM {table}"))


@pytest.mark.case("migrations::destructive_gate_refuses_a_drop_on_a_table_with_rows")
@pytest.mark.parametrize("dialect", DIALECTS)
async def test_destructive_gate_refuses_a_drop_on_a_table_with_rows(tmp_path, dialect):
    async with gate_database(dialect) as (db, prefix):
        await gate_apply(db, tmp_path, prefix + GATE_KEEP + GATE_SHOP)
        await db.execute("INSERT INTO shop (name) VALUES ('a')")
        migs, dropped = gate_next(tmp_path, prefix + GATE_KEEP)
        assert "-- monorm: destructive drop-table shop\n" in dropped.up_sql
        with pytest.raises(MigrationError, match=gate_refusal(dropped.name, "drops table shop", "shop")):
            await Migrator(db, migs).upgrade()
        assert await gate_rows(db, "shop") == 1
        assert [s.applied for s in await Migrator(db, migs).status()] == [True, False]


@pytest.mark.case("migrations::destructive_gate_allows_the_drop_with_the_flag")
@pytest.mark.parametrize("dialect", DIALECTS)
async def test_destructive_gate_allows_the_drop_with_the_flag(tmp_path, dialect):
    async with gate_database(dialect) as (db, prefix):
        await gate_apply(db, tmp_path, prefix + GATE_KEEP + GATE_SHOP)
        await db.execute("INSERT INTO shop (name) VALUES ('a')")
        migs, dropped = gate_next(tmp_path, prefix + GATE_KEEP)
        done = await Migrator(db, migs).upgrade(allow_destructive=True)
        assert [m.name for m in done] == [dropped.name]
        assert not await gate_has_table(db, dialect, "shop")


@pytest.mark.case("migrations::destructive_gate_allows_the_drop_on_an_empty_or_missing_table")
@pytest.mark.parametrize("dialect", DIALECTS)
async def test_destructive_gate_allows_the_drop_on_an_empty_or_missing_table(tmp_path, dialect):
    async with gate_database(dialect) as (db, prefix):
        await gate_apply(db, tmp_path, prefix + GATE_KEEP + GATE_SHOP)
        migs, dropped = gate_next(tmp_path, prefix + GATE_KEEP)
        assert [m.name for m in await Migrator(db, migs).upgrade()] == [dropped.name]  # empty table
        # a marker for a table that does not exist: nothing to lose
        ghost = tmp_path / "0003_ghost"
        ghost.mkdir()
        (ghost / "up.sql").write_text("-- monorm: destructive drop-table ghost\nCREATE TABLE ghost_done (id integer);\n")
        assert [m.name for m in await Migrator(db, migs).upgrade()] == ["0003_ghost"]


@pytest.mark.case("migrations::destructive_gate_ignores_a_migration_with_no_marker")
@pytest.mark.parametrize("dialect", DIALECTS)
async def test_destructive_gate_ignores_a_migration_with_no_marker(tmp_path, dialect):
    async with gate_database(dialect) as (db, prefix):
        await gate_apply(db, tmp_path, prefix + GATE_KEEP + GATE_SHOP)
        await db.execute("INSERT INTO shop (name) VALUES ('a')")
        wider = GATE_SHOP.replace("  note String?", "  note String?\n  extra String?")
        added = await gate_apply(db, tmp_path, prefix + GATE_KEEP + wider)
        assert "-- monorm: destructive" not in added.up_sql
        assert await gate_rows(db, "shop") == 1


@pytest.mark.case("migrations::destructive_gate_sees_a_drop_after_a_table_rename")
@pytest.mark.parametrize("dialect", DIALECTS)
async def test_destructive_gate_sees_a_drop_after_a_table_rename(tmp_path, dialect):
    async with gate_database(dialect) as (db, prefix):
        await gate_apply(db, tmp_path, prefix + GATE_KEEP + GATE_SHOP)
        await db.execute("INSERT INTO shop (name, note) VALUES ('a', 'n')")
        venue = (
            GATE_SHOP.replace('@@map("shop")', '@@map("venue")\n  @@renamed_from("shop")').replace("model Shop", "model Venue").replace("  note String?\n", "")
        )
        migs, step = gate_next(tmp_path, prefix + GATE_KEEP + venue)
        # the marker names the table as the database has it before the migration
        assert "-- monorm: destructive drop-column shop.note\n" in step.up_sql
        with pytest.raises(MigrationError, match=gate_refusal(step.name, "drops column shop.note", "shop")):
            await Migrator(db, migs).upgrade()
        assert await gate_rows(db, "shop") == 1
        await Migrator(db, migs).upgrade(allow_destructive=True)
        assert await gate_rows(db, "venue") == 1


@pytest.mark.case("migrations::rollback_gate_refuses_a_down_drop_on_a_table_with_rows")
@pytest.mark.parametrize("dialect", DIALECTS)
async def test_rollback_gate_refuses_a_down_drop_on_a_table_with_rows(tmp_path, dialect):
    async with gate_database(dialect) as (db, prefix):
        await gate_apply(db, tmp_path, prefix + GATE_KEEP)
        migs, created = gate_next(tmp_path, prefix + GATE_KEEP + GATE_SHOP)
        migrator = Migrator(db, migs)
        await migrator.upgrade()  # creating a table is not destructive
        await db.execute("INSERT INTO shop (name) VALUES ('a')")
        assert "-- monorm: destructive drop-table shop\n" in created.down_sql
        with pytest.raises(MigrationError, match=gate_refusal(f"rolling back {created.name}", "drops table shop", "shop")):
            await migrator.downgrade()
        assert await gate_rows(db, "shop") == 1
        assert [m.name for m in await migrator.downgrade(allow_destructive=True)] == [created.name]


@pytest.mark.case("migrations::destructive_gate_waits_for_a_concurrent_insert_on_postgres")
async def test_destructive_gate_waits_for_a_concurrent_insert_on_postgres(tmp_path):
    async with gate_database("postgres") as (db, prefix):
        await gate_apply(db, tmp_path, prefix + GATE_KEEP + GATE_SHOP)
        migs, dropped = gate_next(tmp_path, prefix + GATE_KEEP)
        writer = await connect(models(GATE_KEEP + GATE_SHOP))
        try:
            async with writer.transaction():
                await writer.execute("INSERT INTO shop (name) VALUES ('a')")  # not committed yet
                upgrade = asyncio.ensure_future(Migrator(db, migs).upgrade())
                await asyncio.sleep(0.5)
                assert not upgrade.done()  # the gate waits for the table lock
            with pytest.raises(MigrationError, match=gate_refusal(dropped.name, "drops table shop", "shop")):
                await upgrade
            assert await gate_rows(db, "shop") == 1
        finally:
            await writer.close()


NO_TX_INDEX = "-- monorm: no-transaction create-index shop_name_idx\nCREATE UNIQUE INDEX CONCURRENTLY shop_name_idx ON shop (name);\n"
NO_TX_DROP = "-- monorm: no-transaction drop-index shop_name_idx\nDROP INDEX CONCURRENTLY IF EXISTS shop_name_idx;\n"


def no_tx_folder(tmp_path, name="0002_shop_name_idx", up=NO_TX_INDEX, down=NO_TX_DROP, extra=None):
    folder = tmp_path / name
    folder.mkdir()
    (folder / "up.sql").write_text(up)
    (folder / "down.sql").write_text(down)
    for file, text in (extra or {}).items():
        (folder / file).write_text(text)
    return Migrations(tmp_path, models(GATE_KEEP + GATE_SHOP))


async def index_valid(db, name="shop_name_idx"):
    return await scalar(db, f"SELECT indisvalid FROM pg_index WHERE indexrelid = to_regclass('{name}')")


@pytest.mark.case("migrations::a_no_transaction_migration_builds_an_index_concurrently")
async def test_a_no_transaction_migration_builds_an_index_concurrently(tmp_path):
    async with gate_database("postgres") as (db, _):
        await gate_apply(db, tmp_path, GATE_KEEP + GATE_SHOP)
        migs = no_tx_folder(tmp_path)
        done = await Migrator(db, migs).upgrade()
        assert [m.name for m in done] == ["0002_shop_name_idx"]
        assert await index_valid(db) == "t"
        assert await gate_rows(db, "monorm_migrations") == 2
        assert await Migrator(db, migs).upgrade() == []


@pytest.mark.case("migrations::a_no_transaction_migration_repairs_an_invalid_index")
async def test_a_no_transaction_migration_repairs_an_invalid_index(tmp_path):
    async with gate_database("postgres") as (db, _):
        await gate_apply(db, tmp_path, GATE_KEEP + GATE_SHOP)
        await db.execute("INSERT INTO shop (name) VALUES ('a'), ('a')")
        migs = no_tx_folder(tmp_path)
        with pytest.raises(monorm.DatabaseError, match="could not create unique index"):
            await Migrator(db, migs).upgrade()
        assert await index_valid(db) == "f"  # the failed build leaves an invalid index
        assert [s.applied for s in await Migrator(db, migs).status()] == [True, False]
        await db.execute("DELETE FROM shop")
        assert [m.name for m in await Migrator(db, migs).upgrade()] == ["0002_shop_name_idx"]
        assert await index_valid(db) == "t"


@pytest.mark.case("migrations::a_no_transaction_migration_with_a_data_step_is_refused")
async def test_a_no_transaction_migration_with_a_data_step_is_refused(tmp_path):
    async with gate_database("postgres") as (db, _):
        await gate_apply(db, tmp_path, GATE_KEEP + GATE_SHOP)
        migs = no_tx_folder(tmp_path, extra={"data.py": "async def run(db):\n    pass\n"})
        with pytest.raises(MigrationError, match="0002_shop_name_idx is a no-transaction migration and has a data step"):
            await Migrator(db, migs).upgrade()
        assert await index_valid(db) is None


@pytest.mark.case("migrations::a_no_transaction_migration_is_refused_on_sqlite")
async def test_a_no_transaction_migration_is_refused_on_sqlite(tmp_path):
    async with gate_database("sqlite") as (db, prefix):
        await gate_apply(db, tmp_path, prefix + GATE_KEEP + GATE_SHOP)
        migs = no_tx_folder(tmp_path)
        with pytest.raises(MigrationError, match="0002_shop_name_idx is a no-transaction migration; sqlite has no such mode"):
            await Migrator(db, migs).upgrade()


@pytest.mark.case("migrations::a_no_transaction_migration_rolls_back_with_its_down_file")
async def test_a_no_transaction_migration_rolls_back_with_its_down_file(tmp_path):
    async with gate_database("postgres") as (db, _):
        await gate_apply(db, tmp_path, GATE_KEEP + GATE_SHOP)
        migs = no_tx_folder(tmp_path)
        migrator = Migrator(db, migs)
        await migrator.upgrade()
        assert [m.name for m in await migrator.downgrade()] == ["0002_shop_name_idx"]
        assert await index_valid(db) is None
        assert await gate_rows(db, "monorm_migrations") == 1


@pytest.mark.case("migrations::a_no_transaction_migration_waits_for_another_migrator")
async def test_a_no_transaction_migration_waits_for_another_migrator(tmp_path):
    async with gate_database("postgres") as (db, _):
        await gate_apply(db, tmp_path, GATE_KEEP + GATE_SHOP)
        migs = no_tx_folder(tmp_path)
        other = await connect(models(GATE_KEEP + GATE_SHOP))
        try:
            async with other.transaction():
                await other.execute("SELECT pg_advisory_xact_lock(1869770093)")  # the lock of a normal migration
                upgrade = asyncio.ensure_future(Migrator(db, migs).upgrade())
                await asyncio.sleep(0.5)
                assert not upgrade.done()
                assert await index_valid(db) is None
            assert [m.name for m in await upgrade] == ["0002_shop_name_idx"]
            assert await gate_rows(db, "monorm_migrations") == 2
        finally:
            await other.close()


@pytest.mark.case("migrations::two_migrators_run_one_no_transaction_migration_once")
async def test_two_migrators_run_one_no_transaction_migration_once(tmp_path):
    async with gate_database("postgres") as (db, _):
        await gate_apply(db, tmp_path, GATE_KEEP + GATE_SHOP)
        migs = no_tx_folder(tmp_path)
        other = await connect(models(GATE_KEEP + GATE_SHOP))
        try:
            async with other.transaction():
                await other.execute("SELECT pg_advisory_xact_lock(1869770093)")
                first = asyncio.ensure_future(Migrator(db, migs).upgrade())
                second = asyncio.ensure_future(Migrator(db, migs).upgrade())
                await asyncio.sleep(0.5)
                assert not first.done() and not second.done()
            done = sorted([[m.name for m in await first], [m.name for m in await second]])
            assert done == [[], ["0002_shop_name_idx"]]  # the loser rechecks under the lock and does nothing
            assert await gate_rows(db, "monorm_migrations") == 2
            assert await index_valid(db) == "t"
        finally:
            await other.close()


@pytest.mark.case("migrations::a_normal_migration_waits_for_a_session_lock_of_a_no_transaction_migrator")
async def test_a_normal_migration_waits_for_a_session_lock_of_a_no_transaction_migrator(tmp_path):
    async with gate_database("postgres") as (db, _):
        await gate_apply(db, tmp_path, GATE_KEEP + GATE_SHOP)
        migs, migration = gate_next(tmp_path, GATE_KEEP + GATE_SHOP + "\nmodel Extra {\n  id BigInt @id\n}\n")
        other = await connect(models(GATE_KEEP + GATE_SHOP))
        try:
            async with other.transaction():  # one pinned connection holds the session lock
                await other.execute("SELECT pg_advisory_lock(1869770093)")
                upgrade = asyncio.ensure_future(Migrator(db, migs).upgrade())
                await asyncio.sleep(0.5)
                assert not upgrade.done()
                await other.execute("SELECT pg_advisory_unlock(1869770093)")
            assert [m.name for m in await upgrade] == [migration.name]
        finally:
            await other.close()


@pytest.mark.case("migrations::a_no_transaction_migration_runs_between_normal_migrations_in_order")
async def test_a_no_transaction_migration_runs_between_normal_migrations_in_order(tmp_path):
    async with gate_database("postgres") as (db, _):
        await gate_apply(db, tmp_path, GATE_KEEP + GATE_SHOP)
        no_tx_folder(tmp_path)
        after = tmp_path / "0003_after"
        after.mkdir()
        (after / "up.sql").write_text("CREATE TABLE after_tx (id integer);\n")
        (after / "down.sql").write_text("DROP TABLE after_tx;\n")
        migs = Migrations(tmp_path, models(GATE_KEEP + GATE_SHOP))
        assert [m.name for m in await Migrator(db, migs).upgrade()][-2:] == ["0002_shop_name_idx", "0003_after"]
        assert await gate_rows(db, "monorm_migrations") == 3
        assert await index_valid(db) == "t"
        bad = tmp_path / "0004_bad"
        bad.mkdir()
        (bad / "up.sql").write_text("CREATE TABLE half (id integer);\nSELECT 1 / 0;\n")
        (bad / "down.sql").write_text("DROP TABLE half;\n")
        with pytest.raises(monorm.DatabaseError, match="division by zero"):
            await Migrator(db, Migrations(tmp_path, models(GATE_KEEP + GATE_SHOP))).upgrade()
        assert await scalar(db, "SELECT to_regclass('half')") is None  # the normal migration rolls back whole
        assert await gate_rows(db, "monorm_migrations") == 3
        assert await index_valid(db) == "t"  # the no-transaction one stays recorded


@pytest.mark.case("migrations::a_no_transaction_down_file_is_refused_on_sqlite_and_names_the_folder")
async def test_a_no_transaction_down_file_is_refused_on_sqlite_and_names_the_folder(tmp_path):
    async with gate_database("sqlite") as (db, prefix):
        await gate_apply(db, tmp_path, prefix + GATE_KEEP + GATE_SHOP)
        migs = no_tx_folder(tmp_path, up="CREATE TABLE extra (id integer);\n")
        migrator = Migrator(db, migs)
        assert [m.name for m in await migrator.upgrade()] == ["0002_shop_name_idx"]
        with pytest.raises(MigrationError, match="0002_shop_name_idx/down.sql is a no-transaction migration; sqlite has no such mode"):
            await migrator.downgrade()
        assert await gate_rows(db, "monorm_migrations") == 2


@pytest.mark.case("migrations::a_rollback_waits_for_a_no_transaction_build_without_a_deadlock")
async def test_a_rollback_waits_for_a_no_transaction_build_without_a_deadlock(tmp_path):
    async with gate_database("postgres") as (db, _):
        await gate_apply(db, tmp_path, GATE_KEEP + GATE_SHOP)
        after = tmp_path / "0002_after"
        after.mkdir()
        (after / "up.sql").write_text("CREATE TABLE after_tx (id integer);\n")
        (after / "down.sql").write_text("DROP TABLE after_tx;\n")
        await Migrator(db, Migrations(tmp_path, models(GATE_KEEP + GATE_SHOP))).upgrade()
        migs = no_tx_folder(tmp_path, name="0003_shop_name_idx")
        other = await connect(models(GATE_KEEP + GATE_SHOP))
        try:
            async with other.transaction():
                await other.execute("INSERT INTO shop (name) VALUES ('w')")  # an open writer: the index build waits for it
                build = asyncio.ensure_future(Migrator(db, migs).upgrade())
                await asyncio.sleep(0.5)
                rollback = asyncio.ensure_future(Migrator(db, migs).downgrade())  # a normal step: waits for the migration lock
                await asyncio.sleep(1.5)  # longer than deadlock_timeout (1 s)
                assert not build.done() and not rollback.done()
            assert [m.name for m in await build] == ["0003_shop_name_idx"]
            assert [m.name for m in await rollback] == ["0002_after"]  # chosen before the build recorded its row
            assert await index_valid(db) == "t"
        finally:
            await other.close()
