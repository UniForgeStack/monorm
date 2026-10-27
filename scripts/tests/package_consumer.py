"""Runs against generated models in a fresh package consumer."""
import asyncio
import json
import os
from pathlib import Path
import orm
from acme_outside import Wrapped
from models import Note, configure_outside
from orm._packages import import_runtime


async def run():
    # The generated lock and the installed runtime expose the same manifest identity.
    locked = json.loads(Path("models.py").read_text().split('_SCHEMA = r"""\n', 1)[1].split('\n"""', 1)[0])
    packages = locked["behavior"]["packages"]
    assert set(packages) == {"acme.outside", "orm.inherit", "orm.locking", "orm.query-defaults", "orm.soft-delete", "orm.timestamps"}
    for ident, stamp in packages.items():
        assert import_runtime(ident).MANIFEST_HASH == stamp["manifest_hash"]
    # The lock stores an expression default as a record: `region` from a hook that generate evaluated, `owner` still a hook.
    defaults = {f["name"]: f.get("client_default") for m in locked["models"] if m["name"] == "Note" for f in m["fields"]}
    assert defaults["shop_id"] == {"expr": {"t": "scope", "name": "shop"}}
    assert defaults["region"] == {"expr": {"t": "text", "value": "eu"}}
    assert defaults["owner"] == {"hook": {"export": "acme.outside.owner"}}
    db = await orm.connect(os.environ["ORM_DATABASE_URL"])
    await db.drop_tables()
    await db.create_tables()
    try:
        notes = Note.objects.using(db)

        async def fails(coro, *parts, errors=(orm.QueryError,)):
            try:
                await (coro() if callable(coro) else coro)
            except errors as error:
                for part in parts:
                    assert part in str(error), f"{part!r} is missing from {error}"
                return
            raise AssertionError(f"expected an error with {parts}")

        with orm.scope(shop=1, user="ann"):
            # `@@query.filter("positive", "id > 0")` hides this row; the guard filter and the package filter both apply.
            await notes.insert(id=-1, text="hidden", start=1, end=2, shop_id=1, secret="")
            assert await notes.count() == 0
            assert await notes.without_defaults().count() == 1
            # acme.outside.trim (point 1) runs before acme.outside.nonempty (point 2).
            row = await notes.insert(id=1, text="  first ", start=1, end=2, shop_id=1, secret="")
            assert row.text == "first"
            assert row.tag_label() == "installed"
            assert row.version == 0
            # Hook 7 gives `region` and `owner` an expression default; `updated_by` and `shop_id` default to the request scope.
            created = await notes.get(Note.id == 1)
            assert (created.region, created.owner, created.updated_by, created.shop_id) == ("eu", "by:ann", "ann", 1)
            await fails(notes.insert(id=9, text="   ", start=1, end=2, shop_id=1, secret=""), "Note.text: must not be empty")
            # acme.outside.ordered (point 3) reads two fields.
            await fails(notes.insert(id=9, text="x", start=3, end=1, shop_id=1, secret=""), "start 3 must be before end 1")
            assert await notes.filter(Note.id == 9).count() == 0
            # acme.outside.actor (point 4) takes the user of the request scope on update.
            await row.update(text="updated")
            assert row.version == 1 and row.changed is not None
            assert (await notes.tagged().all())[0].text == "updated"
            # Expression assignments run on update: `slug` is `lower(text)` and reads the row as it was before the update.
            # `stamp` comes from a hook that reads no context and `editor` from one that reads the user of the request scope.
            changed = await notes.get(Note.id == 1)
            assert (changed.updated_by, changed.slug, changed.stamp, changed.editor) == ("ann", "first", "guarded", "by:ann")
            # `touched_by` is an expression assignment that reads the request scope: `scope.user`.
            assert changed.touched_by == "ann"
            # A value that the caller sets for that field goes through the hook too.
            await row.update(updated_by="eve")
            assert (await notes.get(Note.id == 1)).updated_by == "ann"
            assert (await notes.tagged().all())[0].text == "updated"
            # `@outside.wrapped("w:")` installs the second consumer of the field adapter runtime: label = runtime prefix + option prefix.
            assert (await notes.get(Note.id == 1)).note is None
            configure_outside("rt-")
            await notes.filter(Note.id == 1).update(note={"a": 1})
            loaded = await notes.get(Note.id == 1)
            assert isinstance(loaded.note, Wrapped) and loaded.note.label == "rt-w:" and loaded.note.payload == {"a": 1}
            assert loaded.note_unwrap() == {"a": 1}
            await notes.filter(Note.id == 1).update(note=Wrapped("ignored", {"b": 2}))
            row = await notes.get(Note.id == 1)  # the bulk updates changed the version
            assert row.note_unwrap() == {"b": 2}
            # acme.outside.initials (point 5) computes `initials` from `first` and `last` on read.
            await notes.insert(id=3, text="named", start=1, end=2, first="ada", last="lovelace", secret="")
            assert (await notes.get(Note.id == 3)).initials == "AL"
            assert [n.initials for n in await notes.filter(Note.id.in_([1, 3])).order_by(Note.id).all()] == ["", "AL"]
            # The bindings drop a computed field from the update input, so the call fails before the engine runs.
            await fails(lambda: notes.filter(Note.id == 3).update(initials="ZZ"), "initials", errors=(orm.QueryError, TypeError))
            await fails(lambda: notes.filter(Note.initials == "AL").count(), "computed fields")
            await notes.filter(Note.id == 3).delete()
            # Hook 1 seals `secret` before the write; hook 5 reads it back as `secret_plain`.
            await notes.insert(id=4, text="vault", start=1, end=2, secret="open")
            vault = await notes.get(Note.id == 4)
            assert (vault.secret, vault.secret_plain) == ("enc:nepo", "open")
            await notes.filter(Note.id == 4).delete()
            # Another shop's row is outside the default filter `shop_id == scope.shop`.
            await notes.insert(id=2, text="other", start=1, end=2, shop_id=2, secret="")
            assert [n.id for n in await notes.all()] == [1]
        with orm.scope(shop=2, user="bob"):
            assert [n.id for n in await notes.all()] == [2]
            assert await notes.filter(Note.text == "other").update(text="bulk") == 1
            assert (await notes.get(Note.id == 2)).updated_by == "bob"
            assert (await notes.get(Note.id == 2)).editor == "by:bob"
            assert (await notes.get(Note.id == 2)).touched_by == "bob"
        with orm.scope(shop=1):
            await fails(notes.insert(id=8, text="x", start=1, end=2, secret=""), "user")
            assert await notes.filter(Note.id == 8).count() == 0
            await fails(row.update(text="again"), "no user in the request scope")
        with orm.scope(shop=1, user="ann"):
            await row.delete()
            assert await notes.all() == []
            # The soft-deleted rows 3 and 4 stay visible next to row 1.
            assert sorted(n.id for n in await notes.all_with_deleted().all()) == [1, 3, 4]
    finally:
        await db.drop_tables()
        await db.close()


asyncio.run(run())
print("package consumer python ok")
