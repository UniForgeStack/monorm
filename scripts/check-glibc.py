#!/usr/bin/env python3
"""Fail when a Linux library needs a glibc symbol version above the floor.

usage: check-glibc.py [--libc glibc|musl] <file-or-wheel>...
A `.whl` is opened and each `.so` inside it is checked.
With `--libc musl` a library fails on any `GLIBC_` symbol and must list `libc.so` or `libc.musl-<arch>.so.1` as a needed library.
"""
from __future__ import annotations

import re
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

FLOOR = (2, 28)
SYMBOL = re.compile(r"GLIBC_(\d+)\.(\d+)")
# zig links a musl library against `libc.so` (musl maps that name to itself); a musl toolchain names `libc.musl-<arch>.so.1`.
MUSL_LOADER = re.compile(r"NEEDED\s+(libc\.so|libc\.musl-(x86_64|aarch64)\.so\.1)(\s|$)")


def highest(library: Path) -> tuple[int, int]:
    text = subprocess.run(["objdump", "-T", str(library)], check=True, capture_output=True, text=True).stdout
    return max(((int(a), int(b)) for a, b in SYMBOL.findall(text)), default=(0, 0))


def musl_problem(library: Path) -> str | None:
    """Why this library is not a musl build, or None."""
    if (version := highest(library)) != (0, 0):
        return f"needs glibc {version[0]}.{version[1]}; a musl library has no GLIBC_ symbol"
    text = subprocess.run(["objdump", "-p", str(library)], check=True, capture_output=True, text=True).stdout
    if not MUSL_LOADER.search(text):
        return "does not need libc.so or libc.musl-<arch>.so.1"
    return None


def libraries(path: Path, scratch: Path):
    if path.suffix != ".whl":
        yield path
        return
    with zipfile.ZipFile(path) as archive:
        for name in archive.namelist():
            if name.endswith(".so") or ".so." in name:
                yield Path(archive.extract(name, scratch))


def main(arguments: list[str]) -> int:
    libc = "glibc"
    if arguments[:1] == ["--libc"]:
        libc = arguments[1] if len(arguments) > 1 else ""
        arguments = arguments[2:]
    if libc not in ("glibc", "musl") or not arguments:
        print(__doc__, file=sys.stderr)
        return 2
    failed = False
    with tempfile.TemporaryDirectory() as scratch:
        for argument in arguments:
            found = False
            for library in libraries(Path(argument), Path(scratch)):
                found = True
                if libc == "musl":
                    problem = musl_problem(library)
                    failed |= problem is not None
                    print(f"{'ok  ' if problem is None else 'FAIL'} {argument}: {library.name} {problem or 'is a musl library'}")
                    continue
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
