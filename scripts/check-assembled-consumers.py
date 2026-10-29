#!/usr/bin/env python3
"""Main seam on one platform: install the assembled packages in a clean environment and run the consumers.

usage: check-assembled-consumers.py <dist-dir> <dialect,...>
`<dist-dir>` holds the wheels and npm tarballs of check-assembled-packages.py. Works on Linux, macOS and Windows.
"""
import glob
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
dist, dialects = Path(sys.argv[1]).resolve(), sys.argv[2].split(",")
TSC = ROOT / "languages/js/node_modules/typescript/bin/tsc"
SCHEMA = (ROOT / "scripts/tests/package_consumer.prisma").read_text()


def run(*command, cwd=None, env=None):
    print("+", *command, flush=True)
    subprocess.run([str(c) for c in command], cwd=cwd, env={**os.environ, **(env or {})}, check=True)


def tool(name):
    path = shutil.which(name)
    if path is None:
        raise SystemExit(f"{name} is not on PATH")
    return path


(ROOT / "target").mkdir(exist_ok=True)
work = Path(tempfile.mkdtemp(prefix="assembled-seam-", dir=ROOT / "target"))
npm, node, uv = tool("npm"), tool("node"), tool("uv")
wheels = work / "wheels"
run(uv, "build", "--wheel", "--out-dir", wheels, ROOT)
run(tool("maturin"), "build", "--out", wheels, cwd=ROOT / "languages/python/packaging/tooling")
run(npm, "run", "build:native", "--", "--profile", "tooling", cwd=ROOT / "languages/js")
run(node, TSC, "-p", "tsconfig.json", cwd=ROOT / "languages/js")
venv = work / "venv"
run(uv, "venv", venv)
python = venv / ("Scripts/python.exe" if sys.platform == "win32" else "bin/python")
run(uv, "pip", "install", "--python", python, *glob.glob(str(wheels / "*.whl")), *glob.glob(str(dist / "*.whl")))
# The tooling native package for Node is a shim over the addon of the checkout.
native = work / "native"
native.mkdir()
(native / "index.cjs").write_text("module.exports = require('./monorm.node');\n")
shutil.copy2(ROOT / "languages/js/monorm.node", native)
version = json.loads((ROOT / "languages/js/package.json").read_text())["version"]  # the peer range of @ufstack/monorm names this version
(native / "package.json").write_text(json.dumps({"name": "@ufstack/monorm-native-tooling", "version": version, "main": "index.cjs"}))
tarballs = work / "tarballs"
tarballs.mkdir()
for directory in (ROOT / "languages/js", native):
    run(npm, "pack", "--pack-destination", tarballs, "--silent", cwd=directory)
js = work / "js"
js.mkdir()
(js / "package.json").write_text('{"private":true,"type":"module"}\n')
run(npm, "install", "--ignore-scripts", "--no-audit", "--no-fund", *glob.glob(str(tarballs / "*.tgz")), *glob.glob(str(dist / "*.tgz")), cwd=js)
bun = tool("bun")
for dialect in dialects:
    consumer = js / dialect
    consumer.mkdir()
    (consumer / "schema.prisma").write_text(SCHEMA.replace("__DIALECT__", dialect))
    models_py, models_ts = consumer / "models.py", consumer / "models.ts"
    run(python, "-m", "monorm", "--schema", consumer / "schema.prisma", "generate", "python", "-o", models_py, cwd=js)
    run(node, "node_modules/@ufstack/monorm/dist/src/cli.js", "--schema", consumer / "schema.prisma", "generate", "typescript", "-o", models_ts, cwd=js)
    run(node, TSC, models_ts, "--outDir", consumer, "--module", "ESNext", "--target", "ES2022", "--moduleResolution", "Bundler", "--skipLibCheck", "--noCheck")
    shutil.copy2(ROOT / "scripts/tests/package_consumer.py", consumer / "use.py")
    shutil.copy2(ROOT / "scripts/tests/package_consumer.mjs", consumer / "use.mjs")
    env = {"MONORM_DATABASE_URL": "sqlite://:memory:" if dialect == "sqlite" else os.environ["ORM_TEST_DATABASE_URL"]}
    run(python, "use.py", cwd=consumer, env=env)
    run(node, "use.mjs", cwd=consumer, env=env)
    run(bun, "use.mjs", cwd=consumer, env=env)
shutil.rmtree(work, ignore_errors=True)
