"""Runs mypy and pyright over languages/python/tests/typing/check_models.py.

Every line marked `# E:` must produce an error and no other line may.
"""

import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[3]
CHECKS = [ROOT / "languages/python/tests/typing" / name for name in ("check_models.py", "check_nested.py")]


def error_lines(output: str, pattern: str) -> set[int]:
    return {int(m.group(1)) for m in re.finditer(pattern, output, re.M)}


def expected(check: Path) -> set[int]:
    return {i for i, line in enumerate(check.read_text().splitlines(), 1) if "# E:" in line}


@pytest.mark.parametrize("check", CHECKS, ids=lambda c: c.stem)
def test_mypy(check):
    pytest.importorskip("mypy")
    out = subprocess.run(
        [sys.executable, "-m", "mypy", "--strict", "--no-incremental", str(check)],
        capture_output=True,
        text=True,
        env={"MYPYPATH": f"{ROOT / 'languages/python'}:{ROOT / 'examples'}", "PATH": ""},
        cwd=ROOT,
    ).stdout
    assert error_lines(out, rf"{check.name}:(\d+): error") == expected(check), out


@pytest.mark.parametrize("check", CHECKS, ids=lambda c: c.stem)
def test_pyright(check):
    exe = shutil.which("pyright")
    if exe is None:
        pytest.skip("pyright not installed")
    out = subprocess.run([exe, str(check)], capture_output=True, text=True, cwd=check.parent).stdout
    assert error_lines(out, rf"{check.name}:(\d+):\d+ - error") == expected(check), out
