"""The `proto` extension: `@proto.message("pkg.Msg")` on a `Bytes` field, end to end on PostgreSQL and SQLite."""

import importlib.util
import os
import sys
import textwrap
from pathlib import Path
from types import SimpleNamespace

import pytest

import monorm
from monorm import QueryError, SchemaError
from monorm.__main__ import main as cli
from monorm_proto import ProtoError, proto_codec

GENERATED = Path(__file__).parent / "proto_gen"
sys.path.insert(0, str(GENERATED))  # the generated message classes import as `acme.note_pb2`

from acme import note_pb2  # noqa: E402
from acme.billing import invoice_pb2  # noqa: E402

URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SCHEMA = """model ProtoEvent {
  id      Int    @id
  payload Bytes? @proto.message("acme.Note")
  inner   Bytes? @proto.message("acme.Outer.Inner")
  bill    Bytes? @proto.message("acme.billing.Invoice")
  raw     Bytes?
  @@map("proto_event")
}"""
SETTINGS = '[proto.python]\n"acme" = "acme.note_pb2"\n"acme.billing" = "acme.billing.invoice_pb2"\n'


def build(root: Path, schema: str, settings: str, provider: str, name: str = "models") -> SimpleNamespace:
    """Run `monorm generate` in `root` and import the generated models into a fresh registry."""
    datasource = 'datasource db {\n  provider = "sqlite"\n}\n' if provider == "sqlite" else ""
    (root / "schema.prisma").write_text(datasource + schema)
    (root / "monorm.toml").write_text(settings)
    cwd = Path.cwd()
    os.chdir(root)
    try:
        assert cli(["generate"]) == 0
    finally:
        os.chdir(cwd)
    registry = monorm.Registry()
    previous = monorm.model.registry
    monorm.model.registry = registry
    try:
        spec = importlib.util.spec_from_file_location(f"proto_models_{name}", root / "models.py")
        assert spec and spec.loader
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
    finally:
        monorm.model.registry = previous
    return SimpleNamespace(module=module, registry=registry, root=root)


@pytest.fixture(params=["sqlite", "postgres"])
async def event(request, tmp_path):
    built = build(tmp_path, SCHEMA, SETTINGS, request.param)
    try:
        db = await monorm.connect("sqlite://:memory:" if request.param == "sqlite" else URL, registry=built.registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    Event = built.module.ProtoEvent
    yield Event, Event.objects.using(db), db
    await db.drop_tables()
    await db.close()


def note(title="first", priority=3, tags=("a", "b"), label=None):
    value = note_pb2.Note(title=title, priority=priority, tags=list(tags))
    if label is not None:
        value.inner.label = label
    return value


async def stored(db, column="payload", id=1):
    """The bytes in the column, read without the codec."""
    return (await db.fetch(f'SELECT "{column}" AS v FROM proto_event WHERE id = {int(id)}'))[0]["v"]


async def test_proto_round_trip(event):
    Event, qs, db = event
    row = await qs.insert(id=1, payload=note(label="deep"), inner=note_pb2.Outer.Inner(label="in"), bill=invoice_pb2.Invoice(number="n-1", cents=250))
    assert isinstance(row.payload, note_pb2.Note)
    loaded = await qs.get(Event.id == 1)
    assert loaded.payload == note(label="deep")
    assert loaded.inner == note_pb2.Outer.Inner(label="in")
    assert loaded.bill == invoice_pb2.Invoice(number="n-1", cents=250)
    assert loaded.raw is None
    assert bytes(await stored(db)) == note(label="deep").SerializeToString()


async def test_a_null_and_an_empty_message_differ(event):
    Event, qs, db = event
    await qs.insert_many([{"id": 1, "payload": None}, {"id": 2, "payload": note_pb2.Note()}])
    rows = {r.id: r.payload for r in await qs.order_by(Event.id)}
    assert rows[1] is None and rows[2] == note_pb2.Note()
    assert bytes(await stored(db, id=2)) == b""


async def test_bulk_writes_encode_every_row(event):
    Event, qs, _ = event
    await qs.insert_many([{"id": i, "payload": note(title=f"t{i}", priority=i)} for i in range(1, 6)])
    assert [r.payload.title for r in await qs.order_by(Event.id)] == ["t1", "t2", "t3", "t4", "t5"]
    await qs.filter(Event.id > 3).update(payload=note(title="late"))
    await qs.filter(Event.id == 1).update(payload=None)
    got = {r.id: r.payload for r in await qs.order_by(Event.id)}
    assert got[1] is None and got[2].title == "t2" and got[4].title == "late" and got[5].title == "late"
    await qs.update_many([{"id": 2, "payload": note(title="two")}, {"id": 3, "payload": note(title="three")}])
    assert [r.payload.title for r in await qs.filter(Event.id.in_([2, 3])).order_by(Event.id)] == ["two", "three"]


async def test_reading_a_row_decodes_the_field_once_when_the_row_loads(event):
    Event, qs, _ = event
    await qs.insert(id=1, payload=note(title="x"))
    row = await qs.get(Event.id == 1)
    row.payload.title = "kept"
    assert row.payload.title == "kept"
    assert (await qs.get(Event.id == 1)).payload.title == "x"
    await row.load(reload=True)
    assert row.payload.title == "x"


async def test_a_message_of_the_wrong_class_is_refused_before_the_statement(event):
    Event, qs, _ = event
    for bad in (note_pb2.Other(name="x"), invoice_pb2.Invoice(number="n"), b"\x0a\x01a", "text", 7):
        with pytest.raises(ProtoError, match="acme.Note"):
            await qs.insert(id=1, payload=bad)
    assert await qs.count() == 0
    with pytest.raises(ProtoError):
        await qs.insert_many([{"id": 1, "payload": note()}, {"id": 2, "payload": note_pb2.Other()}])
    assert await qs.count() == 0


async def test_a_nested_message_class_is_found_by_its_dotted_name(event):
    Event, qs, _ = event
    with pytest.raises(ProtoError, match="Outer.Inner"):
        await qs.insert(id=1, inner=note_pb2.Outer())
    await qs.insert(id=1, inner=note_pb2.Outer.Inner(label="ok"))
    assert (await qs.get(Event.id == 1)).inner.label == "ok"


async def test_is_null_tests_work_and_every_other_use_is_refused(event):
    Event, qs, _ = event
    await qs.insert_many([{"id": 1, "payload": note()}, {"id": 2, "payload": None}])
    assert await qs.filter(Event.payload.is_null()).select(Event.id).scalars() == [2]
    assert await qs.filter(Event.payload.is_not_null()).select(Event.id).scalars() == [1]
    use = "IS NULL or IS NOT NULL"
    refused = [
        (lambda: qs.filter(Event.payload == note()), use),
        (lambda: qs.filter(Event.payload != note()), "nullable"),
        (lambda: qs.filter(Event.payload.in_([note()])), use),
        (lambda: qs.order_by(Event.payload), "not defined on a Bytes value"),
        (lambda: qs.filter((Event.payload == note()).and_not_null()), use),
        (lambda: qs.select(Event.payload, monorm.func.length(Event.payload)), use),
    ]
    for build_query, match in refused:
        with pytest.raises(QueryError, match=match):
            await build_query()
    with pytest.raises(QueryError, match="not defined on a Bytes value"):
        await qs.select(monorm.func.max(Event.payload)).scalars()
    # Selecting the whole field is allowed and gives the stored bytes, with no decoding.
    assert [None if v is None else bytes(v) for v in await qs.order_by(Event.id).select(Event.payload).scalars()] == [note().SerializeToString(), None]


async def test_unknown_fields_of_a_later_release_survive_a_round_trip(event):
    Event, qs, db = event
    later = note_pb2.NoteV2(title="n", priority=1, tags=["x"], added_later="kept").SerializeToString()
    await qs.insert(id=1, raw=later)
    await db.execute("UPDATE proto_event SET payload = raw")
    row = await qs.get(Event.id == 1)
    assert row.payload.title == "n"
    edited = note_pb2.Note.FromString(row.payload.SerializeToString())
    edited.title = "edited"
    await qs.filter(Event.id == 1).update(payload=edited)
    again = note_pb2.NoteV2.FromString(bytes(await stored(db)))
    assert (again.title, again.added_later) == ("edited", "kept")


async def test_a_message_without_a_package_map_entry_is_a_schema_error(tmp_path, capfd):
    (tmp_path / "schema.prisma").write_text(SCHEMA)
    (tmp_path / "monorm.toml").write_text('[proto.python]\n"other" = "other_pb2"\n')
    cwd = Path.cwd()
    os.chdir(tmp_path)
    try:
        assert cli(["generate"]) != 0
    finally:
        os.chdir(cwd)
    error = capfd.readouterr().err
    assert "acme.Note" in error and "proto.python" in error
    assert not (tmp_path / "models.py").exists()


FAKE = '''
class Thing:
    """No protobuf API: the user registers a codec."""

    def __init__(self, text=""):
        self.text = text
'''
FAKE_SCHEMA = """model FakeEvent {
  id   Int    @id
  body Bytes? @proto.message("fake.Thing")
  @@map("proto_fake_event")
}"""


@pytest.fixture
async def fake(tmp_path, monkeypatch):
    (tmp_path / "fakecls.py").write_text(FAKE)
    monkeypatch.syspath_prepend(str(tmp_path))
    built = build(tmp_path, FAKE_SCHEMA, '[proto.python]\n"fake" = "fakecls"\n', "sqlite", "fake")
    db = await monorm.connect("sqlite://:memory:", registry=built.registry, default=False)
    await db.create_tables()
    import fakecls

    yield built.module.FakeEvent, fakecls.Thing, db
    await db.close()
    sys.modules.pop("fakecls", None)


async def test_a_class_without_a_codec_names_proto_codec(fake):
    FakeEvent, Thing, db = fake
    with pytest.raises(ProtoError, match="proto_codec"):
        await FakeEvent.objects.using(db).insert(id=1, body=Thing("a"))
    assert await FakeEvent.objects.using(db).count() == 0


async def test_a_registered_codec_serves_a_class_without_a_protobuf_api(fake):
    FakeEvent, Thing, db = fake
    proto_codec(Thing, lambda value: value.text.encode(), lambda data: Thing(data.decode()))
    qs = FakeEvent.objects.using(db)
    await qs.insert_many([{"id": 1, "body": Thing("hello")}, {"id": 2, "body": None}])
    assert [getattr(r.body, "text", None) for r in await qs.order_by(FakeEvent.id)] == ["hello", None]
    with pytest.raises(ProtoError, match="fake.Thing"):
        await qs.insert(id=3, body="hello")


def refused(schema: str, match: str):
    with pytest.raises(SchemaError, match=match):
        monorm.loads(f'datasource db {{\n  provider = "sqlite"\n}}\n{schema}', registry=monorm.Registry())


@pytest.mark.parametrize(
    "fields, extra, match",
    [
        ('body Bytes? @proto.message("a.B") @index', "", "@index"),
        ('body Bytes? @proto.message("a.B") @unique', "", "@unique"),
        ('body Bytes? @proto.message("a.B") @check("body IS NOT NULL")', "", "@check"),
        ('body Bytes? @proto.message("a.B") @default(raw("x\'00\'"))', "", "default"),
        ('body Bytes[] @proto.message("a.B")', "", r"Bytes\[\]"),
        ('body String? @proto.message("a.B")', "", "Bytes field is required"),
        ('body Bytes? @proto.message("Note")', "", "full message name"),
        ('body Bytes? @proto.message("a.B")', "@@index([body])", "@index"),
    ],
)
def test_a_proto_field_that_the_database_could_not_use_is_refused_by_the_schema(fields, extra, match):
    refused(f"model M {{\n id Int @id\n {fields}\n {extra}\n}}", match)


def test_a_proto_field_cannot_be_a_key_or_sit_in_a_nested_type():
    refused('model M {\n body Bytes @id @proto.message("a.B")\n}', "key")
    refused('type T {\n  body Bytes? @proto.message("a.B")\n}\nmodel M {\n id Int @id\n t T\n}', "unknown attribute")


def test_the_generated_stub_types_the_field_as_the_message_class(tmp_path):
    built = build(tmp_path, SCHEMA, SETTINGS, "sqlite", "stub")
    stub = (built.root / "models.pyi").read_text()
    assert "import acme.note_pb2 as _pm0" in stub and "import acme.billing.invoice_pb2 as _pm1" in stub
    assert "payload: f.Bytes[_pm0.Note | None]" in stub
    assert "inner: f.Bytes[_pm0.Outer.Inner | None]" in stub
    assert "bill: f.Bytes[_pm1.Invoice | None]" in stub
    assert "raw: f.Bytes[bytes | None]" in stub or "raw: f.Bytes[" in stub
    source = (built.root / "models.py").read_text()
    assert "import acme.note_pb2 as _pm0" in source and '"symbol": "Outer.Inner"' in source
    assert textwrap.dedent(source)


async def test_the_admin_lists_reads_and_edits_a_row_but_refuses_bytes_for_a_message_field(tmp_path):
    import base64

    from monorm import admin

    built = build(tmp_path, 'import "app:monorm-admin" (prefix: "admin_")\n' + SCHEMA, SETTINGS, "sqlite", "admin")
    db = await monorm.connect("sqlite://:memory:", registry=built.registry, default=False)
    await db.create_tables()
    model = built.module.ProtoEvent
    await model.objects.using(db).insert(id=1, payload=note_pb2.Note(title="hi"))
    site = admin.Admin(built.registry)
    site.register(model, {})
    client = site.test_client(db, user={"key": 1, "is_superuser": True})
    path = "/admin/api/resources/ProtoEvent"
    stored = base64.b64encode(note_pb2.Note(title="hi").SerializeToString()).decode()
    assert (await client.get(f"{path}/1")).status == 200
    listing = await client.get(path)
    assert listing.status == 200 and stored in listing.text
    edit = await client.patch(f"{path}/1", json={"set": {"raw": "eA=="}, "loaded": {"raw": None}})
    assert edit.status == 200, edit.text
    # The form sends base64 bytes: the adapter refuses them, and the admin answers 422 with the reason, never 500.
    refused = await client.patch(f"{path}/1", json={"set": {"payload": stored}, "loaded": {"payload": stored}})
    assert refused.status == 422 and "takes acme.Note, not bytes" in refused.text
    other = base64.b64encode(note_pb2.Note(title="other").SerializeToString()).decode()
    conflict = await client.patch(f"{path}/1", json={"set": {"payload": stored}, "loaded": {"payload": other}})
    assert conflict.status == 409
    created = await client.post(path, json={"set": {"id": 2, "payload": stored}})
    assert created.status == 422 and await model.objects.using(db).filter(model.id == 2).first() is None
    row = await model.objects.using(db).filter(model.id == 1).first()
    assert row is not None and row.raw == b"x" and row.payload.title == "hi"
    await db.close()
