"""Type-checks a SQL view model: each `# E:` line must fail and no other line may."""

import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from monorm.__main__ import main as cli

ROOT = Path(__file__).resolve().parents[3]
SCHEMA = """\
model User {
  id   Int @id
  name String
}

view ActiveUser {
  id   Int @id
  name String
}
"""
# One function per refused call: a call that returns `Never` ends its flow, and a checker skips what follows.
USE = """\
from .models import ActiveUser, User, UserInsert


async def reads(row: ActiveUser) -> None:
    await ActiveUser.objects.filter(ActiveUser.id == 1).all()
    await User.objects.insert(id=1, name="a")
    _ = UserInsert


def insert() -> None:
    ActiveUser.objects.insert(id=1, name="a")  # E:


def insert_many() -> None:
    ActiveUser.objects.insert_many([{"id": 1, "name": "a"}])  # E:


async def get_or_insert() -> None:
    await ActiveUser.objects.get_or_insert(id=1, name="a")  # E:


def update() -> None:
    ActiveUser.objects.update(name="a")  # E:


def update_many() -> None:
    ActiveUser.objects.update_many([{"id": 1, "name": "a"}])  # E:


async def delete() -> None:
    await ActiveUser.objects.delete()  # E:


async def instance_update(row: ActiveUser) -> None:
    await row.update(name="a")  # E:


async def instance_delete(row: ActiveUser) -> None:
    await row.delete()  # E:
"""
EXPECTED = {i for i, line in enumerate(USE.splitlines(), 1) if "# E:" in line}


@pytest.fixture(scope="module")
def package(tmp_path_factory: pytest.TempPathFactory) -> Path:
    root = tmp_path_factory.mktemp("sv-typing")
    pkg = root / "svpkg"
    pkg.mkdir()
    (pkg / "__init__.py").write_text("")
    (pkg / "use.py").write_text(USE)
    (root / "schema.prisma").write_text(SCHEMA)
    (root / "views" / "public").mkdir(parents=True)
    (root / "views" / "public" / "ActiveUser.sql").write_text("SELECT id, name FROM \"user\"")
    code = cli(["--schema", str(root / "schema.prisma"), "generate", "python", "-o", str(pkg / "models.py")])
    assert code == 0
    return pkg


def error_lines(output: str, pattern: str) -> set[int]:
    return {int(m.group(1)) for m in re.finditer(pattern, output, re.M)}


def test_mypy(package: Path):
    pytest.importorskip("mypy")
    out = subprocess.run(
        [sys.executable, "-m", "mypy", "--strict", "--no-incremental", str(package / "use.py")],
        capture_output=True,
        text=True,
        env={"MYPYPATH": f"{ROOT / 'languages/python'}:{package.parent}", "PATH": ""},
        cwd=package.parent,
    ).stdout
    assert error_lines(out, r"use\.py:(\d+): error") == EXPECTED, out


def test_pyright(package: Path):
    exe = shutil.which("pyright")
    if exe is None:
        pytest.skip("pyright not installed")
    out = subprocess.run(
        [exe, str(package / "use.py")],
        capture_output=True,
        text=True,
        cwd=package.parent,
        env={"HOME": os.environ["HOME"], "PATH": str(Path(exe).parent), "PYTHONPATH": str(ROOT / "languages/python")},
    ).stdout
    assert error_lines(out, r"use\.py:(\d+):\d+ - error") == EXPECTED, out
