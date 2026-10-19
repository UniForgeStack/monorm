"""Standalone enabled/disabled smoke check; does not import generated fixtures."""

import asyncio
import json
import sys

import orm
from orm import _native

expected = sys.argv[1] == "enabled"
available = "reference-loading" in json.loads(_native.native_artifact())["capabilities"]
assert available is expected
assert ("orm._references" in sys.modules) is expected
registry = orm.Registry()
m = orm.loads(
    """datasource db {\n provider = "sqlite"\n}
model Parent {\n id Int @id\n children Child[]\n}
model Child {\n id Int @id\n parent_id Int\n parent Parent @relation(fields: [parent_id], references: [id])\n}""",
    registry=registry,
)
assert hasattr(m["Child"], "load")
assert not hasattr(m["Child"], "load_parent")
assert (type(m["Child"]._meta.relations["parent"]).__name__ == "ReferenceBelongsTo") is expected
if not expected:
    before = registry.ir()
    try:
        orm.define({"models": []}, registry=registry, required_capabilities=("reference-loading",))
    except TypeError as error:
        assert "rebuild" in str(error)
    else:
        raise AssertionError("missing generated capability must reject")
    assert registry.ir() == before


async def verify():
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await m["Parent"].objects.using(db).insert(id=1)
        child = await m["Child"].objects.using(db).insert(id=1, parent_id=1)
        await child.update(parent_id=1)
        await child.load(reload=True)
        assert await child.load(m["Child"].parent) is child
        assert child.parent.id == 1
        if not expected:
            assert not any(name.startswith("_reference_") for name in child.__dict__)
            joined = await m["Child"].objects.using(db).load(m["Child"].parent).get()
            assert joined.parent.id == 1
    finally:
        await db.close()


asyncio.run(verify())
print(f"reference profile {sys.argv[1]} verified")
