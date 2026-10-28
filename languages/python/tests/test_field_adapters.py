"""The async field adapter pipeline (`orm.adapters`): validate, ``await prepare``, SQL, decode."""

import asyncio

import pytest
import orm
from orm import _capabilities
from orm.adapters import AdapterWriteCancelled, AdapterWriteError, FieldAdapterError, Pending, register_field_adapter

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


async def test_unique_update_prepares_once():
    db, models, body = await setup()
    try:
        Doc = models["Doc"]
        objects = Doc.objects.using(db)
        await objects.insert_many([{"id": 1, "title": "a"}, {"id": 2, "title": "a"}])
        assert await objects.filter(Doc.id == 1).update(body="new") == 1
        assert body.calls == ["new"]
        rows = await objects.filter(Doc.id == 2).update(body="again").returning()
        assert [r.body for r in rows] == [("body", {"tag": "body", "value": "again"})]
    finally:
        await db.close()


async def test_a_filtered_update_prepares_once_and_writes_the_same_value_to_every_row():
    db, models, body = await setup()
    try:
        Doc = models["Doc"]
        objects = Doc.objects.using(db)
        await objects.insert_many([{"id": 1, "title": "a"}, {"id": 2, "title": "a"}, {"id": 3, "title": "b"}])
        assert await objects.filter(Doc.title == "a").update(body="shared") == 2
        assert body.calls == ["shared"]
        shared = ("body", {"tag": "body", "value": "shared"})
        assert [r.body for r in await objects.order_by(Doc.id).all()] == [shared, shared, None]
        rows = await objects.filter(Doc.title == "b").update(body="other").returning()
        assert [r.id for r in rows] == [3] and body.calls == ["shared", "other"]
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


async def test_batch_writes_store_checked_values_without_a_prepare_call():
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
        assert body.calls == []
    finally:
        await db.close()


def stored(tag, value):
    return (tag, {"tag": tag, "value": value})


async def test_insert_many_prepares_each_pending_value_and_keeps_the_others():
    db, models, body = await setup()
    try:
        Doc = models["Doc"]
        objects = Doc.objects.using(db)
        rows = [
            {"id": 1, "title": "a", "body": "one"},
            {"id": 2, "title": "b", "body": {"stored": 2}},
            {"id": 3, "title": "c", "body": "three"},
            {"id": 4, "title": "d"},
        ]
        assert await objects.insert_many(rows) == 4
        assert sorted(body.calls) == ["one", "three"]
        got = {r.id: r.body for r in await objects.all()}
        assert got == {1: stored("body", "one"), 2: ("body", {"stored": 2}), 3: stored("body", "three"), 4: None}
        news = await objects.insert_many([{"id": 5, "title": "e", "body": "five"}]).returning()
        assert [(r.id, r.body) for r in news] == [(5, stored("body", "five"))]
        assert rows[0]["body"] == "one"
    finally:
        await db.close()


async def test_update_many_prepares_each_pending_value():
    db, models, body = await setup()
    try:
        Doc = models["Doc"]
        objects = Doc.objects.using(db)
        await objects.insert_many([{"id": 1, "title": "a"}, {"id": 2, "title": "b"}, {"id": 3, "title": "c"}])
        assert await objects.update_many([{"id": 1, "body": "one"}, {"id": 2, "body": {"stored": 2}}]) == 2
        assert body.calls == ["one"]
        done = await objects.update_many([{"id": 2, "body": "two"}, {"id": 3, "body": "three"}]).returning()
        assert sorted(body.calls) == ["one", "three", "two"]
        assert {r.id: r.body for r in done} == {2: stored("body", "two"), 3: stored("body", "three")}
        assert (await objects.get(Doc.id == 1)).body == stored("body", "one")
    finally:
        await db.close()


class Parallel(Counting):
    def __init__(self, tag):
        super().__init__(tag)
        self.running = self.peak = 0

    async def prepare(self, value):
        self.running += 1
        self.peak = max(self.peak, self.running)
        await asyncio.sleep(0.01)
        self.running -= 1
        return await super().prepare(value)


async def test_batch_prepare_calls_run_in_parallel_up_to_the_limit():
    body = Parallel("body")
    db, models, _ = await setup(body=body)
    try:
        objects = models["Doc"].objects.using(db)
        await objects.insert_many([{"id": n, "title": "t", "body": f"f{n}"} for n in range(1, 41)])
        assert len(body.calls) == 40
        assert body.peak == 16
        body.peak = 0
        await objects.update_many([{"id": n, "body": f"g{n}"} for n in range(1, 41)])
        assert body.peak == 16
    finally:
        await db.close()


async def test_a_failed_batch_prepare_lists_the_finished_values_and_writes_nothing():
    body = Counting("body", fail_on="bad")
    db, models, _ = await setup(body=body)
    try:
        objects = models["Doc"].objects.using(db)
        rows = [{"id": 1, "title": "a", "body": "good"}, {"id": 2, "title": "b", "body": "bad"}]
        with pytest.raises(AdapterWriteError) as caught:
            await objects.insert_many(rows)
        assert caught.value.stored == {(0, "body"): {"tag": "body", "value": "good"}}
        assert isinstance(caught.value.__cause__, RuntimeError)
        assert await objects.count() == 0
        await objects.insert_many([{"id": 1, "title": "a"}, {"id": 2, "title": "b"}])
        with pytest.raises(AdapterWriteError):
            await objects.update_many([{"id": 1, "body": "bad"}, {"id": 2, "body": "good"}])
        assert [r.body for r in await objects.order_by(models["Doc"].id).all()] == [None, None]
    finally:
        await db.close()


async def test_a_failed_batch_insert_retries_without_a_new_prepare():
    db, models, body = await setup()
    try:
        Doc = models["Doc"]
        objects = Doc.objects.using(db)
        await objects.insert(id=2, title="taken")
        with pytest.raises(AdapterWriteError) as caught:
            await objects.insert_many([{"id": 1, "title": "a", "body": "one"}, {"id": 2, "title": "b", "body": "two"}])
        assert sorted(body.calls) == ["one", "two"]
        assert set(caught.value.stored) == {(0, "body"), (1, "body")}
        await objects.filter(Doc.id == 2).delete()
        assert await caught.value.operation.execute() == 2
        assert sorted(body.calls) == ["one", "two"]
        assert (await objects.get(Doc.id == 2)).body == stored("body", "two")
    finally:
        await db.close()


class OneShot:
    """A source that one ``prepare`` call reads empty after the first."""

    def __init__(self, data):
        self.data = data

    def read(self):
        data, self.data = self.data, b""
        return data


class Streams(Counting):
    def validate(self, value):
        if isinstance(value, OneShot):
            return Pending(value, {"placeholder": True})
        return super().validate(value)

    async def prepare(self, value):
        if isinstance(value, OneShot):
            self.calls.append(value)
            return {"tag": self.tag, "size": len(value.read())}
        return await super().prepare(value)


async def test_one_object_in_several_rows_prepares_once():
    body = Streams("body")
    db, models, _ = await setup(body=body)
    try:
        objects = models["Doc"].objects.using(db)
        source = OneShot(b"hello")
        await objects.insert_many([{"id": n, "title": "t", "body": source} for n in (1, 2, 3)])
        assert len(body.calls) == 1
        assert [r.body for r in await objects.all()] == [("body", {"tag": "body", "size": 5})] * 3
        await objects.update_many([{"id": n, "body": OneShot(b"hi")} for n in (1, 2)])
        assert len(body.calls) == 3
    finally:
        await db.close()


async def test_batch_validation_and_protection_run_before_any_prepare():
    db, models, body = await setup()
    try:
        Doc, Locked = models["Doc"], models["Locked"]
        objects = Doc.objects.using(db)
        with pytest.raises(TypeError, match="unsupported value 3"):
            objects.insert_many([{"id": 1, "title": "a", "body": "ok"}, {"id": 2, "title": "b", "body": 3}])
        with pytest.raises(TypeError, match="unsupported value 3"):
            objects.update_many([{"id": 1, "body": "ok"}, {"id": 2, "body": 3}])
        with pytest.raises(TypeError, match="not an instance of"):
            await objects.update_many([{"id": 1, "title": "ok", "body": "aa"}, {"id": 2, "title": 5, "body": "bb"}])
        assert body.calls == []
        with pytest.raises(ValueError, match="title is required"):
            objects.insert_many([{"id": 1, "body": "ok"}])
        insert = Locked.objects.using(db).insert_many([{"id": 1, "body": "file"}])
        with pytest.raises(orm.WriteProtected):
            await insert
        assert insert.operation.stored == {}
        update = Locked.objects.using(db).update_many([{"id": 1, "body": "file"}])
        with pytest.raises(orm.WriteProtected):
            await update
        assert update.operation.stored == {}
        assert body.calls == []
        with orm.allow_writes(Locked):
            assert await Locked.objects.using(db).insert_many([{"id": 1, "body": "file"}]) == 1
    finally:
        await db.close()


async def test_a_pending_batch_value_is_rejected_in_a_conflict_write_and_in_copy():
    db, models, body = await setup()
    try:
        Doc = models["Doc"]
        insert = Doc.objects.using(db).insert_many([{"id": 1, "title": "a", "body": "file"}])
        with pytest.raises(FieldAdapterError, match="conflict"):
            insert.on_conflict(Doc.id, update=True)
        with pytest.raises(FieldAdapterError, match="copy"):
            Doc.objects.using(db).insert_many([{"id": 1, "title": "a", "body": "file"}], copy=True)
        assert body.calls == []
    finally:
        await db.close()


async def test_inspect_on_an_adapted_batch_shows_the_placeholder_and_calls_no_prepare():
    db, models, body = await setup()
    try:
        objects = models["Doc"].objects.using(db)
        insert = objects.insert_many([{"id": 1, "title": "a", "body": "file"}])
        assert len(insert.inspect().statements) == 1
        update = objects.update_many([{"id": 1, "body": "file"}])
        assert len(update.inspect().statements) == 1
        assert body.calls == []
    finally:
        await db.close()


async def test_a_failed_batch_update_retries_without_a_new_prepare():
    db, models, body = await setup()
    try:
        Doc = models["Doc"]
        objects = Doc.objects.using(db)
        await objects.insert(id=1, title="a")
        await db.execute("DROP TABLE Doc")
        with pytest.raises(AdapterWriteError) as caught:
            await objects.update_many([{"id": 1, "body": "one"}, {"id": 2, "body": "two"}])
        assert sorted(body.calls) == ["one", "two"]
        assert set(caught.value.stored) == {(0, "body"), (1, "body")}
        await db.create_tables()
        await objects.insert(id=1, title="a")
        assert await caught.value.operation.execute() == 1
        assert sorted(body.calls) == ["one", "two"]
        assert (await objects.get(Doc.id == 1)).body == stored("body", "one")
    finally:
        await db.close()


class Cancelling(Counting):
    async def prepare(self, value):
        if value == "cancel":
            raise asyncio.CancelledError
        await asyncio.sleep(0.05)
        return await super().prepare(value)


async def test_a_prepare_call_that_raises_cancelled_leaves_no_call_running():
    body = Cancelling("body")
    db, models, _ = await setup(body=body)
    try:
        objects = models["Doc"].objects.using(db)
        with pytest.raises(AdapterWriteCancelled) as caught:
            await objects.insert_many([{"id": 1, "title": "a", "body": "cancel"}, {"id": 2, "title": "b", "body": "slow"}])
        seen = dict(caught.value.stored)
        await asyncio.sleep(0.1)
        assert dict(caught.value.stored) == seen
        assert (1, "body") not in seen
    finally:
        await db.close()


M2M_SCHEMA = """datasource db { provider = "sqlite" }
model Post {
  id   Int   @id
  tags Tag[] @relation(through: PostTag, through_fields: [post, tag])
}
model Tag {
  id    Int    @id
  name  String
  icon  Json?
  posts Post[] @relation(through: PostTag, through_fields: [tag, post])
}
model PostTag {
  id      Int  @id @default(autoincrement())
  post_id Int
  tag_id  Int
  post    Post @relation(fields: [post_id], references: [id])
  tag     Tag  @relation(fields: [tag_id], references: [id])
}"""


class InTransaction(Counting):
    """Records whether ``prepare`` ran inside a transaction of ``db``."""

    def __init__(self, tag, db):
        super().__init__(tag)
        self.db, self.inside = db, []

    async def prepare(self, value):
        self.inside.append(self.db._tx() is not None)
        return await super().prepare(value)


async def test_a_related_insert_on_a_many_to_many_prepares_before_the_transaction():
    registry = orm.Registry()
    models = orm.loads(M2M_SCHEMA, registry=registry)
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        Post, Tag = models["Post"], models["Tag"]
        icon = InTransaction("icon", db)
        register_field_adapter(Tag, "icon", icon)
        post = await Post.objects.using(db).insert(id=1)
        tag = await post.tags.insert(id=1, name="a", icon="file")
        assert icon.inside == [False]
        assert tag.icon == stored("icon", "file")
        assert [t.id for t in await post.tags.all()] == [1]
        with pytest.raises(AdapterWriteError):
            await post.tags.insert(id=1, name="again", icon="file")
        assert icon.calls == ["file", "file"]
        assert await Post.objects.using(db).count() == 1
        assert [t.id for t in await post.tags.all()] == [1]
    finally:
        await db.close()


class StreamsInTransaction(Streams):
    def __init__(self, tag, db):
        super().__init__(tag)
        self.db, self.inside = db, []

    async def prepare(self, value):
        self.inside.append(self.db._tx() is not None)
        return await super().prepare(value)


async def test_through_defaults_prepare_once_before_the_transaction():
    registry = orm.Registry()
    models = orm.loads(M2M_SCHEMA.replace("  tag_id  Int\n", "  tag_id  Int\n  file    Json?\n"), registry=registry)
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        Post, Tag, PostTag = models["Post"], models["Tag"], models["PostTag"]
        file = StreamsInTransaction("file", db)
        register_field_adapter(PostTag, "file", file)
        await Tag.objects.using(db).insert_many([{"id": n, "name": str(n)} for n in (1, 2, 3, 4)])
        post = await Post.objects.using(db).insert(id=1)
        await post.tags.set([1, 2, 3], through_defaults={"file": OneShot(b"hello")})
        assert len(file.calls) == 1 and file.inside == [False]
        links = await PostTag.objects.using(db).all()
        assert [link.file for link in links] == [("file", {"tag": "file", "size": 5})] * 3
        await post.tags.add(4, through_defaults={"file": OneShot(b"hi")})
        assert len(file.calls) == 2 and file.inside == [False, False]
        assert (await PostTag.objects.using(db).get(PostTag.tag_id == 4)).file == ("file", {"tag": "file", "size": 2})
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
