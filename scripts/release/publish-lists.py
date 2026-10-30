#!/usr/bin/env python3
"""Check the release files against the exact package lists.

usage: publish-lists.py check pypi|npm <dir> <version>
`<version>` is `0.1.0`, `0.1.0rc1` or `0.1.0-rc.1`.
A release is one PyPI project (16 wheels, no sdist) and one npm package.
"""
import json
import re
import sys
import tarfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from versions import forms  # noqa: E402

# The wheels of 4 platforms (Linux x86_64, Linux arm64, macOS arm64, Windows x64) and 4 Python versions. An sdist would lack the libraries and auth.
WHEELS = 16
PYPI = {"monorm": ("wheel",) * WHEELS}
NPM = ["@ufstack/monorm"]


def normalize(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def pypi_file(path: Path) -> tuple[str, str, str]:
    """Return (project, version, kind) of a wheel or an sdist."""
    if path.suffix == ".whl":
        name, version, *_ = path.name.split("-")
        return normalize(name), version, "wheel"
    if path.name.endswith(".tar.gz"):
        name, version = path.name.removesuffix(".tar.gz").rsplit("-", 1)
        return normalize(name), version, "sdist"
    raise SystemExit(f"{path.name}: not a wheel or an sdist")


def npm_file(path: Path) -> tuple[str, str]:
    with tarfile.open(path) as tar:
        member = tar.extractfile("package/package.json")
        if member is None:
            raise SystemExit(f"{path.name}: no package/package.json")
        data = json.load(member)
    return data["name"], data["version"]


def check_pypi(directory: Path, version: str) -> list[str]:
    wanted = forms(version)["py"]
    found: dict[str, list[str]] = {}
    problems = []
    for path in sorted(directory.iterdir()):
        project, file_version, kind = pypi_file(path)
        found.setdefault(project, []).append(kind)
        if file_version != wanted:
            problems.append(f"{path.name}: version {file_version}, want {wanted}")
    for project in sorted(set(found) | set(PYPI)):
        if project not in PYPI:
            problems.append(f"{project}: not a project of this release")
        elif sorted(found.get(project, [])) != sorted(PYPI[project]):
            problems.append(f"{project}: files {sorted(found.get(project, []))}, want {sorted(PYPI[project])}")
    return problems


def check_npm(directory: Path, version: str) -> list[str]:
    wanted = forms(version)["semver"]
    found: dict[str, list[str]] = {}
    problems = []
    for path in sorted(directory.glob("*.tgz")):
        name, file_version = npm_file(path)
        found.setdefault(name, []).append(path.name)
        if file_version != wanted:
            problems.append(f"{path.name}: version {file_version}, want {wanted}")
    for name in sorted(set(found) | set(NPM)):
        if name not in NPM:
            problems.append(f"{name}: not a package of this release ({found[name]})")
        elif len(found.get(name, [])) != 1:
            problems.append(f"{name}: {len(found.get(name, []))} files, want 1")
    return problems


def main(argv: list[str]) -> int:
    if len(argv) == 4 and argv[0] == "check" and argv[1] in ("pypi", "npm"):
        problems = (check_pypi if argv[1] == "pypi" else check_npm)(Path(argv[2]), argv[3])
        print("\n".join(problems) if problems else f"{argv[1]}: monorm, all files present")
        return 1 if problems else 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
