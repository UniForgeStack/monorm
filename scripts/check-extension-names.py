"""Fail when a host source names an extension, or an extension source names another extension (owner rule 5).

Usage: python3 scripts/check-extension-names.py [ROOT]
The names come from the manifests, so a new extension needs no edit here.
"""

import re
import sys
import tomllib
from pathlib import Path

SCANNED = [
    "crates/core",
    "crates/engine",
    "crates/contracts",
    "crates/cli",
    "crates/admin",
    "languages/python/native",
    "languages/js/native",
    "languages/python/monorm",
    "languages/js/src",
]
MANIFESTS = ["extensions/*/monorm-extension.toml", "tests/outside-extension/monorm-extension.toml"]
# The one host line that tells a user which extension supplies a bare `Generic` type.
ALLOWED = [
    ("crates/core/src/dsl/lower.rs", "Generic belongs to the built-in generic extension"),
    # The Python admin names the package that holds the files of a `files` field; the message tells the user what to install.
    ("languages/python/monorm/admin.py", "monorm_file_storage"),
    # `monorm.scope(...)` and `scope.<name>` are the runtime call and the filter variable of the host, not the extension.
    ("languages/python/monorm/db.py", "monorm_scope"),
    ("languages/python/monorm/db.py", "monorm.scope("),
    ("languages/python/monorm/prepared_write.py", "monorm.scope()"),
    ("languages/python/monorm/query.py", "monorm.scope()"),
    ("languages/python/monorm/select.py", "monorm.scope()"),
    # `query.filter(...)` is a method call on a variable named `query`.
    ("languages/js/src/auth/methods.ts", "query.filter("),
]
# `monorm init` offers the built-in extensions by name, so its catalog is the one host file that lists them.
CATALOGS = {"crates/cli/src/init.rs"}
EDGE = "A-Za-z0-9_-"
# The folders of an extension that hold its own code: Rust, and the Python and TypeScript packages.
EXTENSION_SOURCES = ("src", "python", "typescript")
TEST_MODULE = re.compile(r"#\[cfg\(test\)\]\s*mod\s")


def derived_names(manifest: dict) -> tuple[set[str], set[str]]:
    """Return (full names, Cargo feature names) of one manifest."""
    ident = manifest["id"]
    scope, _, short = ident.partition(".")
    if not short:
        raise ValueError(f"extension id {ident!r} is not a dotted name")
    names = {
        ident,
        ident.replace(".", "-").replace("_", "-"),
        ident.replace(".", "_").replace("-", "_"),
        f"@{scope}/{short}",
    }
    if "library" in manifest:
        names.add(manifest["library"])
    names.update(a["name"] for a in manifest.get("attributes", []) if "name" in a)
    return names, {short}


def read_names(root: Path) -> tuple[set[str], set[str]]:
    names: set[str] = set()
    features: set[str] = set()
    for pattern in MANIFESTS:
        for path in sorted(root.glob(pattern)):
            n, f = derived_names(tomllib.loads(path.read_text()))
            names |= n
            features |= f
    return names, features


def word(name: str) -> re.Pattern[str]:
    return re.compile(rf"(?<![{EDGE}]){re.escape(name)}(?![{EDGE}])")


def feature_lines(text: str):
    """Yield (line number, line) inside the [features] table of a Cargo.toml."""
    inside = False
    for number, line in enumerate(text.splitlines(), 1):
        if line.startswith("["):
            inside = line.strip() == "[features]"
        elif inside:
            yield number, line


def scan(root: Path, names: set[str], features: set[str]) -> list[str]:
    full_patterns = [word(n) for n in sorted(names)]
    feature_patterns = [word(f) for f in sorted(features)]
    hits: list[str] = []
    for base in SCANNED:
        for path in sorted((root / base).rglob("*")):
            relative = path.relative_to(root)
            if not path.is_file() or "tests" in relative.parts or path.name == "tests.rs" or str(relative) in CATALOGS:
                continue
            if "target" in relative.parts or path.suffix not in {".rs", ".toml", ".py", ".pyi", ".ts", ".json"} or {"node_modules", "__pycache__", "dist"} & set(relative.parts):
                continue
            text = path.read_text(errors="replace")
            if path.suffix == ".rs":
                # An inline test module is the last item of a file; a lone `#[cfg(test)]` item is not cut.
                text = TEST_MODULE.split(text, 1)[0]
            lines = list(enumerate(text.splitlines(), 1))
            for number, line in lines:
                if any(p.search(line) for p in full_patterns) and not any(
                    str(relative) == f and needle in line for f, needle in ALLOWED
                ):
                    hits.append(f"{relative}:{number}: {line.strip()}")
            if path.name == "Cargo.toml":
                for number, line in feature_lines(text):
                    if any(p.search(line) for p in feature_patterns):
                        hits.append(f"{relative}:{number}: {line.strip()}")
    return sorted(set(hits))


def extension_names(root: Path) -> dict[str, set[str]]:
    """Return the full names of each extension crate, keyed by its directory."""
    return {
        path.parent.name: derived_names(tomllib.loads(path.read_text()))[0]
        for path in sorted(root.glob("extensions/*/monorm-extension.toml"))
    }


def scan_extensions(root: Path, owned: dict[str, set[str]]) -> list[str]:
    """Find a line of an extension source that names another extension. Test code may."""
    hits: list[str] = []
    for directory, own in owned.items():
        others = sorted(set().union(*(n for d, n in owned.items() if d != directory)) - own)
        patterns = [word(n) for n in others]
        sources = [p for folder in EXTENSION_SOURCES for p in (root / "extensions" / directory / folder).rglob("*") if p.suffix in {".rs", ".py", ".ts"}]
        for path in sorted(sources):
            relative = path.relative_to(root)
            if {"tests", "test", "node_modules", "__pycache__"} & set(relative.parts) or path.name == "tests.rs":
                continue
            text = path.read_text(errors="replace")
            cut = TEST_MODULE.search(text) if path.suffix == ".rs" else None
            # The package entry `monorm::extension!(...)` comes after the test module; its line is still extension code.
            tail = {text[: cut.start()].count("\n") + n for n, line in enumerate(text[cut.start():].splitlines(), 1) if "extension!(" in line} if cut else set()
            for number, line in enumerate(text.splitlines(), 1):
                if cut and number > text[: cut.start()].count("\n") and number not in tail:
                    continue
                if any(p.search(line) for p in patterns):
                    hits.append(f"{relative}:{number}: {line.strip()}")
    return hits


def main() -> int:
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[1]
    names, features = read_names(root)
    print("extension names:", ", ".join(sorted(names)))
    print("feature names:", ", ".join(sorted(features)))
    if not names:
        print("error: no extension manifest found")
        return 1
    hits = scan(root, names, features)
    for hit in hits:
        print(hit)
    if hits:
        print(f"error: {len(hits)} host line(s) name an extension")
    crossing = scan_extensions(root, extension_names(root))
    for hit in crossing:
        print(hit)
    if crossing:
        print(f"error: {len(crossing)} extension line(s) name another extension")
    return 1 if hits or crossing else 0


if __name__ == "__main__":
    sys.exit(main())
