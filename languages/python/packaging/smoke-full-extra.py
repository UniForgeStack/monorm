"""Run in a fresh environment that holds `orm[full]`: every member is installed and its attribute works."""

import asyncio
import importlib.metadata as metadata
import importlib.util
import json
import subprocess
import sys
import tempfile
from pathlib import Path

import orm
from orm import _native

assert json.loads(_native.profile_metadata())["profile"] == "tooling"
for requirement in metadata.requires("orm") or []:
    name, _, rest = requirement.partition(";")
    if 'extra == "full"' not in rest:
        continue
    dist, _, version = name.strip().partition("==")
    assert metadata.version(dist) == version, (dist, version)

SCHEMA = """datasource db { provider = "sqlite" }
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
"""
# `orm identities` lists an abstract model that `generate` skips, so the generic models get their own schema.
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


async def run_main(models):
    db = await orm.connect("sqlite://:memory:")
    try:
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
    finally:
        await db.close()


async def run_generic(models):
    db = await orm.connect("sqlite://:memory:")
    try:
        await db.create_tables()
        photo = await models.Photo.objects.using(db).insert(id=1)
        await models.Label.objects.using(db).insert(id=1, target_type=models.ContentType.Photo, target_id=photo.id)
        assert await models.Label.objects.using(db).count() == 1
    finally:
        await db.close()


def generate(work, name, source, identities):
    schema = work / f"{name}.prisma"
    schema.write_text(source)
    commands = ([["identities"]] if identities else []) + [["generate", "python", "-o", str(work / f"{name}.py")]]
    for command in commands:
        subprocess.run([sys.executable, "-m", "orm", "--schema", str(schema), *command], check=True, cwd=work)
    spec = importlib.util.spec_from_file_location(f"full_extra_{name}", work / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# Generated modules share the default registry, so each schema runs in its own interpreter.
if len(sys.argv) == 1:
    for name in ("main", "generic"):
        subprocess.run([sys.executable, __file__, name], check=True)
    print("orm[full] smoke ok")
else:
    with tempfile.TemporaryDirectory() as directory:
        work = Path(directory)
        if sys.argv[1] == "main":
            asyncio.run(run_main(generate(work, "main", SCHEMA, False)))
        else:
            asyncio.run(run_generic(generate(work, "generic", GENERIC, True)))
