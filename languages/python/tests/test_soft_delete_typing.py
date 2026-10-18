"""Type-checks the generated soft-delete members: each `# E:` line must fail and no other line may.

Runs with an artifact that selects the soft-delete extension.
"""

import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from orm import _native

ROOT = Path(__file__).resolve().parents[3]
CAPABILITIES = set(json.loads(_native.native_artifact())["capabilities"])
SCHEMA = """\
model Post {
  id      Int       @id
  title   String
  deleted DateTime? @soft_delete.deleted_at
}

model Tag {
  id Int @id
}
"""
USE = """\
from .models import Post, Tag


async def go(post: Post) -> None:
    await Post.objects.deleted_only().all()
    await Post.objects.all_with_deleted().filter(Post.id == 1).all()
    await Post.objects.hard_delete()
    await post.undelete()
    await post.hard_delete()
    Tag.objects.deleted_only()  # E:
    Tag.objects.all_with_deleted()  # E:
    await Tag.objects.hard_delete()  # E:
"""
EXPECTED = {i for i, line in enumerate(USE.splitlines(), 1) if "# E:" in line}

pytestmark = pytest.mark.skipif(
    "soft-delete" not in CAPABILITIES and "soft_delete" not in CAPABILITIES,
    reason="requires a selected soft-delete native artifact",
)


@pytest.fixture(scope="module")
def package(tmp_path_factory: pytest.TempPathFactory) -> Path:
    root = tmp_path_factory.mktemp("sd-typing")
    pkg = root / "sdpkg"
    pkg.mkdir()
    (pkg / "__init__.py").write_text("")
    (pkg / "use.py").write_text(USE)
    (root / "schema.prisma").write_text(SCHEMA)
    code = _native.cli(["--schema", str(root / "schema.prisma"), "generate", "python", "-o", str(pkg / "models.py")])
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
        env={"PATH": str(Path(exe).parent), "PYTHONPATH": str(ROOT / "languages/python")},
    ).stdout
    assert error_lines(out, r"use\.py:(\d+):\d+ - error") == EXPECTED, out
