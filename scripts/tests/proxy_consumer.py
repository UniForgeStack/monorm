"""Runs against generated models that use the built-in proxy extension of the installed monorm wheel."""
import asyncio
import json
import os
from pathlib import Path
import monorm
from models import Account, ActiveAccount, Status
from monorm._packages import import_runtime


async def run():
    locked = json.loads(Path("models.py").read_text().split('_SCHEMA = r"""\n', 1)[1].split('\n"""', 1)[0])
    stamp = locked["behavior"]["packages"]["monorm.proxy"]
    assert import_runtime("monorm.proxy").MANIFEST_HASH == stamp["manifest_hash"]
    assert "proxy_models" not in locked["behavior"]
    db = await monorm.connect(os.environ["MONORM_DATABASE_URL"])
    await db.drop_tables()
    await db.create_tables()
    try:
        await Account.objects.using(db).insert_many([
            {"id": 1, "name": "old one", "status": Status.OLD, "note": "n"},
            {"id": 2, "name": "live", "status": Status.ACTIVE, "note": "n"},
        ])
        rows = await ActiveAccount.objects.using(db).all()
        assert sorted(row.name for row in rows) == ["live", "old one"]
        created = await ActiveAccount.objects.using(db).insert(id=3)
        assert created.name == "new" and created.status is Status.ACTIVE
    finally:
        await db.drop_tables()
        await db.close()


asyncio.run(run())
print("proxy consumer python ok")
