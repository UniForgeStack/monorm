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
SCHEMA = """datasource db {{ provider = "{dialect}" }}
model Base {{
  id Int @id
  @@inherit.abstract
  @@timestamps.updated_at("changed", mode: "application")
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@locking.version("version")
}}
model Note {{
  text String
  start Int
  end Int
  shop_id Int
  updated_by String?
  note Json? @outside.wrapped("w:")
  @@inherit.extends(Base)
  @@outside.tag("installed")
  @@query.filter("positive", "id > 0")
  @@outside.guard
  @@map("pkg1d_seam_notes")
}}
"""


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
(native / "index.cjs").write_text("module.exports = require('./orm.node');\n")
shutil.copy2(ROOT / "languages/js/orm.node", native)
(native / "package.json").write_text(json.dumps({"name": "@orm/native-tooling", "version": "0.1.0", "main": "index.cjs"}))
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
    (consumer / "schema.prisma").write_text(SCHEMA.format(dialect=dialect))
    models_py, models_ts = consumer / "models.py", consumer / "models.ts"
    run(python, "-m", "orm", "--schema", consumer / "schema.prisma", "generate", "python", "-o", models_py, cwd=js)
    run(node, "node_modules/orm/dist/src/cli.js", "--schema", consumer / "schema.prisma", "generate", "typescript", "-o", models_ts, cwd=js)
    run(node, TSC, models_ts, "--outDir", consumer, "--module", "ESNext", "--target", "ES2022", "--moduleResolution", "Bundler", "--skipLibCheck", "--noCheck")
    shutil.copy2(ROOT / "scripts/tests/package_consumer.py", consumer / "use.py")
    shutil.copy2(ROOT / "scripts/tests/package_consumer.mjs", consumer / "use.mjs")
    env = {"ORM_DATABASE_URL": "sqlite://:memory:" if dialect == "sqlite" else os.environ["ORM_TEST_DATABASE_URL"]}
    run(python, "use.py", cwd=consumer, env=env)
    run(node, "use.mjs", cwd=consumer, env=env)
    run(bun, "use.mjs", cwd=consumer, env=env)
shutil.rmtree(work, ignore_errors=True)
