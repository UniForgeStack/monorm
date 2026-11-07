"""`monorm admin init` in a project directory, and the copies of the `app:admin` schema."""

from pathlib import Path

from monorm.__main__ import main as cli

ROOT = Path(__file__).resolve().parents[3]

SCHEMA = 'model Book {\n  id    BigInt @id @default(autoincrement())\n  title String\n\n  @@map("books")\n}\n'


def test_the_admin_schema_copies_of_both_packages_match_the_source():
    source = (ROOT / "apps/admin/schema.prisma").read_text()
    assert (ROOT / "apps/admin/python/monorm_admin/schema.prisma").read_text() == source
    assert (ROOT / "languages/js/admin/schema.prisma").read_text() == source


def test_admin_init_writes_the_module_and_the_settings_and_the_schema_still_checks(tmp_path, capfd, monkeypatch):
    (tmp_path / "schema.prisma").write_text(SCHEMA)
    monkeypatch.chdir(tmp_path)
    assert cli(["admin", "init", "--models", "books"]) == 0
    out = capfd.readouterr().out
    assert "next: makemigrations, migrate" in out
    module = (tmp_path / "admin.py").read_text()
    assert "from books import Book" in module and "admin.register(Book)" in module
    assert (tmp_path / "monorm.toml").read_text() == '[admin]\napp = "admin:admin"\n'
    assert 'import "app:monorm-admin" (prefix: "admin_")' in (tmp_path / "schema.prisma").read_text()
    assert cli(["check"]) == 0
    assert "ok (3 models)" in capfd.readouterr().out
    assert cli(["admin", "init", "--models", "books"]) == 2
    assert "already have an admin section" in capfd.readouterr().err
