#!/usr/bin/env python3
"""Fail when a Linux library needs a glibc symbol version above the floor.

usage: check-glibc.py <file-or-wheel>...
A `.whl` is opened and each `.so` inside it is checked.
"""
import re
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

FLOOR = (2, 28)
SYMBOL = re.compile(r"GLIBC_(\d+)\.(\d+)")


def highest(library: Path) -> tuple[int, int]:
    text = subprocess.run(["objdump", "-T", str(library)], check=True, capture_output=True, text=True).stdout
    return max(((int(a), int(b)) for a, b in SYMBOL.findall(text)), default=(0, 0))


def libraries(path: Path, scratch: Path):
    if path.suffix != ".whl":
        yield path
        return
    with zipfile.ZipFile(path) as archive:
        for name in archive.namelist():
            if name.endswith(".so") or ".so." in name:
                yield Path(archive.extract(name, scratch))


def main(paths: list[str]) -> int:
    if not paths:
        print(__doc__, file=sys.stderr)
        return 2
    failed = False
    with tempfile.TemporaryDirectory() as scratch:
        for argument in paths:
            found = False
            for library in libraries(Path(argument), Path(scratch)):
                found = True
                version = highest(library)
                ok = version <= FLOOR
                failed |= not ok
                print(f"{'ok  ' if ok else 'FAIL'} {argument}: {library.name} needs glibc {version[0]}.{version[1]} (floor {FLOOR[0]}.{FLOOR[1]})")
            if not found:
                failed = True
                print(f"FAIL {argument}: no library found")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
