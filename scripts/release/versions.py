#!/usr/bin/env python3
"""Check or set the release version at every site that carries it.

usage: versions.py check [<version>] [--allow-tbd] [--root <dir>]
       versions.py set <version> [--root <dir>]
       versions.py show [--root <dir>]
       versions.py tag <tag>

`<version>` is `0.1.0`, or a release candidate in either form: `0.1.0rc1` (PEP 440) or `0.1.0-rc.1` (semver).
Python sites hold the PEP 440 form. The npm, Cargo, extension manifest and native metadata sites hold the semver form.
`check` without a version takes the one in the root `pyproject.toml`.
`check` also needs a `## [<semver>]` heading in CHANGELOG.md, except for a release candidate.
`show` prints the version of the root `pyproject.toml` in the semver form.
`tag` prints the version of a release tag, `v0.1.0` or `v0.1.0-rc.1`, and fails for any other tag, such as `v0.1.0rc1`.
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
VERSION = re.compile(r"(\d+\.\d+\.\d+)(?:rc(\d+)|-rc\.(\d+))?")
TAG = re.compile(r"v(\d+\.\d+\.\d+(?:-rc\.\d+)?)")
NATIVE = ("postgres", "sqlite", "combined", "tooling")
DEPENDENCY_SECTIONS = ("dependencies", "peerDependencies", "optionalDependencies")
VERSION_LINE = r'^version = "([^"]+)"'
# The crates whose version `set` changes; Cargo.lock holds each of them.
VERSIONED_CRATES = ("monorm-contracts", "monorm-python", "monorm-node")
LOCK_ENTRY = r'^name = "{}"\nversion = "([^"]+)"'
# (path or glob, form, pattern, expected matches per file)
TEXT_SITES = [
    ("pyproject.toml", "py", VERSION_LINE, 1),
    ("pyproject.toml", "py", r'"monorm-[a-z-]+==([^"]+)"', 13),
    *[(f"languages/python/packaging/{p}/pyproject.toml", "py", VERSION_LINE, 1) for p in NATIVE],
    ("apps/auth/python/pyproject.toml", "py", VERSION_LINE, 1),
    ("extensions/*/monorm-extension.toml", "semver", VERSION_LINE, 1),
    ("crates/contracts/Cargo.toml", "semver", VERSION_LINE, 1),
    ("languages/python/native/Cargo.toml", "semver", VERSION_LINE, 1),
    ("languages/js/native/Cargo.toml", "semver", VERSION_LINE, 1),
    *[("Cargo.lock", "semver", LOCK_ENTRY.format(crate), 1) for crate in VERSIONED_CRATES],
    ("tests/outside-extension/Cargo.lock", "semver", LOCK_ENTRY.format("monorm-contracts"), 1),
    ("languages/python/monorm/_profiles.py", "semver", r'get\("version"\) != "([^"]+)"', 1),
    ("languages/python/monorm/_profiles.py", "semver", r"matching monorm (\d[^ ]*) packages", 1),
    ("languages/js/src/native.ts", "semver", r'meta\.version !== "([^"]+)"', 1),
    ("languages/js/src/native.ts", "semver", r"matching monorm (\d[^ ]*) packages", 1),
    ("languages/js/packaging/build-node-profile.mjs", "semver", r'version: "([^"]+)"', 1),
]
JSON_SITES = [
    "languages/js/package.json",
    "languages/js/package-lock.json",
    "languages/js/unscoped/package.json",
    "apps/auth/typescript/package.json",
    "apps/auth/typescript/package-lock.json",
    *[f"languages/js/packaging/{p}/package.json" for p in NATIVE],
]


def parse(text: str) -> tuple[str, int | None]:
    match = VERSION.fullmatch(text.removeprefix("v"))
    if not match:
        raise SystemExit(f"not a version: {text!r}; use 0.1.0, 0.1.0rc1 or 0.1.0-rc.1")
    return match[1], int(match[2] or match[3]) if match[2] or match[3] else None


def forms(text: str) -> dict[str, str]:
    base, rc = parse(text)
    return {"py": base if rc is None else f"{base}rc{rc}", "semver": base if rc is None else f"{base}-rc.{rc}"}


def tree_version(root: Path) -> str:
    match = re.search(VERSION_LINE, (root / "pyproject.toml").read_text(), re.M)
    if not match:
        raise SystemExit("pyproject.toml: no version line")
    return match[1]


def tag_version(tag: str) -> str:
    match = TAG.fullmatch(tag)
    if not match:
        raise SystemExit(f"not a release tag: {tag!r}; use v0.1.0 or v0.1.0-rc.1")
    return match[1]


def text_files(root: Path):
    for pattern, form, regex, count in TEXT_SITES:
        paths = sorted(root.glob(pattern))
        if not paths:
            raise SystemExit(f"no file matches {pattern}")
        for path in paths:
            yield path, form, re.compile(regex, re.M), count


def json_slots(data: dict):
    """Yield (container, key) for every version slot of a package.json or package-lock.json.

    A lock file also holds the root package and the packages that `file:` dependencies link to (their paths start with `../`).
    """
    nodes = [data]
    for path, node in data.get("packages", {}).items():
        if path == "" or (path.startswith("../") and node.get("name", "").startswith("@ufstack/monorm")):
            nodes.append(node)
    for node in nodes:
        yield node, "version"
        for section in DEPENDENCY_SECTIONS:
            for name, value in node.get(section, {}).items():
                if name.startswith("@ufstack/monorm") and value[:1].isdigit():
                    yield node[section], name


def read_all(root: Path) -> list[tuple[str, str, str]]:
    """Every (file, form, value) found."""
    found = []
    for path, form, regex, count in text_files(root):
        values = regex.findall(path.read_text())
        if len(values) != count:
            raise SystemExit(f"{path.relative_to(root)}: expected {count} match of {regex.pattern}, found {len(values)}")
        found += [(path.relative_to(root).as_posix(), form, value) for value in values]
    for name in JSON_SITES:
        data = json.loads((root / name).read_text())
        found += [(name, "semver", container[key]) for container, key in json_slots(data)]
    return found


def check(version: str, root: Path, allow_tbd: bool) -> int:
    wanted = forms(version)
    wrong = [f"{file}: {value} (want {wanted[form]})" for file, form, value in read_all(root) if value != wanted[form]]
    _, rc = parse(version)
    if rc is None:
        heading = re.search(rf"^## \[{re.escape(wanted['semver'])}\](.*)$", (root / "CHANGELOG.md").read_text(), re.M)
        if not heading:
            wrong.append(f"CHANGELOG.md: no heading ## [{wanted['semver']}]")
        elif "TBD" in heading[1] and not allow_tbd:
            wrong.append(f"CHANGELOG.md: the date of [{wanted['semver']}] is TBD")
    print("\n".join(wrong) if wrong else f"every site is at {version}")
    return 1 if wrong else 0


def set_all(version: str, root: Path) -> int:
    wanted = forms(version)
    for path, form, regex, _ in text_files(root):
        path.write_text(regex.sub(lambda m: m[0].replace(m[1], wanted[form]), path.read_text()))
    for name in JSON_SITES:
        path = root / name
        data = json.loads(path.read_text())
        for container, key in json_slots(data):
            container[key] = wanted["semver"]
        path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")
    print(f"set every site to {version}")
    return 0


def main(argv: list[str]) -> int:
    root = ROOT
    if "--root" in argv:
        index = argv.index("--root")
        root = Path(argv[index + 1])
        del argv[index : index + 2]
    allow_tbd = "--allow-tbd" in argv
    command, *args = [a for a in argv if a != "--allow-tbd"] or [""]
    if command == "check" and len(args) <= 1:
        return check(args[0] if args else tree_version(root), root, allow_tbd)
    if command == "set" and len(args) == 1:
        return set_all(args[0], root)
    if command == "show" and not args:
        print(forms(tree_version(root))["semver"])
        return 0
    if command == "tag" and len(args) == 1:
        print(tag_version(args[0]))
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
