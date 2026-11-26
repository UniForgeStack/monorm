"""`[apps]`: the command line reads the apps of the settings file, and finds an installed package that it names with `app:<package>`."""

from pathlib import Path

import pytest

from monorm.__main__ import main as cli

SHOP = 'model Order {\n id Int @id\n}\n'


def project(root: Path, apps: str) -> None:
    (root / "monorm.toml").write_text(f'[database]\nprovider = "sqlite"\n[apps]\n{apps}')
    (root / "shop.prisma").write_text(SHOP)


def test_the_apps_table_compiles_a_bundled_app_and_names_a_wrong_label(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capfd: pytest.CaptureFixture[str]) -> None:
    monkeypatch.chdir(tmp_path)
    project(tmp_path, 'shop = "shop.prisma"\nauth = "app:monorm-auth"\n')
    assert cli(["check"]) == 0
    # the package declares the label `auth`: the project cannot relabel it
    project(tmp_path, 'shop = "shop.prisma"\nidentity = "app:monorm-auth"\n')
    capfd.readouterr()
    assert cli(["check"]) == 1
    error = capfd.readouterr().err
    assert "`auth`" in error and "`identity`" in error
    # a prefix on an installed app is a setup error
    project(tmp_path, 'shop = "shop.prisma"\nauth = { path = "app:monorm-auth", prefix = "x_" }\n')
    capfd.readouterr()
    assert cli(["check"]) == 2
    assert "not allowed on an installed app" in capfd.readouterr().err
