#!/usr/bin/env python3
"""Check or set the release version at every site that carries it.

usage: versions.py check <version> [--allow-tbd] [--root <dir>]
       versions.py set <version> [--root <dir>]

`<version>` is `0.1.0`, or a release candidate in either form: `0.1.0rc1` (PEP 440) or `0.1.0-rc.1` (semver).
Python sites hold the PEP 440 form. The npm, Cargo, extension manifest and native metadata sites hold the semver form.
`check` also needs a `## [<semver>]` heading in CHANGELOG.md, except for a release candidate.
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
VERSION = re.compile(r"(\d+\.\d+\.\d+)(?:rc(\d+)|-rc\.(\d+))?")
NATIVE = ("postgres", "sqlite", "combined", "tooling")
DEPENDENCY_SECTIONS = ("peerDependencies", "optionalDependencies")
VERSION_LINE = r'^version = "([^"]+)"'
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
    ("languages/python/monorm/_profiles.py", "semver", r'get\("version"\) != "([^"]+)"', 1),
    ("languages/python/monorm/_profiles.py", "semver", r"matching monorm (\d[^ ]*) packages", 1),
    ("languages/js/src/native.ts", "semver", r'meta\.version !== "([^"]+)"', 1),
    ("languages/js/src/native.ts", "semver", r"matching monorm (\d[^ ]*) packages", 1),
    ("languages/js/packaging/build-node-profile.mjs", "semver", r'version: "([^"]+)"', 1),
]
JSON_SITES = [
    "languages/js/package.json",
    "languages/js/package-lock.json",
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


def text_files(root: Path):
    for pattern, form, regex, count in TEXT_SITES:
        paths = sorted(root.glob(pattern))
        if not paths:
            raise SystemExit(f"no file matches {pattern}")
        for path in paths:
            yield path, form, re.compile(regex, re.M), count


def json_slots(data: dict):
    """Yield (container, key) for every version slot of a package.json or package-lock.json."""
    roots = [data] + ([data["packages"][""]] if "packages" in data else [])
    for node in roots:
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
    base, rc = parse(version)
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
    print(f"set every site to {version}; run `cargo update --workspace` to refresh Cargo.lock")
    return 0


def main(argv: list[str]) -> int:
    root = ROOT
    if "--root" in argv:
        index = argv.index("--root")
        root = Path(argv[index + 1])
        del argv[index : index + 2]
    allow_tbd = "--allow-tbd" in argv
    args = [a for a in argv if a != "--allow-tbd"]
    if len(args) != 2 or args[0] not in ("check", "set"):
        print(__doc__, file=sys.stderr)
        return 2
    return check(args[1], root, allow_tbd) if args[0] == "check" else set_all(args[1], root)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
