"""Schema files and migration generation: no database needed."""

import json
from pathlib import Path

import pytest

import monorm
from monorm import Registry, SchemaError
from monorm.__main__ import main as cli
from monorm.migrations import Migrations
from _planned import planned, planned_up

ROOT = Path(__file__).resolve().parents[3]

V1 = """
model Author {
  id    BigInt @id @default(autoincrement())
  email String @unique @db.VarChar(254)
  books Book[]

  @@map("authors")
}

model Book {
  id        BigInt @id @default(autoincrement())
  author_id BigInt
  title     String @db.VarChar(200)
  pages     Int    @default(0)
  author    Author @relation(fields: [author_id], references: [id], onDelete: Cascade)

  @@index([author_id])
  @@map("books")
}
"""

# v1 + an extension type, a renamed column, new columns, indexes, constraints, a trigger
V2 = r"""
model Author {
  id    BigInt @id @default(autoincrement())
  email String @unique @db.Citext
  books Book[]

  @@map("authors")
}

model Book {
  id         BigInt   @id @default(autoincrement())
  author_id  BigInt
  name       String   @renamed_from("title") @db.VarChar(200)
  pages      Int      @default(0) @check("pages >= 0")
  meta       Json?    @comment("free-form attributes")
  updated_at DateTime @default(now())
  author     Author   @relation(fields: [author_id], references: [id], onDelete: Cascade)

  @@index([author_id])
  @@index([author_id, updated_at(sort: Desc)], where: raw("pages > 0"))
  @@index([name(ops: raw("gin_trgm_ops"))], type: Gin)
  @@unique([author_id, name])
  @@map("books")
  @@trigger(touch, when: before, operation: [update], func: "BEGIN\n    NEW.updated_at := now();\n    RETURN NEW;\nEND;")
}
"""


def models(source: str) -> Registry:
    reg = Registry()
    monorm.loads(source, registry=reg)
    return reg


def test_loads_builds_models_from_schema_text():
    reg = models(V2)
    Book = reg.get("Book")
    assert Book._meta.table == "books"
    assert list(Book._meta.fields) == ["id", "author_id", "name", "pages", "meta", "updated_at"]
    assert Book._meta.fields["meta"].nullable
    assert Book._meta.fields["updated_at"].has_server_value
    assert Book._meta.relations["author"].target is reg.get("Author")
    # schema objects travel in the IR untouched by Python
    ir = Book._meta.ir()
    assert ir["triggers"][0]["body"].startswith("BEGIN\n    NEW.updated_at")
    assert ir["indexes"][1] == {"columns": [{"field": "name", "opclass": "gin_trgm_ops"}], "method": "gin"}
    email = reg.get("Author")._meta.ir()["fields"][1]
    assert email["db_type"] == "citext" and email["write_sql"] == "CAST({} AS citext)"


def test_schema_errors_name_the_line():
    with pytest.raises(SchemaError, match=r"<schema>:3:9: Book.title: unknown type Strin"):
        monorm.loads("model Book {\n  id    BigInt @id\n  title Strin\n}")
    with pytest.raises(SchemaError, match="unknown argument `wher`"):
        monorm.loads('model Book {\n  id BigInt @id\n  @@index([id], wher: "x")\n}')


def test_the_first_migration(tmp_path):
    migs = Migrations(tmp_path, models(V1))
    m = migs.make()
    assert m.name == "0001_initial"
    assert m.path == tmp_path / "0001_initial.json"
    assert [next(iter(op)) for op in m.operations] == ["create_table", "create_table", "create_index"]
    up = m.up_sql
    assert up.index('CREATE TABLE "authors"') < up.index('CREATE TABLE "books"')
    assert (
        'CONSTRAINT "books_author_id_fkey" FOREIGN KEY ("author_id") REFERENCES "authors" ("id") ON DELETE CASCADE'
        in up
    )
    assert 'CREATE INDEX "books_author_id_idx" ON "books" ("author_id");' in up
    assert 'DROP TABLE "books";' in m.down_sql
    assert json.loads(m.path.read_text())["version"] == 1
    assert sorted(p.name for p in tmp_path.iterdir()) == ["0001_initial.json"]
    assert migs.make() is None  # nothing changed


def test_the_second_migration_alters_in_place(tmp_path):
    Migrations(tmp_path, models(V1)).make()
    migs = Migrations(tmp_path, models(V2))
    plan = migs.plan()
    sql, down = planned(migs)
    assert sql[0] == 'CREATE EXTENSION IF NOT EXISTS "citext"'
    assert sql[1] == 'CREATE EXTENSION IF NOT EXISTS "pg_trgm"'
    assert sql[2].startswith('CREATE OR REPLACE FUNCTION "books_touch"() RETURNS trigger')
    assert 'ALTER TABLE "books" RENAME COLUMN "title" TO "name"' in sql
    assert 'ALTER TABLE "books" ADD COLUMN "updated_at" timestamp with time zone DEFAULT now() NOT NULL' in sql
    assert 'ALTER TABLE "authors" ALTER COLUMN "email" TYPE citext USING "email"::citext' in sql
    assert 'ALTER TABLE "books" ADD CONSTRAINT "books_pages_check" CHECK (pages >= 0)' in sql
    assert 'CREATE INDEX "books_name_idx" ON "books" USING gin ("name" gin_trgm_ops)' in sql
    assert sql[-2].startswith('CREATE TRIGGER "touch" BEFORE UPDATE ON "books" FOR EACH ROW EXECUTE FUNCTION "books_touch"()')
    assert sql[-1] == 'COMMENT ON COLUMN "books"."meta" IS \'free-form attributes\''
    assert any("changes authors.email from varchar(254) to citext" in w for w in plan.warnings)
    assert 'ALTER TABLE "books" RENAME COLUMN "name" TO "title"' in down
    assert down[-3:] == [
        'DROP FUNCTION "books_touch"()',
        'DROP EXTENSION IF EXISTS "citext"',
        'DROP EXTENSION IF EXISTS "pg_trgm"',
    ]
    m = migs.make("catalog changes")
    assert m.name == "0002_catalog_changes"
    assert "-- WARNING: changes authors.email" in m.up_sql
    assert migs.make() is None


def test_plan_operations_say_which_ones_lose_data(tmp_path):
    first = Migrations(tmp_path, models(V1))
    assert not any(o.destructive for o in first.plan().operations)
    first.make()
    plan = Migrations(tmp_path, models(V2)).plan()
    flagged = [o.summary for o in plan.operations if o.destructive]
    assert flagged == ["change type of authors.email to citext"]
    assert all(o.atomic for o in plan.operations)
    m = Migrations(tmp_path, models(V2)).make("catalog")
    assert "-- WARNING: changes authors.email" in m.up_sql
    # the gate comes from the operations: no marker line is written
    assert "monorm:" not in m.up_sql and "monorm:" not in m.down_sql
    assert "monorm:" not in m.path.read_text()


def test_table_rename_renames_generated_constraint_names(tmp_path):
    Migrations(tmp_path, models(V1)).make()
    v1b = V1.replace('@@map("authors")', '@@map("writers")\n  @@renamed_from("authors")')
    up = planned_up(Migrations(tmp_path, models(v1b)))
    assert up == [
        'ALTER TABLE "authors" RENAME TO "writers"',
        'ALTER TABLE "writers" RENAME CONSTRAINT "authors_pkey" TO "writers_pkey"',
        'ALTER TABLE "writers" RENAME CONSTRAINT "authors_email_key" TO "writers_email_key"',
    ]


def test_kept_rename_hints_do_nothing_after_the_rename(tmp_path):
    renamed = V1.replace('@@map("authors")', '@@map("writers")\n  @@renamed_from("authors")').replace(
        "  title     String @db.VarChar(200)", '  name      String @renamed_from("title") @db.VarChar(200)'
    )
    Migrations(tmp_path, models(V1)).make()
    Migrations(tmp_path, models(renamed)).make()
    later = Migrations(
        tmp_path, models(renamed.replace("  pages     Int", "  added     Int    @default(42)\n  pages     Int"))
    )
    assert planned_up(later) == ['ALTER TABLE "books" ADD COLUMN "added" integer DEFAULT 42 NOT NULL']


def test_migrations_from_a_schema_file(tmp_path):
    (tmp_path / "schema.prisma").write_text(V1)
    m = Migrations(tmp_path / "migrations", tmp_path / "schema.prisma").make()
    assert m.path == tmp_path / "migrations" / "0001_initial.json"
    assert m.path.is_file()


def test_the_generated_models_are_current():
    module, stub = monorm._native.generate_python(str(ROOT / "examples/blog/schema.prisma"))
    committed = (ROOT / "examples/blog/models.py").read_text()
    assert committed == module, "run `python -m monorm generate`"
    assert (ROOT / "examples/blog/models.pyi").read_text() == stub, "run `python -m monorm generate`"
    nested = ROOT / "examples/nested"
    module, stub = monorm._native.generate_python(str(nested / "schema.prisma"))
    assert (nested / "models.py").read_text() == module, f"run `python -m monorm generate` in {nested}"
    assert (nested / "models.pyi").read_text() == stub, f"run `python -m monorm generate` in {nested}"


def test_the_cli(tmp_path, capfd, monkeypatch):
    (tmp_path / "schema.prisma").write_text(V1)
    monkeypatch.chdir(tmp_path)
    assert cli(["check"]) == 0
    assert cli(["generate", "-o", "app/models.py"]) == 0
    assert "class Book(Model):" in (tmp_path / "app/models.pyi").read_text()
    assert cli(["makemigrations", "--check"]) == 1
    assert cli(["makemigrations"]) == 0
    assert "Created " in capfd.readouterr().out
    assert cli(["makemigrations", "--check"]) == 0
    assert cli(["sqlmigrate", "1"]) == 0
    assert 'CREATE TABLE "books"' in capfd.readouterr().out
    (tmp_path / "schema.prisma").write_text("model X {")
    assert cli(["check"]) == 1
    assert "schema.prisma:1:1: model X is not closed" in capfd.readouterr().err


ONLINE_SHOP = 'model Shop {\n  id   BigInt @id @default(autoincrement())\n  name String\n%s\n  @@map("shop")\n}\n'
ONLINE_PLAIN = ONLINE_SHOP % ""
ONLINE_INDEXED = ONLINE_SHOP % "  @@index([name])\n"
ONLINE_ITEM = 'model Item {\n  id      BigInt @id @default(autoincrement())\n  shop_id BigInt\n%s\n  @@map("item")\n}\n'
ONLINE_SHOP_WITH_ITEMS = 'model Shop {\n  id    BigInt @id @default(autoincrement())\n  name  String\n  items Item[]\n\n  @@map("shop")\n}\n'
ONLINE_ITEM_FK = ONLINE_ITEM % "  shop  Shop   @relation(fields: [shop_id], references: [id])\n"


def test_online_makemigrations_runs_an_index_on_an_existing_table_outside_a_transaction(tmp_path):
    Migrations(tmp_path, models(ONLINE_PLAIN), online=True).make()
    migs = Migrations(tmp_path, models(ONLINE_INDEXED), online=True)
    plan = migs.plan()
    assert [(o.summary, o.atomic) for o in plan.operations] == [("create index shop_name_idx on shop", False)]
    first = migs.make("add_idx")
    assert [m.name for m in migs.all()] == ["0001_initial", "0002_add_idx"]  # one change, one file
    assert first.operations[0]["atomic"] is False
    assert first.up_sql.startswith("-- 0002_add_idx (up)\n\n-- outside a transaction\n")
    assert 'CREATE INDEX CONCURRENTLY IF NOT EXISTS "shop_name_idx" ON "shop" ("name");' in first.up_sql
    assert "DROP INDEX CONCURRENTLY IF EXISTS" in first.down_sql
    assert migs.make() is None  # nothing changed
    # a removed index and a new column: the index drop runs alone, the column in a group of its own
    gone = Migrations(tmp_path, models(ONLINE_SHOP % "  note String?\n"), online=True)
    swapped = gone.make("swap")
    assert [m.name for m in gone.all()][-1] == "0003_swap"
    assert [(next(iter(op)), op.get("atomic", True)) for op in swapped.operations] == [("drop_index", False), ("add_column", True)]
    assert not gone.plan()


def test_online_makemigrations_adds_a_foreign_key_in_the_main_group(tmp_path):
    v1 = ONLINE_PLAIN + ONLINE_ITEM % ""
    Migrations(tmp_path, models(v1), online=True).make()
    v2 = ONLINE_SHOP_WITH_ITEMS + ONLINE_ITEM_FK
    migs = Migrations(tmp_path, models(v2), online=True)
    plan = migs.plan()
    assert [o.atomic for o in plan.operations] == [True]
    up, down = planned(migs)
    assert len(up) == 1 and "FOREIGN KEY" in up[0] and "NOT VALID" not in up[0], up
    assert down == ['ALTER TABLE "item" DROP CONSTRAINT "item_shop_id_fkey"']
    written = migs.make("add_fk")
    assert [m.name for m in migs.all()] == ["0001_initial", "0002_add_fk"]  # one file, not two
    assert [next(iter(op)) for op in written.operations] == ["add_constraint"]
    assert not migs.plan()
    assert not Migrations(tmp_path, models(v2)).plan()  # the file ends in the same schema


def test_online_makemigrations_leaves_a_new_table_alone(tmp_path):
    # first migration: a new table with an index and a foreign key stays in one folder
    migs = Migrations(tmp_path, models(ONLINE_SHOP_WITH_ITEMS + ONLINE_ITEM_FK), online=True)
    made = migs.make()
    assert [m.name for m in migs.all()] == ["0001_initial"]
    assert "CONCURRENTLY" not in made.up_sql and "NOT VALID" not in made.up_sql
    assert "FOREIGN KEY" in made.up_sql
    off = tmp_path / "off"
    off.mkdir()
    plain = Migrations(off, models(ONLINE_SHOP_WITH_ITEMS + ONLINE_ITEM_FK)).make()
    assert plain.up_sql == made.up_sql and plain.down_sql == made.down_sql
    # a table added next to an existing one, with an index: still one folder
    more = ONLINE_SHOP_WITH_ITEMS + ONLINE_ITEM_FK + 'model Tag {\n  id   BigInt @id\n  name String\n\n  @@index([name])\n  @@map("tag")\n}\n'
    migs2 = Migrations(tmp_path, models(more), online=True)
    tag = migs2.make()
    assert len(migs2.all()) == 2
    assert 'CREATE INDEX "tag_name_idx" ON "tag" ("name")' in tag.up_sql
    assert "CONCURRENTLY" not in tag.up_sql


def test_online_makemigrations_changes_nothing_on_sqlite(tmp_path):
    source = 'datasource db { provider = "sqlite" }\n'
    on, off = tmp_path / "on", tmp_path / "off"
    on.mkdir()
    off.mkdir()
    for directory, flag in ((on, True), (off, False)):
        Migrations(directory, models(source + ONLINE_PLAIN), online=flag).make()
        Migrations(directory, models(source + ONLINE_INDEXED), online=flag).make()
    names = [m.name for m in Migrations(on, Registry()).all()]
    assert names == [m.name for m in Migrations(off, Registry()).all()] and len(names) == 2
    for a, b in zip(Migrations(on, Registry()).all(), Migrations(off, Registry()).all(), strict=True):
        assert a.path.read_text() == b.path.read_text()
    assert "atomic" not in (on / f"{names[1]}.json").read_text()


def test_the_cli_online_setting(tmp_path, capfd, monkeypatch):
    (tmp_path / "schema.prisma").write_text(ONLINE_PLAIN)
    (tmp_path / "monorm.toml").write_text("[migrations]\nonline = true\n")
    monkeypatch.chdir(tmp_path)
    assert cli(["makemigrations"]) == 0
    (tmp_path / "schema.prisma").write_text(ONLINE_INDEXED)
    assert cli(["makemigrations", "--check"]) == 1
    assert cli(["makemigrations"]) == 0
    assert "0002_create_index_shop_name_idx_on_shop" in capfd.readouterr().out
    assert cli(["makemigrations", "--check"]) == 0
    written = next((tmp_path / "migrations").glob("0002_*.json")).read_text()
    assert '"atomic": false' in written


@pytest.mark.case("migrations::node_launcher_converts_native_expressions_offline_and_preserves_pending_changes")
def test_the_launcher_converts_native_expressions_offline_and_preserves_pending_changes(tmp_path):
    import subprocess
    import sys

    path = tmp_path / "schema.prisma"
    old = 'model Item {\n id Int @id\n qty Int @check(raw("qty > 0"))\n}\n'
    path.write_text(old)

    def run(*args):
        return subprocess.run([sys.executable, "-m", "monorm", "--schema", str(path), "--dir", str(tmp_path / "migrations"), *args], capture_output=True, text=True)

    assert run("makemigrations", "initial").returncode == 0
    snapshot = (tmp_path / "migrations/0001_initial.json").read_text()
    path.write_text(old.replace(" qty Int", " note String?\n qty Int"))
    result = run("fmt", "--native")
    assert result.returncode == 0, result.stderr
    converted = path.read_text()
    assert "note String?" in converted
    assert "@check((qty > 0))" in converted
    assert run("makemigrations", "--check").returncode == 1
    assert run("fmt", "--native").returncode == 0
    assert path.read_text() == converted
    assert (tmp_path / "migrations/0001_initial.json").read_text() == snapshot
