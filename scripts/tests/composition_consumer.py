"""Runs against generated models that use the installed orm-composition package."""
import asyncio
import json
import os
from pathlib import Path
import orm
from models import Person, Employee, Customer
from orm._packages import import_runtime


async def run():
    locked = json.loads(Path("models.py").read_text().split('_SCHEMA = r"""\n', 1)[1].split('\n"""', 1)[0])
    stamp = locked["behavior"]["packages"]["orm.composition"]
    assert import_runtime("orm.composition").MANIFEST_HASH == stamp["manifest_hash"]
    db = await orm.connect(os.environ["ORM_DATABASE_URL"])
    await db.drop_tables()
    await db.create_tables()
    try:
        alice = await Employee.objects.using(db).insert(name="Alice", salary=20)
        assert (alice.name, alice.salary) == ("Alice", 20)
        customer = await Customer.objects.using(db).attach(alice.id, {"points": 5})
        assert (customer.id, customer.name, customer.points) == (alice.id, "Alice", 5)
        counts = [await m.objects.using(db).count() for m in (Person, Employee, Customer)]
        assert counts == [1, 1, 1]
    finally:
        await db.drop_tables()
        await db.close()


asyncio.run(run())
print("composition consumer python ok")
