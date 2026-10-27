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
    packages = locked["behavior"]["packages"]
    assert set(packages) == {"acme.outside", "orm.inherit", "orm.locking", "orm.soft-delete", "orm.timestamps"}
    for ident, stamp in packages.items():
        assert import_runtime(ident).MANIFEST_HASH == stamp["manifest_hash"]
    db = await orm.connect(os.environ["ORM_DATABASE_URL"])
    await db.drop_tables()
    await db.create_tables()
    try:
        notes = Note.objects.using(db)

        async def fails(coro, *parts):
            try:
                await coro
            except orm.QueryError as error:
                for part in parts:
                    assert part in str(error), f"{part!r} is missing from {error}"
                return
            raise AssertionError(f"expected an error with {parts}")

        with orm.scope(shop=1, user="ann"):
            # acme.outside.trim (point 1) runs before acme.outside.nonempty (point 2).
            row = await notes.insert(id=1, text="  first ", start=1, end=2, shop_id=1)
            assert row.text == "first"
            assert row.tag_label() == "installed"
            assert row.version == 0 and row.updated_by is None
            await fails(notes.insert(id=9, text="   ", start=1, end=2, shop_id=1), "Note.text: must not be empty")
            # acme.outside.ordered (point 3) reads two fields.
            await fails(notes.insert(id=9, text="x", start=3, end=1, shop_id=1), "start 3 must be before end 1")
            assert await notes.filter(Note.id == 9).count() == 0
            # acme.outside.actor (point 4) takes the user of the request scope on update.
            await row.update(text="updated")
            assert row.version == 1 and row.changed is not None
            assert (await notes.get(Note.id == 1)).updated_by == "ann"
            # A value that the caller sets for that field goes through the hook too.
            await row.update(updated_by="eve")
            assert (await notes.get(Note.id == 1)).updated_by == "ann"
            assert (await notes.tagged().all())[0].text == "updated"
            # Another shop's row is outside the default filter `shop_id == scope.shop`.
            await notes.insert(id=2, text="other", start=1, end=2, shop_id=2)
            assert [n.id for n in await notes.all()] == [1]
        with orm.scope(shop=2, user="bob"):
            assert [n.id for n in await notes.all()] == [2]
            assert await notes.filter(Note.text == "other").update(text="bulk") == 1
            assert (await notes.get(Note.id == 2)).updated_by == "bob"
        with orm.scope(shop=1):
            await fails(row.update(text="again"), "no user in the request scope")
        with orm.scope(shop=1, user="ann"):
            await row.delete()
            assert await notes.all() == []
            assert len(await notes.all_with_deleted().all()) == 1
    finally:
        await db.drop_tables()
        await db.close()


asyncio.run(run())
print("package consumer python ok")
