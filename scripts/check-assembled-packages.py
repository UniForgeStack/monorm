#!/usr/bin/env python3
"""Check the assembled outside extension package (`acme.outside`) and pack its npm package.

usage: check-assembled-packages.py <assembled-dir> <dist-dir>
`<assembled-dir>/<extension>/` holds the output of `monorm-extension-package assemble`.
"""
import json
import os
import shutil
import subprocess
import sys
import tarfile
import zipfile
from pathlib import Path

LIBRARIES = (".dylib", ".so", ".dll")
TAG = "py3-none-macosx_11_0_arm64.manylinux_2_28_x86_64.win_amd64"
assembled, dist = Path(sys.argv[1]), Path(sys.argv[2])
dist.mkdir(parents=True, exist_ok=True)
summary = []
for directory in sorted(p for p in assembled.iterdir() if p.is_dir()):
    wheel, = directory.glob("*.whl")
    if not wheel.name.endswith(f"-{TAG}.whl"):
        raise SystemExit(f"{wheel.name}: the tag must be {TAG}")
    with zipfile.ZipFile(wheel) as archive:
        names = archive.namelist()
        record = archive.read(next(n for n in names if n.endswith(".dist-info/RECORD"))).decode()
    libraries = sorted(n for n in names if n.endswith(LIBRARIES))
    if {Path(n).suffix for n in libraries} != set(LIBRARIES) or any(n not in record for n in libraries):
        raise SystemExit(f"{wheel.name}: needs one library per platform in the wheel and RECORD, has {libraries}")
    npm = directory / "npm"
    package = json.loads((npm / "package.json").read_text())
    if "os" in package or "cpu" in package:
        raise SystemExit(f"{package['name']}: an npm package with 3 libraries has no os or cpu field")
    npm_exe = shutil.which("npm")
    packed = subprocess.run([npm_exe, "pack", "--pack-destination", str(dist.resolve()), "--silent"], cwd=npm, check=True, capture_output=True, text=True).stdout.split()[-1]
    with tarfile.open(dist / packed) as tar:
        packed_libraries = sorted(n for n in tar.getnames() if n.endswith(LIBRARIES))
    if {Path(n).suffix for n in packed_libraries} != set(LIBRARIES):
        raise SystemExit(f"{packed}: needs one library per platform, has {packed_libraries}")
    shutil.copy2(wheel, dist)
    summary += [f"{wheel.name}: {wheel.stat().st_size} bytes", f"{packed}: {(dist / packed).stat().st_size} bytes"]
text = "\n".join(summary)
print(text)
if os.environ.get("GITHUB_STEP_SUMMARY"):
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as out:
        out.write("```\n" + text + "\n```\n")
