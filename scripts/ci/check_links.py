#!/usr/bin/env python3
"""Check that every relative link in a tracked Markdown file points to a file or directory in the tree."""

import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parents[2]
LINK = re.compile(r"(?<!\!)\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
SKIP = ("http://", "https://", "mailto:", "#")
# The rename fixtures keep old names and broken links on purpose.
IGNORED = ("scripts/tests/rename/fixtures/",)


def markdown_files() -> list[str]:
    out = subprocess.run(["git", "ls-files", "*.md"], cwd=ROOT, capture_output=True, text=True, check=True)
    return [p for p in out.stdout.splitlines() if not p.startswith(IGNORED)]


def broken_links(relative: str) -> list[str]:
    path = ROOT / relative
    if not path.exists():
        return []
    broken, fenced = [], False
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if line.lstrip().startswith(("```", "~~~")):
            fenced = not fenced
        if fenced:
            continue
        for target in LINK.findall(re.sub(r"`[^`]*`", "", line)):
            if target.startswith(SKIP):
                continue
            file = unquote(target.split("#", 1)[0])
            if file and not (path.parent / file).exists():
                broken.append(f"{relative}:{number}: {target}")
    return broken


def main() -> int:
    broken = [item for relative in markdown_files() for item in broken_links(relative)]
    print("\n".join(broken))
    return 1 if broken else 0


if __name__ == "__main__":
    sys.exit(main())
