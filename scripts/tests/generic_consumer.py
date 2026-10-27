"""Runs against generated models that use the installed orm-generic package."""
import asyncio
import json
import os
from pathlib import Path
import orm
from models import ContentType, Label, Photo, Video
from orm._packages import import_runtime


async def run():
    locked = json.loads(Path("models.py").read_text().split('_SCHEMA = r"""\n', 1)[1].split('\n"""', 1)[0])
    stamp = locked["behavior"]["packages"]["orm.generic"]
    assert import_runtime("orm.generic").MANIFEST_HASH == stamp["manifest_hash"]
    assert locked["behavior"]["schema_contract_minor"] >= 1
    assert set(locked["behavior"]["extension_data"]["orm.generic"]) == {"relations", "reverse"}
    db = await orm.connect(os.environ["ORM_DATABASE_URL"])
    await db.drop_tables()
    await db.create_tables()
    try:
        photo = await Photo.objects.using(db).insert(id=1, url="cat.png")
        await Video.objects.using(db).insert(id=1, title="cats")
        await Label.objects.using(db).insert_many([
            {"id": 1, "name": "cute", "target_type": ContentType.Photo, "target_id": photo.id},
            {"id": 2, "name": "long", "target_type": ContentType.Video, "target_id": 1},
        ])
        on_photos = await Label.objects.using(db).filter(Label.target_type == ContentType.Photo)
        assert [x.name for x in on_photos] == ["cute"]
        try:
            await Label.objects.using(db).insert(id=3, name="half", target_type=ContentType.Photo)
        except Exception:
            pass
        else:
            raise AssertionError("the pair CHECK constraint accepted a type without a key")
    finally:
        await db.drop_tables()
        await db.close()


asyncio.run(run())
print("generic consumer python ok")
