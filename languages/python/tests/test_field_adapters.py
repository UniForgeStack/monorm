"""The async field adapter pipeline (`orm.adapters`): validate, ``await prepare``, SQL, decode."""

import pytest
import orm
from orm import _capabilities
from orm.adapters import AdapterWriteError, FieldAdapterError, Pending, register_field_adapter

SCHEMA = """datasource db { provider = "sqlite" }
model Doc {
  id    Int     @id
  title String
  body  Json?
  extra Json?
  views Int     @default(0)
}
model Locked {
  id    Int     @id
  body  Json?
  @@protected_write
}"""


class Counting:
    """No I/O: ``prepare`` counts its calls and returns a known stored value."""

    def __init__(self, tag, fail_on=None):
        self.tag, self.fail_on, self.calls = tag, fail_on, []

    def validate(self, value):
        if value is None or isinstance(value, dict):
            return value
        if not isinstance(value, str):
            raise TypeError(f"{self.tag}: unsupported value {value!r}")
        return Pending(value, {"placeholder": True})

    async def prepare(self, value):
        self.calls.append(value)
        if value == self.fail_on:
            raise RuntimeError("prepare failed")
        return {"tag": self.tag, "value": value}

    def decode(self, stored):
        return None if stored is None else (self.tag, stored)


async def setup(body=None, extra=None):
    registry = orm.Registry()
    models = orm.loads(SCHEMA, registry=registry)
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    body = body or Counting("body")
    register_field_adapter(models["Doc"], "body", body)
    if extra is not None:
        register_field_adapter(models["Doc"], "extra", extra)
    register_field_adapter(models["Locked"], "body", Counting("locked"))
    return db, models, body


async def test_insert_prepares_once_and_decodes_the_loaded_value():
    db, models, body = await setup()
    try:
        row = await models["Doc"].objects.using(db).insert(id=1, title="a", body="file")
        assert body.calls == ["file"]
        assert row.body == ("body", {"tag": "body", "value": "file"})
        loaded = await models["Doc"].objects.using(db).get(models["Doc"].id == 1)
        assert loaded.body == ("body", {"tag": "body", "value": "file"})
    finally:
        await db.close()


async def test_a_value_that_needs_no_prepare_writes_without_a_call():
    db, models, body = await setup()
    try:
        objects = models["Doc"].objects.using(db)
        await objects.insert(id=1, title="a", body={"stored": 1})
        await objects.insert(id=2, title="b", body=None)
        assert body.calls == []
        assert (await objects.get(models["Doc"].id == 1)).body == ("body", {"stored": 1})
        assert (await objects.get(models["Doc"].id == 2)).body is None
    finally:
        await db.close()


async def test_validation_runs_before_any_prepare():
    db, models, body = await setup()
    try:
        objects = models["Doc"].objects.using(db)
        with pytest.raises(TypeError, match="unsupported value 3"):
            objects.insert(id=1, title="a", body=3)
        with pytest.raises(ValueError, match="title is required"):
            objects.insert(id=1, body="file")
        assert body.calls == []
    finally:
        await db.close()


async def test_write_protection_is_checked_before_prepare():
    db, models, _ = await setup()
    try:
        pending = models["Locked"].objects.using(db).insert(id=1, body="file")
        with pytest.raises(orm.WriteProtected):
            await pending
        assert pending.operation.stored == {}
        with orm.allow_writes(models["Locked"]):
            await models["Locked"].objects.using(db).insert(id=1, body="file")
        assert (await models["Locked"].objects.using(db).get(models["Locked"].id == 1)).body is not None
    finally:
        await db.close()


async def test_on_conflict_rejects_a_pending_value_without_a_prepare_call():
    db, models, body = await setup()
    try:
        insert = models["Doc"].objects.using(db).insert(id=1, title="a", body="file")
        with pytest.raises(FieldAdapterError, match="conflict"):
            insert.on_conflict(models["Doc"].id, update=True)
        assert body.calls == []
    finally:
        await db.close()


async def test_an_expression_for_an_adapted_field_is_rejected_before_prepare():
    db, models, body = await setup()
    try:
        with pytest.raises(FieldAdapterError, match="no expression"):
            models["Doc"].objects.using(db).filter(models["Doc"].id == 1).update(body=models["Doc"].views + 1)
        assert body.calls == []
    finally:
        await db.close()


async def test_unique_update_prepares_once_and_a_filtered_update_is_rejected():
    db, models, body = await setup()
    try:
        Doc = models["Doc"]
        objects = Doc.objects.using(db)
        await objects.insert_many([{"id": 1, "title": "a"}, {"id": 2, "title": "a"}])
        assert await objects.filter(Doc.id == 1).update(body="new") == 1
        assert body.calls == ["new"]
        with pytest.raises(FieldAdapterError, match="unique row"):
            objects.filter(Doc.title == "a").update(body="shared")
        assert body.calls == ["new"]
        rows = await objects.filter(Doc.id == 2).update(body="again").returning()
        assert [r.body for r in rows] == [("body", {"tag": "body", "value": "again"})]
    finally:
        await db.close()


async def test_instance_update_runs_the_pipeline():
    db, models, body = await setup()
    try:
        row = await models["Doc"].objects.using(db).insert(id=1, title="a")
        await row.update(body="later")
        assert body.calls == ["later"]
        assert row.body == ("body", {"tag": "body", "value": "later"})
    finally:
        await db.close()


async def test_two_adapted_fields_run_in_one_pipeline_in_value_order():
    extra = Counting("extra")
    db, models, body = await setup(extra=extra)
    try:
        row = await models["Doc"].objects.using(db).insert(id=1, title="a", extra="two", body="one")
        assert (body.calls, extra.calls) == (["one"], ["two"])
        assert row.body == ("body", {"tag": "body", "value": "one"})
        assert row.extra == ("extra", {"tag": "extra", "value": "two"})
    finally:
        await db.close()


async def test_a_failed_second_prepare_lists_the_first_stored_value():
    extra = Counting("extra", fail_on="two")
    db, models, body = await setup(extra=extra)
    try:
        with pytest.raises(AdapterWriteError) as caught:
            await models["Doc"].objects.using(db).insert(id=1, title="a", body="one", extra="two")
        assert caught.value.stored == {"body": {"tag": "body", "value": "one"}}
        assert isinstance(caught.value.__cause__, RuntimeError)
        assert await models["Doc"].objects.using(db).count() == 0
    finally:
        await db.close()


async def test_a_failed_sql_statement_retries_without_a_new_prepare():
    db, models, body = await setup()
    try:
        Doc = models["Doc"]
        objects = Doc.objects.using(db)
        await objects.insert(id=1, title="taken")
        with pytest.raises(AdapterWriteError) as caught:
            await objects.insert(id=1, title="again", body="file")
        assert body.calls == ["file"]
        assert caught.value.stored == {"body": {"tag": "body", "value": "file"}}
        await objects.filter(Doc.id == 1).delete()
        row = await caught.value.operation.execute()
        assert body.calls == ["file"]
        assert (row.id, row.body) == (1, ("body", {"tag": "body", "value": "file"}))
    finally:
        await db.close()


async def test_a_method_that_collides_with_a_model_member_is_rejected():
    db, models, _ = await setup()
    try:
        with pytest.raises(FieldAdapterError, match="collides"):
            register_field_adapter(models["Doc"], "extra", Counting("x"), {"title": lambda self: 1})
        register_field_adapter(models["Doc"], "extra", Counting("x"), {"extra_open": lambda self: "ok"})
        assert (await models["Doc"].objects.using(db).insert(id=1, title="a")).extra_open() == "ok"
    finally:
        await db.close()


def test_a_missing_language_capability_fails_the_load_naming_the_extension():
    artifact = {"requires": {"file-storage": ["async-field-adapter.v1"], "other": ["stream.v9"]}}
    with pytest.raises(ImportError, match=r"extension other requires the capability stream\.v9"):
        _capabilities.check_requires(artifact)
    _capabilities.check_requires({"requires": {"file-storage": ["async-field-adapter.v1"]}})
    _capabilities.check_requires({})
    with pytest.raises(ImportError, match="file-storage requires the capability async-field-adapter.v1"):
        _capabilities.check_requires(artifact, frozenset())


async def test_batch_writes_store_checked_values_and_reject_a_pending_value():
    db, models, body = await setup()
    try:
        Doc = models["Doc"]
        objects = Doc.objects.using(db)
        await objects.insert_many(
            [{"id": 1, "title": "a", "body": {"stored": 1}}, {"id": 2, "title": "b", "body": None}]
        )
        assert (await objects.get(Doc.id == 1)).body == ("body", {"stored": 1})
        assert (await objects.get(Doc.id == 2)).body is None
        await objects.update_many([{"id": 1, "body": None}, {"id": 2, "body": {"stored": 2}}])
        assert (await objects.get(Doc.id == 1)).body is None
        assert (await objects.get(Doc.id == 2)).body == ("body", {"stored": 2})
        with pytest.raises(FieldAdapterError, match="insert_many.*needs prepare"):
            objects.insert_many(
                [{"id": 3, "title": "c", "body": {"stored": 3}}, {"id": 4, "title": "d", "body": "upload"}]
            )
        with pytest.raises(FieldAdapterError, match="update_many.*needs prepare"):
            objects.update_many([{"id": 1, "body": "upload"}])
        assert await objects.count() == 2
        assert body.calls == []
    finally:
        await db.close()


async def test_a_filtered_update_and_an_upsert_take_null_or_a_stored_value():
    db, models, body = await setup()
    try:
        Doc = models["Doc"]
        objects = Doc.objects.using(db)
        await objects.insert(id=1, title="a", body={"stored": 1})
        await objects.insert(id=2, title="b", body={"stored": 2})
        assert await objects.filter(Doc.id > 0).update(body=None) == 2
        assert (await objects.get(Doc.id == 1)).body is None
        await objects.filter(Doc.id > 0).update(body={"stored": 9})
        assert (await objects.get(Doc.id == 2)).body == ("body", {"stored": 9})
        await objects.insert(id=1, title="again", body=None).on_conflict(Doc.id, update=True)
        assert (await objects.get(Doc.id == 1)).title == "again"
        row, created = await objects.get_or_insert({"title": "c", "body": {"stored": 5}}, id=3)
        assert created and row.body == ("body", {"stored": 5})
        row, created = await objects.get_or_insert({"title": "d", "body": None}, id=4)
        assert created and row.body is None
        assert body.calls == []
    finally:
        await db.close()


class Exits(Counting):
    async def prepare(self, value):
        raise SystemExit(3)


async def test_prepare_system_exit_is_not_wrapped():
    db, models, _ = await setup(body=Exits("body"))
    try:
        pending = models["Doc"].objects.using(db).insert(id=1, title="a", body="file")
        with pytest.raises(SystemExit):
            await pending
        with pytest.raises(FieldAdapterError, match="failed"):
            await pending.operation.prepare()
    finally:
        await db.close()


async def test_a_method_named_like_a_reserved_member_is_rejected():
    registry = orm.Registry()
    models = orm.loads(SCHEMA, registry=registry)
    for name in ("refresh", "_orm_internal"):
        with pytest.raises(FieldAdapterError, match="collides"):
            register_field_adapter(models["Doc"], "body", Counting("body"), {name: lambda self: None})


async def test_inspect_on_an_adapted_write_shows_the_placeholder_and_calls_no_prepare():
    db, models, body = await setup()
    try:
        objects = models["Doc"].objects.using(db)
        insert = objects.insert(id=1, title="a", body="file")
        assert len(insert.inspect().statements) == 1
        update = objects.filter(models["Doc"].id == 1).update(body="file")
        assert len(update.inspect().statements) == 1
        assert len(update.returning().inspect().statements) == 1
        assert body.calls == []
        insert._used = update._used = True
    finally:
        await db.close()
