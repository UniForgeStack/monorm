"""Runs against generated models in a fresh package consumer."""
import asyncio
import json
import os
from pathlib import Path
import orm
from models import Note
from orm._packages import import_runtime


async def run():
    # The generated lock and the installed runtime expose the same manifest identity.
    locked = json.loads(Path("models.py").read_text().split('_SCHEMA = r"""\n', 1)[1].split('\n"""', 1)[0])
    for ident, stamp in locked["behavior"]["packages"].items():
        assert import_runtime(ident).MANIFEST_HASH == stamp["manifest_hash"]
    db = await orm.connect(os.environ["ORM_DATABASE_URL"])
    await db.drop_tables()
    await db.create_tables()
    try:
        row = await Note.objects.using(db).insert(id=1, text="first")
        assert row.tag_label() == "installed"
        assert row.version == 0
        await row.update(text="updated")
        assert row.version == 1 and row.changed is not None
        assert (await Note.objects.using(db).tagged().all())[0].text == "updated"
        await row.delete()
        assert await Note.objects.using(db).all() == []
        assert len(await Note.objects.using(db).all_with_deleted().all()) == 1
    finally:
        await db.drop_tables()
        await db.close()


asyncio.run(run())
print("package consumer python ok")
