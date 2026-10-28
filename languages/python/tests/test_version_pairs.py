"""Run with an artifact that selects the locking extension: bound writes check the loaded versions."""

import importlib
import sys

import pytest

import monorm
from monorm import model as monorm_model
from monorm.__main__ import main as cli
from test_tracking import DATABASES, connect, requires

LOCKED = """
model Doc {
  id      Int    @id
  title   String
  @@locking.version
  @@map("vp_docs")
}
model Plain {
  id      Int    @id
  title   String
  @@map("vp_plain")
}
"""
PAGES = """
model Page {
  id      Int    @id
  title   String
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@locking.version
  @@map("vp_pages")
}
"""
DOCS = LOCKED + PAGES


class Session:
    def __init__(self, db, models):
        self.db, self.models = db, models
        self.events: list[monorm.QueryEvent] = []
        self.remove = db.on_query(self.events.append)

    def writes(self) -> list[str]:
        return [e.sql for e in self.events if e.sql.lstrip().upper().startswith(("UPDATE", "DELETE"))]


@pytest.fixture(params=DATABASES)
async def session(request):
    db, models = await connect(request.param, DOCS)
    try:
        yield Session(db, models)
    finally:
        await db.drop_tables()
        await db.close()


async def seed(session, model="Doc", count=3):
    rows = [{"id": i, "title": f"t{i}"} for i in range(1, count + 1)]
    return await session.models[model].objects.using(session.db).insert_many(rows).returning()


async def titles(session, model="Doc"):
    M = session.models[model]
    return {r.id: (r.title, r.version) for r in await M.objects.using(session.db).order_by(M.id)}


@requires("monorm.locking")
async def test_a_bound_update_with_a_stale_object_raises_and_changes_no_row(session):
    Doc = session.models["Doc"]
    a, b, c = await seed(session)
    other = await Doc.objects.using(session.db).get(Doc.id == 2)
    await other.update(title="newer")
    session.events.clear()
    with pytest.raises(monorm.VersionConflict):
        await Doc.objects.of([a, b, c]).update(title="x")
    assert await titles(session) == {1: ("t1", 0), 2: ("newer", 1), 3: ("t3", 0)}
    assert (a.title, a.version, b.title, b.version) == ("t1", 0, "t2", 0)
    [sql] = session.writes()
    assert "version" in sql


@requires("monorm.locking")
async def test_a_bound_update_of_fresh_objects_writes_one_statement_and_bumps_the_versions(session):
    Doc = session.models["Doc"]
    a, b, c = await seed(session)
    session.events.clear()
    result = await Doc.objects.of([c, a]).update(title="x")
    assert result == [c, a]
    assert [(o.title, o.version) for o in (a, c)] == [("x", 1), ("x", 1)]
    assert await titles(session) == {1: ("x", 1), 2: ("t2", 0), 3: ("x", 1)}
    assert len(session.writes()) == 1
    await Doc.objects.of([a, c]).update(title="y")
    assert [(o.title, o.version) for o in (a, c)] == [("y", 2), ("y", 2)]


@requires("monorm.locking")
async def test_a_bound_delete_with_a_stale_object_raises_and_deletes_no_row(session):
    Doc = session.models["Doc"]
    a, b, c = await seed(session)
    await (await Doc.objects.using(session.db).get(Doc.id == 3)).update(title="newer")
    with pytest.raises(monorm.VersionConflict):
        await Doc.objects.of([a, b, c]).delete()
    assert set(await titles(session)) == {1, 2, 3}
    assert await Doc.objects.of([a, b]).delete() == [a, b]
    assert set(await titles(session)) == {3}


@requires("monorm.locking")
async def test_a_stale_conflict_rolls_back_only_the_bound_write_inside_a_transaction(session):
    Doc = session.models["Doc"]
    a, b, c = await seed(session)
    await (await Doc.objects.using(session.db).get(Doc.id == 1)).update(title="newer")
    async with session.db.transaction():
        await Doc.objects.using(session.db).insert(id=9, title="kept")
        with pytest.raises(monorm.VersionConflict):
            await Doc.objects.of([a, b]).update(title="x")
        assert [r.title for r in await Doc.objects.using(session.db).filter(Doc.id.in_([2, 9])).order_by(Doc.id)] == ["t2", "kept"]
    assert (await titles(session))[9] == ("kept", 0)
    assert (await titles(session))[2] == ("t2", 0)


@requires("monorm.locking")
async def test_an_object_hidden_by_a_filter_is_not_a_conflict_but_a_stale_visible_one_is(session):
    Doc = session.models["Doc"]
    a, b, c = await seed(session)
    assert await Doc.objects.of([a, b]).filter(Doc.title == "t1").update(title="x") == [a]
    await (await Doc.objects.using(session.db).get(Doc.id == 2)).update(title="newer")
    assert await Doc.objects.of([a, b]).filter(Doc.title == "x").update(title="y") == [a]
    with pytest.raises(monorm.VersionConflict):
        await Doc.objects.of([b]).filter(Doc.title == "newer").update(title="z")
    assert await Doc.objects.of([b]).filter(Doc.title == "nothing").update(title="z") == []


@requires("monorm.locking")
async def test_two_objects_of_one_row_with_different_versions_conflict(session):
    Doc = session.models["Doc"]
    [first, *_] = await seed(session)
    second = await Doc.objects.using(session.db).get(Doc.id == 1)
    await second.update(title="newer")
    with pytest.raises(monorm.VersionConflict):
        await Doc.objects.of([first, second]).update(title="x")
    assert (await titles(session))[1] == ("newer", 1)


@requires("monorm.locking")
async def test_a_partial_object_needs_its_version(session):
    Doc = session.models["Doc"]
    await seed(session)
    partial = await Doc.objects.using(session.db).load(Doc.title).get(Doc.id == 1)
    with pytest.raises(monorm.NotLoaded):
        await Doc.objects.of([partial]).update(title="x")
    versioned = await Doc.objects.using(session.db).load(Doc.title, Doc.version).get(Doc.id == 1)
    assert await Doc.objects.of([versioned]).update(title="x") == [versioned]
    assert versioned.version == 1


@requires("monorm.locking")
async def test_a_bound_write_on_a_model_without_locking_runs_the_same_statement(session):
    Plain = session.models["Plain"]
    held = await seed(session, "Plain")
    session.events.clear()
    await Plain.objects.of(held).update(title="x")
    assert len(session.events) == 1
    assert "version" not in session.events[0].sql
    assert {r.title for r in await Plain.objects.using(session.db)} == {"x"}


@requires("monorm.locking")
async def test_an_instance_write_of_a_locking_model_has_one_version_predicate(session):
    Doc = session.models["Doc"]
    a, b, c = await seed(session)
    session.events.clear()
    await a.update(title="x")
    await b.delete()
    for sql in session.writes():
        where = sql.split(" WHERE ", 1)[1]
        assert (where.count('"version" ='), where.count('"id" =')) == (1, 1), sql
    await (await Doc.objects.using(session.db).get(Doc.id == 3)).update(title="newer")
    with pytest.raises(monorm.VersionConflict):
        await c.update(title="y")
    with pytest.raises(monorm.VersionConflict):
        await c.delete()
    assert await titles(session) == {1: ("x", 1), 3: ("newer", 1)}


@requires("monorm.locking")
async def test_a_bound_write_matches_a_key_and_version_pair_for_each_object(session):
    Doc = session.models["Doc"]
    held = await seed(session, count=4)
    session.events.clear()
    await Doc.objects.of(held).update(title="x")
    [sql] = session.writes()
    where = sql.split(" WHERE ", 1)[1]
    assert (where.count(" IN ("), where.count('"version" ='), where.count(" OR ")) == (1, 4, 3), sql


@requires("monorm.locking")
async def test_a_conflict_reads_only_the_key_and_the_version_of_the_rows_that_the_write_left_out(session):
    Doc = session.models["Doc"]
    a, b, c = await seed(session)
    await (await Doc.objects.using(session.db).get(Doc.id == 2)).update(title="newer")
    session.events.clear()
    with pytest.raises(monorm.VersionConflict, match="^Doc 2 was changed since it was loaded$"):
        await Doc.objects.of([a, b, c]).update(title="x")
    [read] = [e.sql for e in session.events if e.sql.lstrip().upper().startswith("SELECT")]
    assert '"version"' in read and '"title"' not in read, read
    assert " IN (" not in read and " OR " not in read, read


@requires("monorm.soft-delete")
@requires("monorm.locking")
async def test_undelete_and_hard_delete_check_the_loaded_versions(session):
    Page = session.models["Page"]
    a, b, c = await seed(session, "Page")
    assert await Page.objects.of([a, b]).delete() == [a, b]
    assert (a.version, b.version) == (1, 1)
    await (await Page.objects.using(session.db).all_with_deleted().get(Page.id == 1)).undelete()
    with pytest.raises(monorm.VersionConflict):
        await Page.objects.of([a, b]).undelete()
    rows = await Page.objects.using(session.db).all_with_deleted().order_by(Page.id)
    assert [(r.id, r.deleted is None, r.version) for r in rows] == [(1, True, 2), (2, False, 1), (3, True, 0)]
    await a.load(reload=True)
    assert await Page.objects.of([a, b]).undelete() == [b]
    assert (b.deleted, b.version) == (None, 2)
    await (await Page.objects.using(session.db).get(Page.id == 1)).update(title="newer")
    with pytest.raises(monorm.VersionConflict):
        await Page.objects.of([a, c]).hard_delete()
    assert {r.id for r in await Page.objects.using(session.db).all_with_deleted()} == {1, 2, 3}
    assert await Page.objects.of([c, b]).hard_delete() == [c, b]
    assert {r.id for r in await Page.objects.using(session.db).all_with_deleted()} == {1}


@pytest.fixture
async def generated(tmp_path, monkeypatch):
    """Load a schema through the `models.py` that `monorm generate` writes, as an application does, on SQLite."""
    opened: list[tuple[monorm.Database, str]] = []

    async def load(schema: str) -> Session:
        package = f"vpgen{len(opened)}"
        root = tmp_path / package
        (root / package).mkdir(parents=True)
        (root / package / "__init__.py").write_text("")
        (root / "schema.prisma").write_text('datasource db { provider = "sqlite" }\n' + schema)
        args = ["--schema", str(root / "schema.prisma"), "generate", "python", "-o", str(root / package / "models.py")]
        assert cli(args) == 0
        registry = monorm.Registry()
        monkeypatch.syspath_prepend(str(root))
        monkeypatch.setattr(monorm_model, "_default_registry", lambda: registry)
        module = importlib.import_module(f"{package}.models")
        db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
        opened.append((db, package))
        await db.create_tables()
        return Session(db, vars(module))

    yield load
    for db, package in opened:
        await db.close()
        for name in [n for n in sys.modules if n == package or n.startswith(f"{package}.")]:
            del sys.modules[name]


@requires("monorm.locking")
async def test_a_generated_module_checks_the_loaded_versions_of_a_bound_write(generated):
    session = await generated(LOCKED)
    Doc, Plain = session.models["Doc"], session.models["Plain"]
    a, b, c = await seed(session)
    await (await Doc.objects.using(session.db).get(Doc.id == 2)).update(title="newer")
    with pytest.raises(monorm.VersionConflict):
        await Doc.objects.of([a, b, c]).update(title="x")
    with pytest.raises(monorm.VersionConflict):
        await Doc.objects.of([a, b, c]).delete()
    assert await titles(session) == {1: ("t1", 0), 2: ("newer", 1), 3: ("t3", 0)}
    held = await seed(session, "Plain")
    session.events.clear()
    await Plain.objects.of(held).update(title="x")
    assert [e.sql for e in session.events if "version" in e.sql] == []


@requires("monorm.soft-delete")
@requires("monorm.locking")
async def test_a_generated_module_checks_the_versions_of_an_undelete(generated):
    session = await generated(PAGES)
    Page = session.models["Page"]
    a, b, _ = await seed(session, "Page")
    assert await Page.objects.of([a, b]).delete() == [a, b]
    await (await Page.objects.using(session.db).all_with_deleted().get(Page.id == 1)).undelete()
    with pytest.raises(monorm.VersionConflict):
        await Page.objects.of([a, b]).undelete()


@requires("monorm.soft-delete")
@requires("monorm.locking")
async def test_a_row_that_another_writer_soft_deleted_is_a_conflict(session):
    Page = session.models["Page"]
    [a, *_] = await seed(session, "Page")
    await (await Page.objects.using(session.db).get(Page.id == 1)).delete()
    with pytest.raises(monorm.VersionConflict):
        await Page.objects.of([a]).update(title="x")
