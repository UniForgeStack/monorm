"""A proxy of a model inherits its root's extension members and its root's QuerySet class.

Runs with an artifact that selects the proxy-models and soft-delete extensions.
Each `# E:` line of the typing use must fail and no other line may.
"""

import importlib
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from artifact_caps import artifact_capabilities, has_extension
from orm import _native

ROOT = Path(__file__).resolve().parents[3]
SCHEMA = """\
model PmPost {
  id      Int       @id
  title   String
  deleted DateTime? @soft_delete.deleted_at
  @@map("proxy_members_post")
}

model PmDraft {
  @@proxy.of(PmPost)
  @@map("proxy_members_post")
}

model PmNote {
  id Int @id
  @@map("proxy_members_note")
}
"""
QUERIES = """\
from typing_extensions import Self

from .models import PmPost, PmPostQuerySet


class PmQueries(PmPostQuerySet):
    def titled(self, title: str) -> Self:
        return self.filter(PmPost.title == title)
"""
USE = """\
from typing_extensions import assert_type

from .models import PmDraft, PmNote, PmPost


async def go(draft: PmDraft, post: PmPost) -> None:
    assert_type(await PmDraft.objects.all_with_deleted().all(), list[PmDraft])
    assert_type(await PmDraft.objects.deleted_only().all(), list[PmDraft])
    assert_type(await PmDraft.objects.of(draft).hard_delete(), list[PmDraft])
    assert_type(await draft.qs().undelete(), list[PmDraft])
    assert_type(await PmPost.objects.titled("a").all_with_deleted().all(), list[PmPost])
    await PmDraft.objects.titled("a").all()
    await draft.undelete()
    PmNote.objects.deleted_only()  # E:
    PmNote.objects.of(draft).undelete()  # E:
"""
EXPECTED = {i for i, line in enumerate(USE.splitlines(), 1) if "# E:" in line}

pytestmark = pytest.mark.skipif(
    "proxy-models" not in artifact_capabilities() or not has_extension("orm.soft-delete"),
    reason="requires a native artifact with the proxy-models and soft-delete extensions",
)


@pytest.fixture(scope="module")
def package(tmp_path_factory: pytest.TempPathFactory) -> Path:
    root = tmp_path_factory.mktemp("proxy-members")
    pkg = root / "pmpkg"
    pkg.mkdir()
    (pkg / "__init__.py").write_text("")
    (pkg / "pq.py").write_text(QUERIES)
    (pkg / "use.py").write_text(USE)
    (root / "schema.prisma").write_text(SCHEMA)
    args = ["--schema", str(root / "schema.prisma"), "generate", "python", "-o", str(pkg / "models.py")]
    assert _native.cli([*args, "--query-set", "PmPost=pmpkg.pq:PmQueries"]) == 0
    return pkg


@pytest.fixture(scope="module")
def models(package: Path):
    sys.path.insert(0, str(package.parent))
    try:
        yield importlib.import_module("pmpkg.models")
    finally:
        sys.path.remove(str(package.parent))
        for name in [n for n in sys.modules if n == "pmpkg" or n.startswith("pmpkg.")]:
            del sys.modules[name]


def test_proxy_has_the_soft_delete_members_of_its_root(models):
    for member in ("all_with_deleted", "deleted_only", "undelete", "hard_delete"):
        assert hasattr(models.PmDraft.objects, member)
    assert hasattr(models.PmDraft, "undelete")
    assert not hasattr(models.PmNote.objects, "deleted_only")
    assert "deleted" in models.PmDraft.objects.sql()
    assert "deleted" not in models.PmDraft.objects.all_with_deleted().sql().split("FROM")[1]
    assert "IS NOT NULL" in models.PmDraft.objects.deleted_only().sql()


def test_proxy_takes_the_query_set_class_of_its_root(models):
    queries = importlib.import_module("pmpkg.pq").PmQueries
    assert isinstance(models.PmPost.objects.titled("a"), queries)
    assert isinstance(models.PmDraft.objects.titled("a"), queries)
    assert models.PmDraft.objects.titled("a")._model is models.PmDraft
    assert not hasattr(models.PmNote.objects, "titled")


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
