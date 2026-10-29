#!/usr/bin/env python3
"""Check the release files against the exact package lists, and split the PyPI files.

usage: publish-lists.py check pypi|npm <dir> <version>
       publish-lists.py split-pypi <dir> <out-dir>
`<version>` is `0.1.0`, `0.1.0rc1` or `0.1.0-rc.1`.
`split-pypi` moves the thin `monorm` files to `<out-dir>/monorm` and all other files to `<out-dir>/deps`.
"""
import json
import re
import shutil
import sys
import tarfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from versions import NATIVE as PROFILES, forms  # noqa: E402

EXTENSIONS = ("composition", "file-storage", "generic", "inherit", "locking", "proxy", "query-defaults", "soft-delete", "timestamps")
PLATFORMS = ("darwin-arm64", "linux-x64", "win32-x64")
# Each native wheel exists for 3 operating systems and 4 Python versions.
NATIVE_WHEELS = 12

PYPI = {
    "monorm": ("wheel", "sdist"),
    "monorm-auth": ("wheel", "sdist"),
    **{f"monorm-native-{p}": ("wheel",) * NATIVE_WHEELS for p in PROFILES},
    **{f"monorm-{e}": ("wheel",) for e in EXTENSIONS},
}
NPM = sorted(
    ["@ufstack/monorm", "@ufstack/monorm-auth"]
    + [f"@ufstack/monorm-native-{p}" for p in PROFILES]
    + [f"@ufstack/monorm-native-{p}-{o}" for p in PROFILES for o in PLATFORMS]
    + [f"@ufstack/monorm-{e}" for e in EXTENSIONS]
)


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


def split_pypi(directory: Path, out: Path) -> None:
    for path in sorted(directory.iterdir()):
        target = out / ("monorm" if pypi_file(path)[0] == "monorm" else "deps")
        target.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target / path.name)


def main(argv: list[str]) -> int:
    if len(argv) == 4 and argv[0] == "check" and argv[1] in ("pypi", "npm"):
        problems = (check_pypi if argv[1] == "pypi" else check_npm)(Path(argv[2]), argv[3])
        count = len(PYPI) if argv[1] == "pypi" else len(NPM)
        print("\n".join(problems) if problems else f"{argv[1]}: {count} packages, all present")
        return 1 if problems else 0
    if len(argv) == 3 and argv[0] == "split-pypi":
        split_pypi(Path(argv[1]), Path(argv[2]))
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
