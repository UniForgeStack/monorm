"""Fail when the `orm[full]` extra differs from the first-party extension manifests.

    python languages/python/packaging/check-full-extra.py [repo-root]
"""

import sys
import tomllib
from pathlib import Path

EXCLUDED = {"orm.file-storage", "file-storage"}  # file storage stays out of orm[full]; the bare id is the id before it is renamed
NATIVE = "orm-native-tooling==0.1.0"


def expected(root: Path) -> set[str]:
    members = {NATIVE}
    for manifest in sorted((root / "extensions").glob("*/orm-extension.toml")):
        data = tomllib.loads(manifest.read_text())
        if data["id"] in EXCLUDED:
            continue
        dist = data["id"].replace(".", "-").replace("_", "-")
        members.add(f"{dist}=={data['version']}")
    return members


def check(root: Path) -> list[str]:
    declared = set(tomllib.loads((root / "pyproject.toml").read_text())["project"]["optional-dependencies"]["full"])
    wanted = expected(root)
    return [f"missing from orm[full]: {name}" for name in sorted(wanted - declared)] + [
        f"not a first-party member of orm[full]: {name}" for name in sorted(declared - wanted)
    ]


if __name__ == "__main__":
    problems = check(Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parents[3]))
    if problems:
        sys.exit("\n".join(problems))
    print("orm[full] matches the first-party manifests")
