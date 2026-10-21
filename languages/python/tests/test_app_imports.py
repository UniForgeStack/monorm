"""`import "app:<name>"`: the Python resolution through the `orm.apps` entry point."""

import json
from pathlib import Path

import pytest

import orm
from orm import Registry, _native
from orm._apps import resolve_apps
from orm._native import SchemaError

PROJECT = 'datasource db {\n  provider = "sqlite"\n}\nimport "app:orm-auth" (prefix: "auth_")\nmodel Post {\n  id Int @id\n}\n'
AUTH = 'import "app:orm-billing" (prefix: "b_")\nmodel User {\n  id Int @id\n  @@query_set("UserQueries")\n}\n'
BILLING = "model Invoice {\n  id Int @id\n}\n"


def install(site: Path, dist: str, schema: str | None, entry: str | None, *, package: str | None = None) -> Path:
    """A package `<package>` with a schema, and a hand-written `<dist>-0.1.dist-info`."""
    package = package or dist.replace("-", "_")
    (site / package).mkdir(parents=True)
    (site / package / "__init__.py").write_text("raise RuntimeError('an app package must not be imported')\n")
    if schema is not None:
        (site / package / "schema.prisma").write_text(schema)
    info = site / f"{package}-0.1.dist-info"
    info.mkdir()
    (info / "METADATA").write_text(f"Metadata-Version: 2.1\nName: {dist}\nVersion: 0.1\n")
    if entry is not None:
        (info / "entry_points.txt").write_text(f"[orm.apps]\n{dist} = {entry}\n")
    return site / package / "schema.prisma"


@pytest.fixture
def site(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    site = tmp_path / "site-packages"
    site.mkdir()
    monkeypatch.syspath_prepend(str(site))
    return site


def project(tmp_path: Path, text: str = PROJECT) -> Path:
    schema = tmp_path / "schema.prisma"
    schema.write_text(text)
    return schema


def test_a_python_package_and_the_app_it_imports_resolve_without_importing_them(site: Path, tmp_path: Path) -> None:
    auth = install(site, "orm-auth", AUTH, "orm_auth:schema.prisma")
    billing = install(site, "orm-billing", BILLING, "orm_billing:schema.prisma")
    schema = project(tmp_path)
    apps = resolve_apps(schema)
    assert apps == {"orm-auth": str(auth), "orm-billing": str(billing)}
    ir = json.loads(_native.compile_schema_file(str(schema), apps))
    assert [m["table"] for m in ir["models"]] == ["auth_b_invoice", "auth_user", "post"]


def test_an_app_that_is_not_installed_is_left_for_the_loader_to_report(tmp_path: Path, site: Path) -> None:
    schema = project(tmp_path)
    assert resolve_apps(schema) == {}
    with pytest.raises(SchemaError, match="app:orm-auth.*not installed.*install the `orm-auth` package"):
        _native.compile_schema_file(str(schema), {})


def test_an_app_without_schema_metadata_names_the_missing_entry_point(site: Path, tmp_path: Path) -> None:
    install(site, "orm-auth", AUTH, None)
    with pytest.raises(SchemaError, match="`orm-auth` package declares no schema.*orm.apps"):
        resolve_apps(project(tmp_path))


def test_an_entry_point_that_names_no_file_is_an_error(site: Path, tmp_path: Path) -> None:
    install(site, "orm-auth", None, "orm_auth:schema.prisma")
    with pytest.raises(SchemaError, match="orm_auth:schema.prisma.*not a schema file"):
        resolve_apps(project(tmp_path))


def test_the_command_line_resolves_apps_before_it_runs_a_command(site: Path, tmp_path: Path, capfd: pytest.CaptureFixture[str]) -> None:
    from orm.__main__ import main

    schema = project(tmp_path)
    assert main(["--schema", str(schema), "check"]) == 1
    assert "not installed" in capfd.readouterr().err
    install(site, "orm-auth", AUTH, "orm_auth:schema.prisma")
    install(site, "orm-billing", BILLING, None)
    assert main(["--schema", str(schema), "check"]) == 1
    assert "`orm-billing` package declares no schema" in capfd.readouterr().err
    (site / "orm_billing-0.1.dist-info" / "entry_points.txt").write_text("[orm.apps]\norm-billing = orm_billing:schema.prisma\n")
    assert main(["--schema", str(schema), "check"]) == 0
    assert "(3 models)" in capfd.readouterr().out


def test_an_import_cycle_across_directories_is_scanned_once(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    (tmp_path / "a").mkdir()
    (tmp_path / "b").mkdir()
    (tmp_path / "a/x.prisma").write_text('import "../b/y.prisma"\n')
    (tmp_path / "b/y.prisma").write_text('import "../a/x.prisma"\n')
    reads: list[Path] = []
    read_text = Path.read_text

    def counting(self: Path, *args: object, **kwargs: object) -> str:
        reads.append(self)
        return read_text(self, *args, **kwargs)  # type: ignore[arg-type]

    monkeypatch.setattr(Path, "read_text", counting)
    assert resolve_apps(tmp_path / "a/x.prisma") == {}
    assert len(reads) == 2


def test_load_resolves_an_installed_app_and_loads_names_the_way_out(site: Path, tmp_path: Path) -> None:
    install(site, "orm-auth", AUTH, "orm_auth:schema.prisma")
    install(site, "orm-billing", BILLING, "orm_billing:schema.prisma")
    schema = project(tmp_path)
    models = orm.load(schema, registry=Registry())
    assert sorted(models) == ["Invoice", "Post", "User"]
    with pytest.raises(SchemaError, match=r'app:orm-auth.*use `load\(path\)` or generate'):
        orm.loads(PROJECT, registry=Registry())
