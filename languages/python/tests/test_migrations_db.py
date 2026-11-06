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
GATE_TABLES = ("shop", "venue", "keep", "ghost_done", "monorm_migrations")
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
