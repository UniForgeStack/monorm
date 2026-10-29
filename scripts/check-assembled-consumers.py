#!/usr/bin/env python3
"""Main seam on one platform: install the one `monorm` wheel and the one `monorm` tarball in fresh consumers and run them.

usage: check-assembled-consumers.py <dist-dir> <dialect,...>
`<dist-dir>` holds `monorm-*.whl`, `monorm-*.tgz` and the wheel and tarball of the outside extension `acme.outside`.
Works on Linux, macOS and Windows. A dialect is `sqlite` or `postgres` (the latter reads ORM_TEST_DATABASE_URL).
"""
import glob
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TESTS = ROOT / "scripts/tests"
dist, dialects = Path(sys.argv[1]).resolve(), sys.argv[2].split(",")
TSC = ROOT / "languages/js/node_modules/typescript/bin/tsc"
SCHEMA = (TESTS / "package_consumer.prisma").read_text()
DEMOS = ROOT / "examples/blog/demo"
# Python names an installed app by its distribution, Node by its bundled name.
APP = {"python": "monorm-auth", "node": "auth"}
COPY_SCHEMA = 'datasource db { provider = "sqlite" }\nmodel Base {\n  id Int @id\n  @@inherit.abstract\n  @@timestamps.updated_at("changed", mode: "application")\n}\nmodel Note {\n  text String\n  @@inherit.extends(Base)\n}\n'


def run(*command, cwd=None, env=None, expect_failure=None):
    """Runs a command. With `expect_failure`, the command must fail and print every given part."""
    print("+", *command, flush=True)
    result = subprocess.run([str(c) for c in command], cwd=cwd, env={**os.environ, **(env or {})}, capture_output=expect_failure is not None, text=True)
    if expect_failure is None:
        if result.returncode != 0:
            raise SystemExit(f"failed: {' '.join(str(c) for c in command)}")
        return
    output = result.stdout + result.stderr
    if result.returncode == 0:
        raise SystemExit(f"this command must fail: {' '.join(str(c) for c in command)}")
    for part in expect_failure:
        if part not in output:
            raise SystemExit(f"{part!r} is missing from the error:\n{output}")
    print("  failed as expected:", output.strip().splitlines()[-1], flush=True)


def tool(name):
    path = shutil.which(name)
    if path is None:
        raise SystemExit(f"{name} is not on PATH")
    return path


def one(pattern):
    found = glob.glob(str(dist / pattern))
    if len(found) != 1:
        raise SystemExit(f"{dist}: expected one file for {pattern}, found {found}")
    return found[0]


(ROOT / "target").mkdir(exist_ok=True)
work = Path(tempfile.mkdtemp(prefix="assembled-seam-", dir=ROOT / "target"))
npm, node, uv, bun = tool("npm"), tool("node"), tool("uv"), tool("bun")
monorm_wheel, monorm_tarball = one("monorm-*.whl"), one("monorm-*.tgz")
outside_wheel, outside_tarball = one("acme_outside-*.whl"), one("acme-outside-*.tgz")
copies = work / "copies"
copy_wheel, copy_npm = subprocess.run([sys.executable, TESTS / "builtin_copy.py", copies], check=True, capture_output=True, text=True).stdout.split("\n")[:2]


def venv(name, *wheels):
    path = work / name
    run(uv, "venv", path)
    python = path / ("Scripts/python.exe" if sys.platform == "win32" else "bin/python")
    run(uv, "pip", "install", "--python", python, *wheels)
    return python


def project(name, *packages):
    path = work / name
    path.mkdir()
    (path / "package.json").write_text('{"private":true,"type":"module"}\n')
    run(npm, "install", "--ignore-scripts", "--no-audit", "--no-fund", *packages, cwd=path)
    return path


python = venv("venv", monorm_wheel, outside_wheel)
js = project("js", monorm_tarball, outside_tarball)
cli = ["node_modules/monorm/dist/src/cli.js"]
has_postgres = "postgres" in dialects
database = {"ORM_TEST_DATABASE_URL": os.environ["ORM_TEST_DATABASE_URL"]} if has_postgres else {"ORM_TEST_DATABASE_URL": ""}

# The wheel and the tarball hold the built-in extensions, file storage and auth; the smoke scripts check each of them.
run(python, ROOT / "languages/python/packaging/check-wheel.py", monorm_wheel)
run(python, ROOT / "languages/python/packaging/smoke-python.py", cwd=work, env=database)
shutil.copy2(ROOT / "languages/js/packaging/smoke-node.mjs", js / "smoke.mjs")
run(node, "smoke.mjs", cwd=js, env=database)
run(bun, "smoke.mjs", cwd=js, env=database)
# A schema without an extension attribute loads without the extension libraries: the Node check deletes them in a project of its own.
plain = project("js-plain", monorm_tarball)
shutil.copy2(ROOT / "languages/js/packaging/smoke-node.mjs", plain / "smoke.mjs")
for library in (plain / "node_modules/monorm/extensions").rglob("*"):
    if library.suffix in (".dylib", ".so", ".dll"):
        library.unlink()
run(node, "smoke.mjs", "plain", cwd=plain)
run(bun, "smoke.mjs", "plain", cwd=plain)

# An outside package that declares the id of a built-in extension: "installed twice", and the error names the package to remove.
twice = ["installed twice", "uninstall", "acme-copy"]
twice_py = venv("twice-py-venv", monorm_wheel, copy_wheel)
twice_js = project("twice-js", monorm_tarball, copy_npm)
for directory in (work / "twice-py", twice_js):
    directory.mkdir(exist_ok=True)
    (directory / "schema.prisma").write_text(COPY_SCHEMA)
run(twice_py, "-m", "monorm", "--schema", "schema.prisma", "generate", "python", "-o", "models.py", cwd=work / "twice-py", expect_failure=twice)
# Node finds an outside package from the project of the schema, so this schema lives in the project.
run(node, *cli, "--schema", "schema.prisma", "generate", "typescript", "-o", "models.ts", cwd=twice_js, expect_failure=twice)


def generate(directory, source, language, app="python", identities=False):
    """Writes the schema and the models of one language into `directory`."""
    schema = directory / f"schema-{language}.prisma"
    schema.write_text(source.replace("__APP__", APP[app]))
    commands = [["identities"]] if identities else []
    if language == "py":
        for command in commands + [["generate", "python", "-o", directory / "models.py"]]:
            run(python, "-m", "monorm", "--schema", schema, *command, cwd=js)
    else:
        for command in commands + [["generate", "typescript", "-o", directory / "models.ts"]]:
            run(node, *cli, "--schema", schema, *command, cwd=js)
        run(node, TSC, directory / "models.ts", "--outDir", directory, "--module", "ESNext", "--target", "ES2022", "--moduleResolution", "Bundler", "--skipLibCheck", "--noCheck")


def consume(directory, name, url):
    """Runs the Python, Node and Bun side of the consumer `name` against `url`."""
    shutil.copy2(TESTS / f"{name}_consumer.py", directory / "use.py")
    shutil.copy2(TESTS / f"{name}_consumer.mjs", directory / "use.mjs")
    env = {"MONORM_DATABASE_URL": url}
    run(python, "use.py", cwd=directory, env=env)
    run(node, "use.mjs", cwd=directory, env=env)
    run(bun, "use.mjs", cwd=directory, env=env)


def plain_schema(demo, dialect):
    header = f'datasource db {{ provider = "{dialect}" }}\n'
    text = (DEMOS / demo).read_text()
    return header + "".join(line for line in text.splitlines(keepends=True) if not line.startswith("//"))


for dialect in dialects:
    url = "sqlite://:memory:" if dialect == "sqlite" else os.environ["ORM_TEST_DATABASE_URL"]
    base = js / dialect
    for sub in ("", "generic", "proxy", "composition", "auth"):
        (base / sub).mkdir(parents=True, exist_ok=True)
    # The outside extension next to five built-in ones.
    main = SCHEMA.replace("__DIALECT__", dialect)
    generate(base, main, "py")
    generate(base, main, "ts", "node")
    (base / "schema.prisma").write_text(main)
    consume(base, "package", url)
    # Generic: each language freezes the identities of its own schema before generation.
    generic = base / "generic"
    source = plain_schema("generic-relations.prisma", dialect)
    generate(generic, source, "py", identities=True)
    generate(generic, source, "ts", "node", identities=True)
    consume(generic, "generic", url)
    for name, demo in (("proxy", "proxy-models.prisma"), ("composition", "model-composition.prisma")):
        source = plain_schema(demo, dialect)
        generate(base / name, source, "py")
        generate(base / name, source, "ts", "node")
        consume(base / name, name, url)
    # The auth app of the same wheel and tarball, in both languages.
    auth = base / "auth"
    source = (TESTS / "auth_consumer.prisma").read_text().replace("__DIALECT__", dialect)
    generate(auth, source, "py")
    generate(auth, source, "ts", "node")
    consume(auth, "auth", url)
    # File storage: the native tests of its runtime, installed in the consumer.
    env = {"FILE_STORAGE_DATABASE_URL": url}
    shutil.copy2(ROOT / "extensions/file-storage/integration/tests/native_node.mjs", base / "file-storage.test.mjs")
    run(python, ROOT / "extensions/file-storage/integration/tests/native_python.py", cwd=base, env=env)
    run(node, "--test", "file-storage.test.mjs", cwd=base, env=env)
    run(bun, "test", "file-storage.test.mjs", cwd=base, env=env)

# The runtime tests of file storage run once, in the installed packages.
run(python, "-m", "unittest", "discover", "-s", ROOT / "extensions/file-storage/integration/python/tests")
compiled = js / "file-storage"
run(node, TSC, *sorted(glob.glob(str(ROOT / "extensions/file-storage/integration/typescript/test/*.ts"))), "--rootDir", ROOT / "extensions/file-storage", "--outDir", compiled, "--module", "ESNext", "--target", "ES2022", "--moduleResolution", "Bundler", "--skipLibCheck", "--noCheck")
shutil.copytree(ROOT / "extensions/file-storage/fixtures", compiled / "fixtures")
run(node, "--test", *sorted(glob.glob(str(compiled / "integration/typescript/test/*.test.js"))), cwd=js)
shutil.rmtree(work, ignore_errors=True)
