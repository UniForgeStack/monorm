"""Fail when a host source names an extension (owner rule 5).

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
    "languages/python/native",
    "languages/js/native",
]
MANIFESTS = ["extensions/*/orm-extension.toml", "tests/outside-extension/orm-extension.toml"]
EDGE = "A-Za-z0-9_-"
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
            if not path.is_file() or "tests" in relative.parts or path.name == "tests.rs":
                continue
            if "target" in relative.parts or path.suffix not in {".rs", ".toml", ".py", ".json"}:
                continue
            text = path.read_text(errors="replace")
            if path.suffix == ".rs":
                # An inline test module is the last item of a file; a lone `#[cfg(test)]` item is not cut.
                text = TEST_MODULE.split(text, 1)[0]
            lines = list(enumerate(text.splitlines(), 1))
            for number, line in lines:
                if any(p.search(line) for p in full_patterns):
                    hits.append(f"{relative}:{number}: {line.strip()}")
            if path.name == "Cargo.toml":
                for number, line in feature_lines(text):
                    if any(p.search(line) for p in feature_patterns):
                        hits.append(f"{relative}:{number}: {line.strip()}")
    return sorted(set(hits))


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
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
