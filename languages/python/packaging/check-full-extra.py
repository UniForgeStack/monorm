"""Fail when the `monorm[full]` extra differs from the first-party extension manifests.

    python languages/python/packaging/check-full-extra.py [repo-root]
"""

import sys
import tomllib
from pathlib import Path

EXCLUDED = {"monorm.file-storage", "file-storage"}  # file storage stays out of monorm[full]; the bare id is the id before it is renamed


def expected(root: Path, version: str) -> set[str]:
    members = {f"monorm-native-tooling=={version}"}
    for manifest in sorted((root / "extensions").glob("*/monorm-extension.toml")):
        data = tomllib.loads(manifest.read_text())
        if data["id"] in EXCLUDED:
            continue
        dist = data["id"].replace(".", "-").replace("_", "-")
        members.add(f"{dist}=={data['version'].replace('-rc.', 'rc')}")  # manifests hold semver; pins hold PEP 440
    return members


def check(root: Path) -> list[str]:
    project = tomllib.loads((root / "pyproject.toml").read_text())["project"]
    declared = set(project["optional-dependencies"]["full"])
    wanted = expected(root, project["version"])
    return [f"missing from monorm[full]: {name}" for name in sorted(wanted - declared)] + [
        f"not a first-party member of monorm[full]: {name}" for name in sorted(declared - wanted)
    ]


if __name__ == "__main__":
    problems = check(Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parents[3]))
    if problems:
        sys.exit("\n".join(problems))
    print("monorm[full] matches the first-party manifests")
