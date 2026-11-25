"""`monorm pull`, baseline and drift against a live database.

The Postgres tests run in a database of their own (``<test database>_pull``): they drop
and recreate its ``public`` schema.
"""

import os
import subprocess
import sys
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit

import pytest
from conftest import DATABASE_URL

import monorm
from monorm import Registry
from artifact_caps import has_extension
from monorm.migrations import Migrations, Migrator, pull

FIXTURE = Path(__file__).resolve().parents[3] / "fixtures" / "pull_live.sql"
BLOG = Path(__file__).resolve().parents[3] / "examples" / "blog" / "schema.prisma"

_url = urlsplit(DATABASE_URL)
PULL_DB = _url.path.lstrip("/") + "_pull"
PULL_URL = urlunsplit(_url._replace(path="/" + PULL_DB))

# What the fixture holds that the schema language can't say (see the `gap:` lines).
# the view comes back around the table step: the diff cannot know what a view body reads
EXPECTED_DIFFERENCES = [
    "drop view live_bundles",
    "drop column bundles_slot.during",
    "create view live_bundles",
]


@pytest.fixture(scope="module", autouse=True)
async def pull_database():
    try:
        admin = await monorm.connect(DATABASE_URL, max_connections=1, default=False, registry=Registry())
    except monorm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {DATABASE_URL}: {e}")
    try:
        if not await admin._fetch_text(f"SELECT 1 FROM pg_database WHERE datname = '{PULL_DB}'"):
            await admin.execute(f'CREATE DATABASE "{PULL_DB}"')
    finally:
        await admin.close()


async def fresh(sql: str = "") -> monorm.Database:
    db = await monorm.connect(PULL_URL, max_connections=2, default=False, registry=Registry())
    await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public;" + sql)
    return db


async def test_pull_baseline_and_drift_of_a_django_style_database(tmp_path):
    db = await fresh(FIXTURE.read_text())
    try:
        pulled = await pull(db)
        text = pulled.schema
        # Django's names survive: foreign keys, uniques, like-indexes, checks.
        assert 'map: "bundles_bundle_shop_id_8f3e_fk_account_shop_id"' in text
        assert "deferrable: deferred" in text
        assert '@@unique([domain], map: "account_shop_domain_9a1b2c3d_uniq")' in text
        assert 'ops: raw("varchar_pattern_ops")' in text
        assert "with: { fillfactor: 70 }" in text
        # a trigger with REFERENCING transition tables reads back with them
        assert "@@trigger(account_shop_updated_at, referencing(new: new_rows), when: after, operation: [update], level: statement" in text
        assert not any("account_shop_updated_at" in g for g in pulled.gaps)
        assert "function set_updated_at {" in text and "volatility = immutable" in text
        assert "status ShopStatus @default(active)" in text
        assert '@relation("BundlesBundle_parent"' in text
        assert "rank Int @default(-1)" in text
        assert "@@id([bundle_id, tag])" in text
        # a view reads back as a view block with its SELECT in a sidecar file
        assert "view LiveBundles {" in text
        assert "_deleted_at IS NULL" in pulled.views["views/public/LiveBundles.sql"]
        gaps = "\n".join(pulled.gaps)
        for what in ["rule bundles_bundle_soft_delete", "tstzrange", "uses a sequence"]:
            assert what in gaps
            assert what in text  # also listed at the end of the file
        assert [s.summary for s in pulled.differences] == EXPECTED_DIFFERENCES

        schema = tmp_path / "schema.prisma"
        pulled.write(schema)
        migrations = Migrations(tmp_path / "migrations", schema)
        migrator = Migrator(db, migrations)
        first = await migrator.baseline()
        assert first.name == "0001_initial"
        assert [(s.migration.name, s.applied) for s in await migrator.status()] == [("0001_initial", True)]
        assert await migrator.upgrade() == []
        assert not migrations.plan()

        drift = await migrator.drift()
        assert drift.migration == "0001_initial"
        assert [s.summary for s in drift.steps] == EXPECTED_DIFFERENCES
        assert any("rule bundles_bundle_soft_delete" in g for g in drift.gaps)

        await db.execute("ALTER TABLE account_shop ADD COLUMN extra integer; DROP INDEX account_shop_live_idx")
        drift = await migrator.drift()
        assert "drop column account_shop.extra" in [s.summary for s in drift.steps]
        assert "create index account_shop_live_idx on account_shop" in [s.summary for s in drift.steps]

        with pytest.raises(monorm.migrations.MigrationError, match="already has applied migrations"):
            await migrator.baseline()
    finally:
        await db.close()


SERIALS = """
CREATE TABLE s_empty (id serial PRIMARY KEY);
CREATE TABLE s_full (id serial PRIMARY KEY);
INSERT INTO s_full (id) SELECT g FROM generate_series(1, 5) g;
SELECT setval('s_full_id_seq', 5);
CREATE SEQUENCE shared_seq;
CREATE TABLE c_empty (id integer PRIMARY KEY DEFAULT nextval('shared_seq'));
CREATE TABLE c_full (id integer PRIMARY KEY DEFAULT nextval('shared_seq'));
INSERT INTO c_full (id) SELECT g FROM generate_series(1, 5) g;
"""


async def test_owned_serials_pull_as_identity_and_shared_sequences_stay_defaults(tmp_path):
    db = await fresh(SERIALS)
    try:
        pulled = await pull(db)
        gaps = "\n".join(pulled.gaps)
        text = pulled.schema
        # an owned serial is an identity column, with a note
        assert "column s_empty.id uses a sequence" in gaps and "column s_full.id uses a sequence" in gaps
        assert text.count("@default(autoincrement())") == 2
        # a shared sequence stays a raw default, with a note
        assert (
            "column c_empty.id defaults to nextval('shared_seq'::regclass), a sequence the column does not own" in gaps
        )
        assert "@default(dbgenerated(\"nextval('shared_seq'::regclass)\"))" in text
        assert pulled.differences == []
        # the next migration leaves the column on the shared sequence
        schema = tmp_path / "schema.prisma"
        pulled.write(schema)
        migrator = Migrator(db, Migrations(tmp_path / "migrations", schema))
        await migrator.baseline()
        assert not migrator.migrations.plan()
        rows = await db._fetch_text(
            "SELECT attrelid::regclass::text, attidentity::text, pg_get_expr(d.adbin, d.adrelid) FROM pg_attribute a "
            "JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum "
            "WHERE attrelid IN ('c_empty'::regclass, 'c_full'::regclass) ORDER BY 1"
        )
        assert rows == [
            ("c_empty", "", "nextval('shared_seq'::regclass)"),
            ("c_full", "", "nextval('shared_seq'::regclass)"),
        ]
    finally:
        await db.close()


async def _assert_generated_pull_is_clean(db, tmp_path, wanted):
    pulled = await pull(db)
    assert wanted in pulled.schema
    assert not any("generated" in g for g in pulled.gaps)
    assert pulled.differences == []
    schema = tmp_path / "schema.prisma"
    pulled.write(schema)
    migrator = Migrator(db, Migrations(tmp_path / "migrations", schema))
    await migrator.baseline()
    assert (await migrator.drift()).steps == []


async def test_generated_columns_pull_back_without_drift(tmp_path):
    db = await fresh(
        "CREATE TABLE gen_lines (id bigint PRIMARY KEY, quantity integer NOT NULL, price integer NOT NULL, "
        "total integer GENERATED ALWAYS AS (quantity * price) STORED)"
    )
    try:
        await _assert_generated_pull_is_clean(db, tmp_path, "@generated(((quantity * price)))")
    finally:
        await db.close()


async def test_bytea_pulls_as_bytes_without_drift(tmp_path):
    db = await fresh("CREATE TABLE blobs (id bigint PRIMARY KEY, data bytea, tail bytea NOT NULL)")
    try:
        pulled = await pull(db)
        assert "data Bytes?" in pulled.schema and "tail Bytes" in pulled.schema
        assert "ByteA" not in pulled.schema
        assert pulled.differences == []
        schema = tmp_path / "schema.prisma"
        pulled.write(schema)
        migrator = Migrator(db, Migrations(tmp_path / "migrations", schema))
        await migrator.baseline()
        assert (await migrator.drift()).steps == []
    finally:
        await db.close()


async def test_a_sqlite_blob_pulls_as_bytes_without_drift(tmp_path):
    registry = Registry()
    monorm.loads('datasource db { provider = "sqlite" }\nmodel Keep { id Int @id }', registry=registry)
    db = await monorm.connect(f"sqlite://{tmp_path / 'live.db'}", default=False, registry=registry)
    try:
        await db.execute("CREATE TABLE blobs (id INTEGER PRIMARY KEY, data BLOB)")
        await _assert_generated_pull_is_clean(db, tmp_path, "data Bytes?")
    finally:
        await db.close()


async def test_a_virtual_generated_column_pulls_back_without_drift_on_postgres(tmp_path):
    db = await fresh("SELECT 1")
    try:
        if int((await db._fetch_text("SHOW server_version_num"))[0][0]) < 180000:
            pytest.skip("VIRTUAL generated columns need PostgreSQL 18")
        await db.execute(
            "CREATE TABLE gen_lines (id bigint PRIMARY KEY, quantity integer NOT NULL, price integer NOT NULL, "
            "total integer GENERATED ALWAYS AS (quantity * price) VIRTUAL)"
        )
        await _assert_generated_pull_is_clean(db, tmp_path, "@generated(((quantity * price)), stored: false)")
    finally:
        await db.close()


async def test_sqlite_generated_columns_pull_back_without_drift(tmp_path):
    for kind in ("STORED", "VIRTUAL"):
        folder = tmp_path / kind
        folder.mkdir()
        registry = Registry()
        monorm.loads('datasource db { provider = "sqlite" }\nmodel Keep { id Int @id }', registry=registry)
        db = await monorm.connect(f"sqlite://{folder / 'live.db'}", default=False, registry=registry)
        try:
            await db.execute(
                "CREATE TABLE gen_lines (id INTEGER PRIMARY KEY, quantity INTEGER NOT NULL, price INTEGER NOT NULL, "
                f"total INTEGER GENERATED ALWAYS AS (quantity * price) {kind})"
            )
            wanted = "@generated((quantity * price))" if kind == "STORED" else "@generated((quantity * price), stored: false)"
            await _assert_generated_pull_is_clean(db, folder, wanted)
        finally:
            await db.close()


async def test_sqlite_triggers_pull_back_as_trigger_attributes_without_drift(tmp_path):
    registry = Registry()
    monorm.loads('datasource db { provider = "sqlite" }\nmodel Keep { id Int @id }', registry=registry)
    db = await monorm.connect(f"sqlite://{tmp_path / 'live.db'}", default=False, registry=registry)
    try:
        await db.execute("CREATE TABLE tr_posts (id INTEGER PRIMARY KEY, title TEXT NOT NULL, touched INTEGER NOT NULL DEFAULT 0)")
        await db.execute(
            'CREATE TRIGGER "tr_posts_touch_trigger" AFTER UPDATE OF "title" ON "tr_posts" FOR EACH ROW WHEN (old.title <> new.title) BEGIN\n'
            'UPDATE "tr_posts" SET "touched" = "touched" + 1 WHERE "id" = NEW."id";\nEND'
        )
        # a trigger in another form is a gap, not an attribute
        await db.execute("CREATE TRIGGER other AFTER INSERT ON tr_posts BEGIN SELECT 1; END")
        pulled = await pull(db)
        assert '@@trigger(touch, when: after, operation: [update_of(title)], condition: "old.title <> new.title", func: ' in pulled.schema
        assert "@@trigger(other" not in pulled.schema
        assert any(g.startswith("trigger other on tr_posts") for g in pulled.gaps)
        assert not any(g.startswith("trigger tr_posts_touch_trigger") for g in pulled.gaps)
        assert pulled.differences == []
    finally:
        await db.close()


async def test_add_identity_refuses_a_column_that_owns_a_sequence_but_defaults_to_another(tmp_path):
    db = await fresh(
        "CREATE SEQUENCE other_seq; CREATE TABLE t_owns (id serial PRIMARY KEY); "
        "ALTER TABLE t_owns ALTER COLUMN id SET DEFAULT nextval('other_seq')"
    )
    try:
        pulled = await pull(db)
        assert "@default(dbgenerated(\"nextval('other_seq'::regclass)\"))" in pulled.schema
        schema = tmp_path / "schema.prisma"
        schema.write_text(
            pulled.schema.replace(
                "@default(dbgenerated(\"nextval('other_seq'::regclass)\"))", "@default(autoincrement())"
            )
        )
        migrator = Migrator(db, Migrations(tmp_path / "migrations", schema))
        await migrator.baseline()
        with pytest.raises(monorm.migrations.MigrationError, match="t_owns.id: the column takes its values from a sequence it does not own"):
            await migrator.drift()
    finally:
        await db.close()


async def test_rollback_refuses_a_baselined_migration_unless_forced(tmp_path):
    db = await fresh("CREATE TABLE keep (id bigint PRIMARY KEY); INSERT INTO keep VALUES (1)")
    try:
        schema = tmp_path / "schema.prisma"
        (await pull(db)).write(schema)
        migrator = Migrator(db, Migrations(tmp_path / "migrations", schema))
        await migrator.baseline()
        assert await db._fetch_text("SELECT baseline FROM monorm_migrations") == [("t",)]
        with pytest.raises(monorm.migrations.MigrationError, match="0001_initial was baselined"):
            await migrator.downgrade()
        assert await db._fetch_text("SELECT count(*) FROM keep") == [("1",)]
        assert [m.name for m in await migrator.downgrade(force=True, allow_destructive=True)] == ["0001_initial"]
        assert await db._fetch_text("SELECT to_regclass('keep')::text") == [(None,)]
    finally:
        await db.close()


async def test_an_older_migrations_table_gets_the_baseline_column(tmp_path):
    db = await fresh(
        "CREATE TABLE monorm_migrations (name text PRIMARY KEY, checksum text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now());"
        "CREATE TABLE keep (id bigint PRIMARY KEY)"
    )
    try:
        schema = tmp_path / "schema.prisma"
        (await pull(db)).write(schema)
        migrator = Migrator(db, Migrations(tmp_path / "migrations", schema))
        await migrator.baseline()
        assert await db._fetch_text("SELECT baseline FROM monorm_migrations") == [("t",)]
    finally:
        await db.close()


async def test_a_database_made_by_the_orm_pulls_back_without_differences(tmp_path):
    blog = monorm.load(BLOG, registry=Registry())
    db = await fresh()
    try:
        migrations = Migrations(tmp_path / "blog", BLOG)
        migrations.make()
        assert await Migrator(db, migrations).upgrade()
        assert not await Migrator(db, migrations).drift()

        pulled = await pull(db)
        assert pulled.differences == []
        assert pulled.gaps == []
        assert "@@check(" in pulled.schema and "extensions = [pg_trgm]" in pulled.schema
        assert blog  # the blog models compile from the same file
    finally:
        await db.close()


SQLITE = """
datasource db {
  provider = "sqlite"
}

enum Color {
  red
  green
  @@storage(text)
}

model Owner {
  id    BigInt @id @default(autoincrement())
  email String @unique
  color Color  @default(red)
  pets  Pet[]
}

model Pet {
  id       BigInt @id @default(autoincrement())
  owner_id BigInt
  name     String
  age      Int    @default(0)
  owner    Owner  @relation(fields: [owner_id], references: [id], onDelete: Cascade)

  @@index([owner_id, -age])
  @@index([sql("lower(name)")], name: "pet_name_lower", where: raw("age > 0"))
  @@check("age >= 0", name: "pet_age")
}
"""


async def test_sqlite_pull_baseline_and_drift(tmp_path):
    source = tmp_path / "schema.prisma"
    source.write_text(SQLITE)
    url = f"sqlite://{tmp_path / 'live.db'}"
    registry = Registry()
    monorm.load(source, registry=registry)
    db = await monorm.connect(url, default=False, registry=registry)
    try:
        made = Migrations(tmp_path / "made", source)
        made.make()
        await Migrator(db, made).upgrade()

        pulled = await pull(db)
        assert pulled.differences == []
        assert 'provider = "sqlite"' in pulled.schema
        assert '@@check((age >= 0), name: "pet_age")' in pulled.schema

        schema = tmp_path / "pulled.prisma"
        pulled.write(schema)
        await db.execute("DROP TABLE monorm_migrations")
        migrator = Migrator(db, Migrations(tmp_path / "migrations", schema))
        await migrator.baseline()
        assert not await migrator.drift()
        with pytest.raises(monorm.migrations.MigrationError, match="was baselined"):
            await migrator.downgrade()

        await db.execute("CREATE INDEX extra_idx ON pet (name)")
        assert [s.summary for s in (await migrator.drift()).steps] == ["drop index extra_idx"]
    finally:
        await db.close()


LOSSY = """
DROP SCHEMA IF EXISTS other CASCADE; CREATE SCHEMA other;
CREATE TABLE other.users (id uuid PRIMARY KEY);
CREATE TABLE users (id bigint PRIMARY KEY);
CREATE TABLE profiles (id bigint PRIMARY KEY, user_id uuid REFERENCES other.users, owner_id bigint REFERENCES users);
ALTER TABLE profiles ENABLE ROW LEVEL SECURITY;
CREATE TABLE booking (id bigint PRIMARY KEY, a int, b int, CONSTRAINT booking_a_uniq UNIQUE (a) INCLUDE (b));
CREATE UNLOGGED TABLE cache (id bigint PRIMARY KEY) WITH (fillfactor = 80);
CREATE TABLE a (id bigint PRIMARY KEY, n int CONSTRAINT positive CHECK (n > 0), p bigint CONSTRAINT fk_p REFERENCES users);
CREATE TABLE b (id bigint PRIMARY KEY, n int CONSTRAINT positive CHECK (n > 0), p bigint CONSTRAINT fk_p REFERENCES users);
CREATE TABLE t (id bigint PRIMARY KEY, c int CONSTRAINT t_c_key UNIQUE, d int CONSTRAINT t_d_key UNIQUE, "class" int,
    price numeric(10, 2) NOT NULL DEFAULT 0.00);
ALTER TABLE t ADD CONSTRAINT uq2 UNIQUE NULLS NOT DISTINCT (c);
CREATE INDEX t_d_idx ON t (d);
CREATE FUNCTION g() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN -- \"\"\"
RETURN NEW; END $$;
CREATE TRIGGER t_g BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION g();
"""


async def test_pull_reports_what_it_cannot_write(tmp_path):
    db = await fresh(LOSSY)
    try:
        pulled = await pull(db)
        text, gaps = pulled.schema, "\n".join(pulled.gaps)
        # a foreign key to another schema is left out, not pointed at the local table
        assert "references other.users, outside the schema; it is left out" in gaps
        assert "fields: [user_id]" not in text
        assert "table profiles has row-level security" in gaps
        assert "booking.booking_a_uniq has INCLUDE columns" in gaps
        assert "table cache is UNLOGGED" in gaps and "table cache has storage options (fillfactor=80)" in gaps
        # check and foreign-key names are unique per table only
        assert text.count('name: "positive")') == 2 and text.count('map: "fk_p")') == 2
        # the @unique shorthand keeps the other objects on its column
        assert 'map: "uq2", nulls_not_distinct: true' in text
        assert '@@index([d], name: "t_d_idx")' in text
        assert 'class_ Int? @map("class")' in text
        assert '@default(dbgenerated("0.00"))' in text
        # a trigger whose function is not written is left out with it
        assert "@@trigger(t_g" not in text and "trigger t.t_g calls g, which is not pulled" in gaps
        assert [s.summary for s in pulled.differences] == ["drop trigger t_g on t", "drop function g()"]

        pulled.write(tmp_path / "schema.prisma")
        made = Migrations(tmp_path / "migrations", tmp_path / "schema.prisma")
        made.make()
        other = await fresh()
        try:
            await Migrator(other, made).upgrade()
        finally:
            await other.close()
    finally:
        await db.close()


async def test_comments_cannot_escape_the_shadow(tmp_path):
    db = await fresh("CREATE TABLE victim (id bigint PRIMARY KEY); CREATE TABLE t (id bigint PRIMARY KEY);")
    await db.execute(f'ALTER DATABASE "{PULL_DB}" SET standard_conforming_strings = off')
    try:
        await db.execute(
            "SET standard_conforming_strings = on; COMMENT ON TABLE t IS 'x\\''; COMMIT; DROP TABLE victim; --'"
        )
        legacy = await monorm.connect(PULL_URL, max_connections=1, default=False, registry=Registry())
        try:
            pulled = await pull(legacy)
        finally:
            await legacy.close()
        assert await db._fetch_text("SELECT to_regclass('victim') IS NOT NULL") == [("t",)]
        assert await db._fetch_text("SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'monorm_shadow%'") == [("0",)]
        assert pulled.differences == []
    finally:
        await db.execute(f'ALTER DATABASE "{PULL_DB}" RESET standard_conforming_strings')
        await db.close()


async def test_drift_after_an_enum_value_recreates_the_columns(tmp_path):
    db = await fresh(
        "CREATE TYPE order_state AS ENUM ('new', 'paid'); CREATE TABLE orders (id bigint PRIMARY KEY, state order_state);"
    )
    try:
        (await pull(db)).write(tmp_path / "schema.prisma")
        made = Migrations(tmp_path / "migrations", tmp_path / "schema.prisma")
        await Migrator(db, made).baseline()
        await db.execute("ALTER TYPE order_state ADD VALUE 'refunded'")
        drift = await Migrator(db, made).drift()
        sql = "\n".join(s.sql for s in drift.steps)
        assert 'ALTER COLUMN "state" TYPE' in sql, sql
    finally:
        await db.close()


async def test_pull_without_create_writes_the_schema(tmp_path):
    db = await fresh("CREATE TABLE t (id bigint PRIMARY KEY);")
    # roles are global to the server: one per test database
    role = f"monorm_pull_reader_{PULL_DB}"
    try:
        await db.execute(
            f"DROP ROLE IF EXISTS {role}; CREATE ROLE {role} LOGIN PASSWORD 'r';"
            f'REVOKE CREATE ON DATABASE "{PULL_DB}" FROM PUBLIC; GRANT USAGE ON SCHEMA public TO {role};'
            f"GRANT SELECT ON ALL TABLES IN SCHEMA public TO {role}"
        )
        url = urlunsplit(_url._replace(path="/" + PULL_DB, netloc=f"{role}:r@{_url.netloc.rsplit('@', 1)[-1]}"))
        reader = await monorm.connect(url, max_connections=1, default=False, registry=Registry())
        try:
            pulled = await pull(reader)
        finally:
            await reader.close()
        assert "model T {" in pulled.schema
        assert any(g.startswith("the schema was not checked against the database") for g in pulled.gaps)

        # baseline marks the migration; a drift check it cannot run is a warning
        await db.execute(f"GRANT CREATE ON SCHEMA public TO {role}")
        pulled.write(tmp_path / "schema.prisma")
        env = {**os.environ, "MONORM_DATABASE_URL": url}
        done = subprocess.run(
            [sys.executable, "-m", "monorm", "--schema", "schema.prisma", "--dir", "migrations", "baseline"],
            cwd=tmp_path,
            env=env,
            capture_output=True,
            text=True,
        )
        assert done.returncode == 0, done.stderr
        assert "warning: the database was not compared with 0001_initial" in done.stdout, done.stdout
    finally:
        await db.execute(
            f'GRANT CREATE ON DATABASE "{PULL_DB}" TO PUBLIC; DROP TABLE IF EXISTS monorm_migrations;'
            f"DROP OWNED BY {role}; DROP ROLE {role}"
        )
        await db.close()


DJANGO_SQLITE = """
CREATE TABLE "auth_user" ("id" integer NOT NULL PRIMARY KEY AUTOINCREMENT, "is_staff" bool NOT NULL,
    "joined" datetime NOT NULL, "age" integer unsigned NOT NULL CHECK ("age" >= 0), "price" decimal NOT NULL,
    CHECK (price > 0));
CREATE TABLE "app_item" ("id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
    "user_id" integer NOT NULL REFERENCES "auth_user" ("id") DEFERRABLE INITIALLY DEFERRED,
    "owner_id" integer NULL REFERENCES auth_user);
INSERT INTO auth_user (is_staff, joined, age, price) VALUES (1, '2024-01-02 03:04:05', 3, 1.5);
"""


async def test_sqlite_pull_reads_column_clauses(tmp_path):
    url = f"sqlite://{tmp_path / 'django.db'}"
    db = await monorm.connect(url, default=False, registry=Registry(dialect="sqlite"))
    try:
        await db.execute(DJANGO_SQLITE)
        pulled = await pull(db)
    finally:
        await db.close()
    text = pulled.schema
    assert '@@check((age >= 0), name: "auth_user_age_check")' in text, text
    assert '@@check((price > 0), name: "auth_user_check")' in text
    assert "is_staff Boolean" in text and "joined DateTime" in text and "price Float" in text
    assert 'user AuthUser @relation("AppItem_user", fields: [user_id], references: [id], deferrable: deferred)' in text
    assert 'owner AuthUser? @relation("AppItem_owner", fields: [owner_id], references: [id])' in text

    # the pulled models read the rows another tool wrote
    registry = Registry()
    User = monorm.loads(text, registry=registry)["AuthUser"]
    db = await monorm.connect(url, default=False, registry=registry)
    try:
        user = await User.objects.using(db).get()
        assert user.is_staff is True and user.price == 1.5
    finally:
        await db.close()


async def test_sql_views_pull_back_without_drift(tmp_path):
    db = await fresh(
        "CREATE TABLE view_orders (id bigint PRIMARY KEY, shop text NOT NULL, total integer NOT NULL);"
        "CREATE VIEW view_big AS SELECT id, shop, total FROM view_orders WHERE total > 100;"
        "CREATE MATERIALIZED VIEW view_by_shop AS SELECT shop AS id, sum(total) AS spent FROM view_big GROUP BY shop;"
        "CREATE UNIQUE INDEX view_by_shop_id ON view_by_shop (id)"
    )
    try:
        pulled = await pull(db)
        assert "view ViewBig {" in pulled.schema and "@@materialized" in pulled.schema
        assert "@@depends_on([ViewBig])" in pulled.schema
        assert not any("view" in g for g in pulled.gaps)
        assert pulled.differences == []
        schema = tmp_path / "schema.prisma"
        pulled.write(schema)
        assert "total > 100" in (tmp_path / "views" / "public" / "ViewBig.sql").read_text()
        migrator = Migrator(db, Migrations(tmp_path / "migrations", schema))
        await migrator.baseline()
        assert not migrator.migrations.plan()
        assert (await migrator.drift()).steps == []
    finally:
        await db.close()


SOFT_DELETE_RULES = """
-- the rules of django-guitars: the rule name is soft_delete on every table, the column is _deleted_at
CREATE TABLE sdm_t (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, name text NOT NULL, _deleted_at timestamptz);
-- guitars: a Django foreign key has no ON DELETE action; a rule cascades the soft delete
CREATE TABLE sdm_c (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, t_id integer NOT NULL REFERENCES sdm_t (id), _deleted_at timestamptz);
-- a foreign key with ON DELETE CASCADE: the monorm cascade trigger reaches this child
CREATE TABLE sdm_d (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, t_id integer NOT NULL REFERENCES sdm_t (id) ON DELETE CASCADE,
                    _deleted_at timestamptz);
CREATE TABLE sdm_o (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY);
CREATE OR REPLACE RULE soft_delete AS ON DELETE TO sdm_t
  WHERE COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD (UPDATE sdm_t SET _deleted_at = NOW() WHERE "id" = old."id" AND _deleted_at IS NULL);
CREATE OR REPLACE RULE soft_delete AS ON DELETE TO sdm_c
  WHERE COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD (UPDATE sdm_c SET _deleted_at = NOW() WHERE "id" = old."id" AND _deleted_at IS NULL);
CREATE OR REPLACE RULE soft_delete_related_sdm_c AS ON UPDATE TO "sdm_t"
  WHERE old._deleted_at IS NULL AND new._deleted_at IS NOT NULL AND
        COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO ALSO (UPDATE "sdm_c" SET _deleted_at = NOW() WHERE "t_id" = old."id");
-- the plain shape, with no guard
CREATE RULE sdm_d_soft_delete AS ON DELETE TO sdm_d
  DO INSTEAD UPDATE sdm_d SET _deleted_at = now() WHERE sdm_d.id = old.id;
CREATE RULE sdm_o_keep AS ON DELETE TO sdm_o DO INSTEAD NOTHING;
"""


async def scalar(db: monorm.Database, sql: str) -> str:
    return (await db._fetch_text(sql))[0][0]


async def test_pull_moves_a_soft_delete_rule_to_triggers(tmp_path):
    db = await fresh(SOFT_DELETE_RULES)
    try:
        pulled = await pull(db)
        assert '@@soft_delete.deleted_at("deleted_at"' in pulled.schema
        assert pulled.schema.count('rule: "soft_delete"') == 2 and 'rule: "sdm_d_soft_delete"' in pulled.schema
        # a rule of another shape stays a gap and is never dropped
        assert any("rule sdm_o_keep" in g for g in pulled.gaps)
        assert any("rule soft_delete_related_sdm_c" in g for g in pulled.gaps)
        assert not any("rule soft_delete " in g or "rule sdm_d_soft_delete" in g for g in pulled.gaps)
        # the legacy database has no guard for a child under a deleted parent: the move adds it for the cascade child sdm_d
        assert [s.summary for s in pulled.differences] == ["create or replace function sdm_d_soft_delete_guard_t()", "create trigger soft_delete_guard_t on sdm_d"]
        # a baseline records the schema as the database, so the guard goes in by hand
        for step in pulled.differences:
            await db.execute(step.sql)

        schema = tmp_path / "schema.prisma"
        pulled.write(schema)
        migrations = Migrations(tmp_path / "migrations", schema)
        migrator = Migrator(db, migrations)
        baseline_name = (await migrator.baseline()).name
        # the baseline keeps the database as it is: the rule still runs
        await db.execute("INSERT INTO sdm_t (name) VALUES ('a'); DELETE FROM sdm_t")
        assert await scalar(db, "SELECT count(*) FROM sdm_t WHERE _deleted_at IS NOT NULL") == "1"
        await db.execute("TRUNCATE sdm_t CASCADE")

        move = migrations.make("move_soft_delete")
        up = move.up_sql
        assert up.index('DROP RULE IF EXISTS "soft_delete" ON "sdm_t"') < up.index("CREATE TRIGGER")
        assert 'DROP RULE IF EXISTS "soft_delete" ON "sdm_c"' in up and 'DROP RULE IF EXISTS "sdm_d_soft_delete" ON "sdm_d"' in up
        assert "sdm_o_keep" not in up and "soft_delete_related" not in up
        assert [m.name for m in await migrator.upgrade()] == [move.name]

        rules = await scalar(db, "SELECT string_agg(rulename, ',' ORDER BY rulename) FROM pg_rules WHERE schemaname = 'public'")
        assert rules == "sdm_o_keep,soft_delete_related_sdm_c"
        await db.execute(
            "INSERT INTO sdm_t (name) VALUES ('a'); INSERT INTO sdm_c (t_id) SELECT id FROM sdm_t;"
            " INSERT INTO sdm_d (t_id) SELECT id FROM sdm_t"
        )
        await db.execute("DELETE FROM sdm_t")
        assert await scalar(db, "SELECT count(*) FROM sdm_t WHERE _deleted_at IS NOT NULL") == "1"
        assert await scalar(db, "SELECT count(*) FROM sdm_c WHERE _deleted_at IS NOT NULL") == "1"
        # no guitars rule reaches sdm_d: only the monorm cascade trigger soft-deletes it
        assert await scalar(db, "SELECT count(*) FROM sdm_d WHERE _deleted_at IS NOT NULL") == "1"

        drift = await migrator.drift()
        assert not drift.steps
        assert sorted(g.split()[1] for g in drift.gaps if g.startswith("rule ")) == ["sdm_o_keep", "soft_delete_related_sdm_c"]
        # with only recognized rules left, a new pull reproduces the database: no step and no gap line, as `pull` prints
        # "The schema reproduces the database." only for that
        await db.execute("DROP RULE sdm_o_keep ON sdm_o; DROP RULE soft_delete_related_sdm_c ON sdm_t")
        again = await pull(db)
        assert again.differences == [] and again.gaps == []
        # the move is undone without the rule: the down migration warns and does not bring it back
        assert [m.name for m in await migrator.downgrade()] == [move.name]
        assert await scalar(db, "SELECT count(*) FROM pg_rules WHERE rulename = 'soft_delete'") == "0"
        # a down and an up again, and a new database, run the move: there is no rule to drop
        assert [m.name for m in await migrator.upgrade()] == [move.name]
        await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        assert [m.name for m in await migrator.upgrade()] == [baseline_name, move.name]
        await db.execute("INSERT INTO sdm_t (name) VALUES ('b'); DELETE FROM sdm_t")
        assert await scalar(db, "SELECT count(*) FROM sdm_t WHERE _deleted_at IS NOT NULL") == "1"
    finally:
        await db.close()


async def test_pull_keeps_a_soft_delete_rule_as_a_gap_when_its_field_name_is_taken():
    db = await fresh(
        """
CREATE TABLE sdm_x (id serial PRIMARY KEY, deleted_at timestamptz, _deleted_at timestamptz);
CREATE RULE soft_delete AS ON DELETE TO sdm_x
  WHERE COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD (UPDATE sdm_x SET _deleted_at = NOW() WHERE "id" = old."id" AND _deleted_at IS NULL);
"""
    )
    try:
        pulled = await pull(db)
        assert "@@soft_delete" not in pulled.schema
        assert any(g.startswith("rule soft_delete on sdm_x") for g in pulled.gaps)
        assert any(g.startswith("rule claims skipped") for g in pulled.gaps)
    finally:
        await db.close()


async def test_pull_keeps_a_soft_delete_rule_as_a_gap_when_a_cascade_loses_its_child():
    db = await fresh(
        """
CREATE TABLE sdm_p (id serial PRIMARY KEY, _deleted_at timestamptz);
CREATE TABLE sdm_k (id serial PRIMARY KEY, p_id integer NOT NULL REFERENCES sdm_p (id) ON DELETE CASCADE);
CREATE RULE soft_delete AS ON DELETE TO sdm_p
  WHERE COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD (UPDATE sdm_p SET _deleted_at = NOW() WHERE "id" = old."id" AND _deleted_at IS NULL);
"""
    )
    try:
        pulled = await pull(db)
        assert "@@soft_delete" not in pulled.schema
        assert any(g.startswith("rule soft_delete on sdm_p") for g in pulled.gaps)
        assert any(g.startswith("rule claims skipped") and "SdmK" in g for g in pulled.gaps)
    finally:
        await db.close()


async def test_pull_keeps_a_disabled_soft_delete_rule_as_a_gap():
    db = await fresh(
        """
CREATE TABLE sdm_q (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, _deleted_at timestamptz);
CREATE RULE soft_delete AS ON DELETE TO sdm_q
  WHERE COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD (UPDATE sdm_q SET _deleted_at = NOW() WHERE "id" = old."id" AND _deleted_at IS NULL);
ALTER TABLE sdm_q DISABLE RULE soft_delete;
"""
    )
    try:
        pulled = await pull(db)
        assert "@@soft_delete" not in pulled.schema
        assert any(g.startswith("rule soft_delete on sdm_q") and "the rule is disabled" in g for g in pulled.gaps)
    finally:
        await db.close()


EXTENSION_MODELS = """
model Post {
 id Int @id @default(autoincrement())
 title String
 @@history.track
 @@timestamps.updated_at("touched_at", mode: "database")
 @@map("pe_posts")
}
model Comment {
 id Int @id @default(autoincrement())
 body String
 @@history.track(model: "CommentAudit")
 @@map("pe_comments")
}
model BundleVersion {
 id Int @id @default(autoincrement())
 bundle_id Int
 @@history.versions(of: bundle_id)
 @@map("pe_bundle_versions")
}
"""


@pytest.mark.skipif(not has_extension("monorm.history", "monorm.timestamps"), reason="requires the history and timestamps packages")
@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_history_and_timestamps_pull_back_as_attributes_without_drift(provider, tmp_path):
    registry = Registry()
    source = f'datasource db {{ provider = "{provider}" }}\n' + EXTENSION_MODELS
    monorm.loads(source, registry=registry)
    url = f"sqlite://{tmp_path / 'live.db'}" if provider == "sqlite" else PULL_URL
    db = await monorm.connect(url, max_connections=2, default=False, registry=registry)
    try:
        if provider == "postgresql":
            await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        await db.create_tables()
        pulled = await pull(db)
        text = pulled.schema
        assert "@@history.track" in text
        assert '@@history.track(model: "CommentAudit")' in text
        assert "@@history.versions(of: bundle_id)" in text
        assert '@@timestamps.updated_at("touched_at", mode: "database")' in text
        # what the extensions generate is not a model, a trigger or a function of its own
        for generated in ["pe_posts_events", "comment_audit", "h_events_", "h_comment_audit_", "updated_at_touched_at", "version_number", "@@trigger("]:
            assert generated not in text, generated
        assert [s.summary for s in pulled.differences] == []
    finally:
        await db.close()


async def pulled_from(provider, tmp_path, models):
    """Create the tables of `models` in an empty database, and pull them back."""
    registry = Registry()
    monorm.loads(f'datasource db {{ provider = "{provider}" }}\n' + models, registry=registry)
    url = f"sqlite://{tmp_path / 'live.db'}" if provider == "sqlite" else PULL_URL
    db = await monorm.connect(url, max_connections=2, default=False, registry=registry)
    try:
        if provider == "postgresql":
            await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        await db.create_tables()
        return await pull(db)
    finally:
        await db.close()


# a tracker argument that the database cannot show, and whether pull writes a gap line for the skipped claim;
# a partial events list leaves no claim to skip, because the claim needs all three triggers
UNSHOWN_TRACKER_ARGUMENTS = [
    ('fields: [title], label: "t"', True),
    ("row: old", True),
    ('when: "NEW.title <> OLD.title"', True),
    ("exclude: [body]", True),
    ("events: [insert, update]", False),
]


@pytest.mark.skipif(not has_extension("monorm.history"), reason="requires the history package")
@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
@pytest.mark.parametrize(("arguments", "gap"), UNSHOWN_TRACKER_ARGUMENTS)
async def test_a_tracker_with_arguments_the_database_cannot_show_pulls_back_as_plain_objects(provider, arguments, gap, tmp_path):
    models = f'model Post {{\n id Int @id @default(autoincrement())\n title String\n body String\n @@history.track({arguments})\n @@map("pf_posts")\n}}'
    pulled = await pulled_from(provider, tmp_path, models)
    assert "@@history.track" not in pulled.schema
    assert any(g.startswith("object claims skipped, the schema with them differs from the database") for g in pulled.gaps) == gap
    assert [s.summary for s in pulled.differences] == []


@pytest.mark.skipif(not has_extension("monorm.history"), reason="requires the history package")
@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_two_trackers_of_one_table_pull_back_as_two_attributes(provider, tmp_path):
    models = 'model Post {\n id Int @id @default(autoincrement())\n title String\n @@history.track\n @@history.track(model: "PostAudit")\n @@map("p2_posts")\n}'
    pulled = await pulled_from(provider, tmp_path, models)
    assert pulled.schema.count("@@history.track(") == 2
    assert "@@history.track()" in pulled.schema
    assert '@@history.track(model: "PostAudit")' in pulled.schema
    assert "p2_posts_events" not in pulled.schema and "post_audit" not in pulled.schema and "@@trigger(" not in pulled.schema
    assert [s.summary for s in pulled.differences] == []


@pytest.mark.skipif(not has_extension("monorm.timestamps"), reason="requires the timestamps package")
@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_a_timestamps_column_with_another_field_name_pulls_back_with_map(provider, tmp_path):
    models = 'model Post {\n id Int @id @default(autoincrement())\n title String\n @@timestamps.updated_at("touched_at", map: "_touched_at", mode: "database")\n @@map("pm_posts")\n}'
    pulled = await pulled_from(provider, tmp_path, models)
    assert '@@timestamps.updated_at("touched_at", map: "_touched_at", mode: "database")' in pulled.schema
    assert "updated_at__touched_at" not in pulled.schema and "@@trigger(" not in pulled.schema
    assert [s.summary for s in pulled.differences] == []


@pytest.mark.skipif(not has_extension("monorm.history"), reason="requires the history package")
async def test_a_version_model_with_scope_pulls_back_with_the_scope_column_as_a_field(tmp_path):
    models = 'model BundleVersion {\n id Int @id @default(autoincrement())\n bundle_id Int\n @@history.versions(of: bundle_id, scope: [user])\n @@map("ps_bundle_versions")\n}'
    pulled = await pulled_from("postgresql", tmp_path, models)
    # the scope column sits in the version table, so the database shows it as a plain column and no difference is left to see
    assert "@@history.versions(of: bundle_id)" in pulled.schema and "scope:" not in pulled.schema
    assert "user String?" in pulled.schema
    assert pulled.gaps == []
    assert [s.summary for s in pulled.differences] == []


@pytest.mark.skipif(not has_extension("monorm.history", "monorm.soft-delete"), reason="requires the history and soft-delete packages")
async def test_a_failing_rule_claim_is_named_when_object_claims_were_tried_with_it(tmp_path):
    registry = Registry()
    monorm.loads('datasource db { provider = "postgresql" }\nmodel Post {\n id Int @id @default(autoincrement())\n title String\n @@history.track\n @@map("pl_posts")\n}', registry=registry)
    db = await monorm.connect(PULL_URL, max_connections=2, default=False, registry=registry)
    try:
        await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        await db.create_tables()
        # the rule claim fails to compile: its field name `_deleted_at` is taken
        await db.execute(
            """
CREATE TABLE sdm_x (id serial PRIMARY KEY, deleted_at timestamptz, _deleted_at timestamptz);
CREATE RULE soft_delete AS ON DELETE TO sdm_x
  WHERE COALESCE(current_setting('rules.hard_deletion', true), '') <> 'on'
  DO INSTEAD (UPDATE sdm_x SET _deleted_at = NOW() WHERE "id" = old."id" AND _deleted_at IS NULL);
"""
        )
        pulled = await pull(db)
        # the first attempt holds both kinds of claim; the gap names both, and the rule claims are tried again alone
        assert any(g.startswith("object claims skipped, the schema with the rule and object claims does not compile") for g in pulled.gaps)
        assert any(g.startswith("rule claims skipped, the schema with them does not compile") for g in pulled.gaps)
        assert "@@history.track" not in pulled.schema
        assert [s.summary for s in pulled.differences] == []
    finally:
        await db.close()


@pytest.mark.parametrize("dialect", ["postgresql", "sqlite"])
async def test_services_share_unmanaged_models(tmp_path, dialect):
    postgres = dialect == "postgresql"
    url = PULL_URL if postgres else f"sqlite://{tmp_path / 'shared.db'}"
    datasource = f'datasource db {{ provider = "{dialect}" }}\n'
    (tmp_path / "shared.prisma").write_text("model Account {\n id Int @id\n label String\n}")
    owner = tmp_path / "owner.prisma"
    owner.write_text(datasource + 'import "shared.prisma" (prefix: "shared_")\n')
    consumer = tmp_path / "consumer.prisma"
    consumer_source = datasource + '''import "shared.prisma" (prefix: "shared_", managed: false)
model Task {
  id Int @id
  account_id Int
  account Account @relation(fields: [account_id], references: [id])
}
'''
    consumer.write_text(consumer_source)
    owner_migrations = Migrations(tmp_path / "owner", owner)
    initial = owner_migrations.make()
    registry = Registry()
    Account = monorm.load(consumer, registry=registry)["Account"]
    db = await monorm.connect(url, max_connections=1, default=False, registry=registry)
    try:
        if postgres:
            await db.execute(
                "DROP SCHEMA IF EXISTS managed_owner CASCADE; DROP SCHEMA IF EXISTS managed_consumer CASCADE; "
                "CREATE SCHEMA managed_owner; CREATE SCHEMA managed_consumer; SET search_path TO managed_owner"
            )
            await Migrator(db, owner_migrations).upgrade()
            await db.execute("SET search_path TO managed_consumer, managed_owner")
        else:
            # SQLite has one namespace. Apply the owner's generated DDL before starting the consumer ledger.
            await db.execute(initial.up_sql)
        await db.execute("CREATE INDEX outside_index ON shared_account (label)")
        consumer_migrations = Migrations(tmp_path / "consumer", consumer)
        migration = consumer_migrations.make()
        assert 'REFERENCES "shared_account"' in migration.up_sql
        assert 'CREATE TABLE "shared_account"' not in migration.up_sql
        await Migrator(db, consumer_migrations).upgrade()

        accounts = Account.objects.using(db)
        await accounts.insert(id=1, label="shared")
        assert await accounts.all().update(label="updated") == 1
        await db.execute("INSERT INTO task (id, account_id) VALUES (1, 1)")
        with pytest.raises(monorm.IntegrityError):
            await db.execute("INSERT INTO task (id, account_id) VALUES (2, 99)")
        consumer.write_text(consumer_source.replace("  id Int @id", "  id Int @id\n  note String?"))
        consumer_migrations.make()
        migrator = Migrator(db, consumer_migrations)
        await migrator.upgrade()
        drift = await migrator.drift()
        assert not drift.steps and not drift.gaps, drift
        index_query = (
            "SELECT indexname FROM pg_indexes WHERE schemaname = 'managed_owner' AND indexname = 'outside_index'"
            if postgres else "SELECT name FROM sqlite_schema WHERE name = 'outside_index'"
        )
        assert await db._fetch_text(index_query) == [("outside_index",)]
        assert (await Account.objects.using(db).get()).label == "updated"
    finally:
        await db.close()
