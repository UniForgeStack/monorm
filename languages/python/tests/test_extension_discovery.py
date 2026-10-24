"""Extension packages: the Python discovery through the `orm.extensions` entry point.

The fake extensions have ids that no real package uses, so the tests also pass in a venv with real extensions installed.
"""

import shutil
import subprocess
from pathlib import Path

import pytest

import orm
from orm.__main__ import main as cli
from orm._native import SchemaError
from orm._packages import resolve_extensions

MANIFEST = 'id = "acme.fixture"\nversion = "0.1.0"\nabi = "0.1"\nlibrary = "libfixture"\n'


def install(site: Path, dist: str, entry: str | None, *, manifest: bool = True, ident: str | None = None) -> Path:
    """A package `<dist>` with `orm-extension.toml`, and a hand-written `.dist-info` that declares the entry point."""
    package = dist.replace("-", "_")
    (site / package).mkdir(parents=True)
    (site / package / "__init__.py").write_text("raise RuntimeError('an extension package must not be imported')\n")
    if manifest:
        (site / package / "orm-extension.toml").write_text(MANIFEST)
    info = site / f"{package}-0.1.dist-info"
    info.mkdir()
    (info / "METADATA").write_text(f"Metadata-Version: 2.1\nName: {dist}\nVersion: 0.1\n")
    if entry is not None:
        (info / "entry_points.txt").write_text(f"[orm.extensions]\n{ident or 'acme.fixture'} = {entry}\n")
    return site / package / "orm-extension.toml"


@pytest.fixture
def site(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    site = tmp_path / "site-packages"
    site.mkdir()
    monkeypatch.syspath_prepend(str(site))
    return site


def test_an_installed_extension_resolves_by_id_without_importing_its_package(site: Path) -> None:
    manifest = install(site, "acme-fixture", "acme_fixture")
    assert resolve_extensions()["acme.fixture"] == str(manifest)


def test_a_dotted_module_resolves_to_its_directory(site: Path) -> None:
    (site / "acme_ns" / "inner").mkdir(parents=True)
    (site / "acme_ns" / "__init__.py").write_text("")
    (site / "acme_ns" / "inner" / "orm-extension.toml").write_text(MANIFEST)
    info = site / "acme_ns-0.1.dist-info"
    info.mkdir()
    (info / "METADATA").write_text("Metadata-Version: 2.1\nName: acme-ns\nVersion: 0.1\n")
    (info / "entry_points.txt").write_text("[orm.extensions]\nacme.ns = acme_ns.inner\n")
    assert resolve_extensions()["acme.ns"] == str(site / "acme_ns" / "inner" / "orm-extension.toml")


def test_a_package_without_a_manifest_names_the_entry_point(site: Path) -> None:
    install(site, "acme-fixture", "acme_fixture", manifest=False)
    with pytest.raises(SchemaError, match=r"entry point of `acme.fixture`.*orm-extension.toml"):
        resolve_extensions()


def test_two_packages_with_one_id_name_both_paths(site: Path) -> None:
    first = install(site, "acme-fixture", "acme_fixture")
    second = install(site, "acme-fixture-two", "acme_fixture_two")
    with pytest.raises(SchemaError) as error:
        resolve_extensions()
    assert "installed twice" in str(error.value)
    assert str(first) in str(error.value) and str(second) in str(error.value)


OUTSIDE = Path(__file__).resolve().parents[3] / "tests" / "outside-extension"


@pytest.fixture(scope="session")
def outside_library(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """The library of `tests/outside-extension`, built in its own workspace."""
    target = tmp_path_factory.mktemp("outside-target")
    subprocess.run(
        ["cargo", "build", "--offline", "--quiet", "--manifest-path", str(OUTSIDE / "Cargo.toml"), "--target-dir", str(target)],
        check=True,
    )
    return next(p for p in (target / "debug").glob("liboutside_extension.*") if p.suffix in {".dylib", ".so", ".dll"})


def test_an_installed_outside_library_lowers_the_schema_through_load_and_loads(
    site: Path, outside_library: Path, tmp_path: Path
) -> None:
    manifest = install(site, "acme-outside", "acme_outside", ident="acme.outside")
    manifest.write_text((OUTSIDE / "orm-extension.toml").read_text())
    shutil.copy(outside_library, manifest.parent / outside_library.name)
    source = 'model Post {\n id Int @id\n @@outside.tag("blue")\n}\n'
    assert orm.loads(source, registry=orm.Registry())["Post"]._meta.fields["tag"].default == "blue"
    schema = tmp_path / "schema.prisma"
    schema.write_text(source)
    assert "tag" in orm.load(schema, registry=orm.Registry())["Post"]._meta.fields


def test_the_command_line_lowers_the_schema_with_an_installed_outside_library(
    site: Path, outside_library: Path, tmp_path: Path, capfd: pytest.CaptureFixture[str], monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = install(site, "acme-outside", "acme_outside", ident="acme.outside")
    manifest.write_text((OUTSIDE / "orm-extension.toml").read_text())
    shutil.copy(outside_library, manifest.parent / outside_library.name)
    (tmp_path / "schema.prisma").write_text('model Post {\n id Int @id\n @@outside.tag("blue")\n}\n')
    monkeypatch.chdir(tmp_path)
    assert cli(["check"]) == 0
    assert cli(["makemigrations"]) == 0
    capfd.readouterr()
    assert cli(["sqlmigrate", "1"]) == 0
    assert '"tag"' in capfd.readouterr().out
