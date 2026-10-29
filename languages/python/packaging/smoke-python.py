"""Run in a fresh environment that holds only the `monorm` wheel: the built-in extensions, file storage and auth work.

Set ORM_TEST_DATABASE_URL to also run the PostgreSQL query.
"""

import asyncio
import ctypes
import importlib.metadata as metadata
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

import monorm
from monorm import _native

BUILT_IN = {
    "monorm.timestamps": "monorm_timestamps",
    "monorm.soft-delete": "monorm_soft_delete",
    "monorm.locking": "monorm_locking",
    "monorm.inherit": "monorm_inherit",
    "monorm.query-defaults": "monorm_query_defaults",
    "monorm.generic": "monorm_generic",
    "monorm.proxy": "monorm_proxy",
    "monorm.composition": "monorm_composition",
    "monorm.file-storage": "monorm_file_storage",
}

SCHEMA = """datasource db { provider = "PROVIDER" }
model Base {
  id Int @id
  @@inherit.abstract
  @@timestamps.updated_at("changed", mode: "application")
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@locking.version("version")
}
model Note {
  text String
  @@inherit.extends(Base)
  @@query.filter("positive", "id > 0")
}
enum Status {
  ACTIVE @map("active")
  OLD    @map("old")
  @@storage(text)
}
model Account {
  id     Int    @id
  name   String?
  status Status @default(OLD)
}
model ActiveAccount {
  name   String         @client_default("new")
  status Status(ACTIVE) @client_default(ACTIVE)
  @@proxy.of(Account)
}
model Person {
  id   Int    @id @default(autoincrement())
  name String
}
model Employee {
  salary Int
  @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
}
model Report {
  id   Int   @id
  file Json? @storage.file(storage: "reports")
}
"""
# `monorm identities` lists an abstract model that `generate` skips, so the generic models get their own schema.
GENERIC = """datasource db { provider = "sqlite" }
model Photo {
  id   Int     @id
  tags Label[] @generic.reverse
}
model Label {
  id     Int      @id
  target Generic? @generic.relation(targets: ["Photo"])
}
"""
PLAIN = """datasource db { provider = "sqlite" }
model Probe {
  id   Int    @id
  name String
}
"""


def loaded_libraries():
    """The names of the shared libraries of this process; `None` where the platform has no way to list them."""
    if sys.platform == "linux":
        return {line.split()[-1] for line in Path("/proc/self/maps").read_text().splitlines() if "/" in line.split()[-1]}
    if sys.platform == "darwin":
        dyld = ctypes.CDLL(None)
        dyld._dyld_get_image_name.restype = ctypes.c_char_p
        dyld._dyld_get_image_name.argtypes = [ctypes.c_uint32]
        return {dyld._dyld_get_image_name(i).decode() for i in range(dyld._dyld_image_count())}
    return None


def loaded_extension_libraries():
    libraries = loaded_libraries()
    return None if libraries is None else sorted(Path(name).name for name in libraries if "monorm_" in Path(name).name)


def generate(work, name, source, identities=False):
    schema = work / f"{name}.prisma"
    schema.write_text(source)
    commands = ([["identities"]] if identities else []) + [["generate", "python", "-o", str(work / f"{name}.py")]]
    for command in commands:
        subprocess.run([sys.executable, "-m", "monorm", "--schema", str(schema), *command], check=True, cwd=work)
    spec = importlib.util.spec_from_file_location(f"smoke_{name}", work / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


async def run_main(models, url, work):
    from opendal import AsyncOperator
    from monorm_file_storage import Registry, Upload

    db = await monorm.connect(url)
    try:
        await db.drop_tables()
        await db.create_tables()
        note = await models.Note.objects.using(db).insert(id=1, text="a")
        assert note.version == 0 and note.deleted is None
        await note.update(text="b")
        assert note.version == 1
        assert await models.Note.objects.using(db).count() == 1
        created = await models.ActiveAccount.objects.using(db).insert(id=1)
        assert (created.name, created.status) == ("new", models.Status.ACTIVE)
        assert (await models.Account.objects.using(db).count()) == 1
        employee = await models.Employee.objects.using(db).insert(name="Alice", salary=20)
        assert (employee.name, employee.salary) == ("Alice", 20)
        files = work / "files"
        files.mkdir()
        models.configure_file_storage(Registry({"reports": AsyncOperator("fs", root=str(files))}))
        await models.Report.objects.using(db).insert(id=1, file=Upload(b"quarterly numbers", filename="q3.txt"))
        report = await models.Report.objects.using(db).require().get(models.Report.id == 1)
        assert b"".join([chunk async for chunk in report.file_open()]) == b"quarterly numbers"
    finally:
        await db.drop_tables()
        await db.close()


async def run_generic(models):
    db = await monorm.connect("sqlite://:memory:")
    try:
        await db.create_tables()
        photo = await models.Photo.objects.using(db).insert(id=1)
        await models.Label.objects.using(db).insert(id=1, target_type=models.ContentType.Photo, target_id=photo.id)
        assert await models.Label.objects.using(db).count() == 1
    finally:
        await db.close()


def check_distribution():
    assert json.loads(_native.profile_metadata())["profile"] == "tooling"
    found = {entry.name: entry for entry in metadata.entry_points(group="monorm.extensions")}
    assert set(BUILT_IN) <= set(found), sorted(set(BUILT_IN) - set(found))
    for ident, module in BUILT_IN.items():
        assert found[ident].dist.name == "monorm" and found[ident].value == module, ident
    apps = {entry.name: entry for entry in metadata.entry_points(group="monorm.apps")}
    assert apps["monorm-auth"].dist.name == "monorm"
    import monorm_auth

    assert (Path(monorm_auth.__file__).parent / "schema.prisma").is_file()
    assert monorm_auth.check_password("secret", monorm_auth.make_password("secret"))
    old = ("monorm-auth", "monorm-file-storage", "monorm-native-tooling", *(ident.replace(".", "-") for ident in BUILT_IN))
    for distribution in old:
        try:
            metadata.version(distribution)
        except metadata.PackageNotFoundError:
            continue
        raise AssertionError(f"{distribution} is installed next to monorm")


def check_plain():
    """A schema with no extension attribute loads no extension library."""
    before = loaded_extension_libraries()
    monorm.loads(PLAIN, registry=monorm.Registry())
    after = loaded_extension_libraries()
    if before is None:
        print("skipped: this platform cannot list loaded libraries")
    else:
        assert before == after == [], (before, after)


def check_libraries_load_on_use():
    """Compiling a schema with an extension attribute opens the library of that extension only."""
    monorm.loads(SCHEMA.replace("PROVIDER", "sqlite"), registry=monorm.Registry())
    loaded = loaded_extension_libraries()
    if loaded is not None:
        assert any("timestamps" in name for name in loaded) and any("inherit" in name for name in loaded), loaded
        assert not any("generic" in name for name in loaded), loaded


# Generated modules share the default registry, so each schema runs in its own interpreter.
if len(sys.argv) == 1:
    check_distribution()
    check_plain()
    check_libraries_load_on_use()
    jobs = [["main", "sqlite"], ["generic"]]
    if os.environ.get("ORM_TEST_DATABASE_URL"):
        jobs.append(["main", "postgres"])
    for job in jobs:
        subprocess.run([sys.executable, __file__, *job], check=True)
    cli = subprocess.run([sys.executable, "-m", "monorm", "--help"], capture_output=True, text=True)
    assert cli.returncode == 0 and "generate" in cli.stdout, cli.stdout + cli.stderr
    print("monorm wheel smoke ok")
else:
    with tempfile.TemporaryDirectory() as directory:
        work = Path(directory)
        if sys.argv[1] == "main":
            postgres = sys.argv[2] == "postgres"
            provider, url = ("postgresql", os.environ["ORM_TEST_DATABASE_URL"]) if postgres else ("sqlite", "sqlite://:memory:")
            models = generate(work, "main", SCHEMA.replace("PROVIDER", provider))
            asyncio.run(run_main(models, url, work))
        else:
            asyncio.run(run_generic(generate(work, "generic", GENERIC, True)))
