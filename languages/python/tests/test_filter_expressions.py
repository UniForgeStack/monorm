"""A default filter is a schema expression: methods, a to-one path and a typed scope on both databases; a string filter still works."""

import os
import subprocess
import sys

import pytest

import monorm
from artifact_caps import has_extension

SOURCE = """
datasource db {
  provider = "%(provider)s"
}

model FxOwner {
  id     Int        @id
  region String
  items  FxNative[]
  @@map("fx_owner")
}

model FxNative {
  id       Int     @id
  owner_id Int
  title    String
  rank     Int
  owner    FxOwner @relation(fields: [owner_id], references: [id])
  @@map("fx_native")
  @@query.filter("window", rank.between(2, 9) & title.startswith("t"))
  @@query.filter("north", owner.region == "north")
}

model FxScoped {
  id    Int    @id
  title String
  shop  Int
  @@map("fx_scoped")
  @@query.filter("tenant", shop == scope.shop)
}

model FxLegacy {
  id    Int    @id
  title String
  rank  Int
  shop  Int
  @@map("fx_legacy")
  @@query.filter("legacy", "rank > 1 and shop == 1")
}
"""

ROWS = [
    {"id": 1, "title": "tea", "rank": 1, "shop": 1},
    {"id": 2, "title": "toast", "rank": 3, "shop": 1},
    {"id": 3, "title": "tart", "rank": 4, "shop": 2},
    {"id": 4, "title": "bun", "rank": 5, "shop": 1},
    {"id": 5, "title": "tin", "rank": 12, "shop": 1},
]
OWNERS = {1: 1, 2: 1, 3: 2, 4: 1, 5: 1}

pytestmark = pytest.mark.skipif(not has_extension("monorm.query-defaults"), reason="requires the monorm.query-defaults package")


@pytest.fixture(params=["sqlite", "postgresql"])
async def world(request):
    provider = request.param
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    registry = monorm.Registry()
    models = monorm.loads(SOURCE % {"provider": provider}, registry=registry)
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    await models["FxOwner"].objects.using(db).insert_many([{"id": 1, "region": "north"}, {"id": 2, "region": "south"}])
    await models["FxNative"].objects.using(db).insert_many([{**{k: v for k, v in r.items() if k != "shop"}, "owner_id": OWNERS[r["id"]]} for r in ROWS])
    await models["FxScoped"].objects.using(db).insert_many([{k: v for k, v in r.items() if k != "rank"} for r in ROWS])
    await models["FxLegacy"].objects.using(db).insert_many(ROWS)
    yield db, models
    await db.drop_tables()
    await db.close()


def ids(rows):
    return [r.id for r in rows]


async def test_method_and_to_one_path_filters_run_on_both_databases(world):
    db, models = world
    Native = models["FxNative"]
    assert ids(await Native.objects.using(db).order_by(Native.id)) == [2]
    assert ids(await Native.objects.using(db).without_defaults("north").order_by(Native.id)) == [2, 3]
    assert ids(await Native.objects.using(db).without_defaults("window").order_by(Native.id)) == [1, 2, 4, 5]
    assert await Native.objects.using(db).count() == 1


async def test_a_scope_value_in_a_filter_takes_the_type_of_its_field(world):
    db, models = world
    Scoped = models["FxScoped"]
    with monorm.scope(shop=1):
        assert ids(await Scoped.objects.using(db).order_by(Scoped.id)) == [1, 2, 4, 5]
    with monorm.scope(shop=2):
        assert ids(await Scoped.objects.using(db).order_by(Scoped.id)) == [3]
    with pytest.raises(monorm.QueryError, match="scope.shop"):
        await Scoped.objects.using(db).count()


async def test_a_filter_written_as_a_string_still_filters(world):
    db, models = world
    Legacy = models["FxLegacy"]
    assert ids(await Legacy.objects.using(db).order_by(Legacy.id)) == [2, 4, 5]


WARNED = """
datasource db {
  provider = "sqlite"
}

model FxWarned {
  id   Int @id
  rank Int
  @@query.filter("big", "rank > 1")
}
"""


def test_a_filter_written_as_a_string_warns_with_its_position_and_the_native_form(tmp_path):
    if not has_extension("monorm.query-defaults"):
        pytest.skip("requires a selected query-defaults native artifact")
    schema = tmp_path / "schema.prisma"
    schema.write_text(WARNED)
    with pytest.warns(monorm.SchemaWarning, match=r"schema\.prisma:9:\d+: warning: this filter is a string; write the expression: rank > 1") as caught:
        monorm.load(schema, registry=monorm.Registry())
    assert len(caught) == 1


SHAPES = """
datasource db {
  provider = "sqlite"
}

model FxBase {
  id     Int     @id
  active Boolean
  @@inherit.abstract
  @@query.filter("on", active == true)
}

model FxOne {
  @@inherit.extends(["FxBase"])
  @@map("fx_one")
}

model FxTwo {
  note String
  @@inherit.extends(["FxBase"])
  @@map("fx_two")
}

model FxStamped {
  id   Int    @id
  name String
  @@timestamps.updated_at("edited", mode: "application")
  @@query.filter("touched", edited != null)
  @@map("fx_stamped")
}

model FxMember {
  id     Int     @id
  active Boolean
  legacy Int?
  @@map("fx_member")
}

model FxProxy {
  @@proxy.of(FxMember)
  @@proxy.fields(exclude: ["legacy"])
  @@query.filter("on", active == true)
}

model FxPerson {
  id     Int     @id
  active Boolean
  @@map("fx_person")
}

model FxStaff {
  salary Int
  @@composition.model(parent: "FxPerson", parentRef: "person", childRef: "staff")
  @@query.filter("on", active == true & salary > 1)
  @@map("fx_staff")
}
"""

needs_shapes = pytest.mark.skipif(
    not has_extension("monorm.query-defaults", "monorm.inherit", "monorm.timestamps", "monorm.proxy", "monorm.composition"),
    reason="requires the query-defaults, inherit, timestamps, proxy and composition packages",
)


@needs_shapes
async def test_a_filter_resolves_on_the_final_shape_of_every_kind_of_model():
    registry = monorm.Registry()
    models = monorm.loads(SHAPES, registry=registry)
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    try:
        # An abstract model's filter runs in each concrete model; a field that a pass adds can be filtered.
        await models["FxOne"].objects.using(db).insert_many([{"id": 1, "active": True}, {"id": 2, "active": False}])
        await models["FxTwo"].objects.using(db).insert_many([{"id": 1, "active": False, "note": "n"}, {"id": 2, "active": True, "note": "n"}, {"id": 3, "active": True, "note": "m"}])
        assert ids(await models["FxOne"].objects.using(db).order_by(models["FxOne"].id)) == [1]
        assert ids(await models["FxTwo"].objects.using(db).order_by(models["FxTwo"].id)) == [2, 3]
        stamped = models["FxStamped"].objects.using(db)
        await stamped.insert_many([{"id": 1, "name": "a"}, {"id": 2, "name": "b"}])
        assert ids(await stamped.order_by(models["FxStamped"].id)) == [1, 2]
        # A proxy keeps the filter of a kept field; a composed model filters a field of its parent.
        await models["FxMember"].objects.using(db).insert_many([{"id": 1, "active": True, "legacy": 0}, {"id": 2, "active": False, "legacy": 0}])
        assert ids(await models["FxProxy"].objects.using(db).order_by(models["FxProxy"].id)) == [1]
        await models["FxStaff"].objects.using(db).insert_many([{"id": 1, "active": True, "salary": 5}, {"id": 2, "active": False, "salary": 5}, {"id": 3, "active": True, "salary": 0}])
        assert ids(await models["FxStaff"].objects.using(db).order_by(models["FxStaff"].id)) == [1]
    finally:
        await db.close()


@needs_shapes
def test_a_proxy_filter_that_names_an_omitted_field_fails_with_its_position():
    source = SHAPES.replace('@@query.filter("on", active == true)\n}\n\nmodel FxPerson', '@@query.filter("on", legacy == 0)\n}\n\nmodel FxPerson')
    assert source != SHAPES
    with pytest.raises(monorm.SchemaError, match=r":4\d:\d+: @@query.filter: unknown field `legacy` on model `FxProxy`"):
        monorm.loads(source, registry=monorm.Registry())


PARENT = """
datasource db {
  provider = "sqlite"
}

model FxParent {
  id     Int     @id
  active Boolean
  rank   Int
  @@query.filter("live", active == true)
}

model FxChild {
  id     Int     @id
  active Boolean
  rank   Int
  @@query.parent("FxParent")
  @@query.filter("live", parent.default_filter & rank > 1)
}
"""


@pytest.mark.skipif(not has_extension("monorm.query-defaults"), reason="requires the monorm.query-defaults package")
async def test_a_child_filter_extends_the_filter_of_its_parent():
    registry = monorm.Registry()
    models = monorm.loads(PARENT, registry=registry)
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    try:
        Child = models["FxChild"]
        await Child.objects.using(db).insert_many(
            [{"id": 1, "active": True, "rank": 1}, {"id": 2, "active": True, "rank": 2}, {"id": 3, "active": False, "rank": 3}]
        )
        assert ids(await Child.objects.using(db).order_by(Child.id)) == [2]
        assert ids(await Child.objects.using(db).without_defaults("live").order_by(Child.id)) == [1, 2, 3]
        with pytest.raises(monorm.SchemaError, match="parent.default_filter has no inherited filter"):
            monorm.loads(PARENT.replace('@@query.parent("FxParent")', ""), registry=monorm.Registry())
    finally:
        await db.close()


# (name, filter expression, ids that the filter keeps); every case runs on both databases.
ROW_SHAPES = [
    ("in", 'rank.in_([1, 4])', [1, 3]),
    ("not_in", 'rank.not_in([1, 4])', [2, 4, 5]),
    ("like_case", 'title.like("T%")', [4]),
    ("like_lower", 'title.like("t%")', [1, 2, 3]),
    ("ilike", 'title.ilike("T%")', [1, 2, 3, 4]),
    ("function", 'lower(email) == "a@x.io"', [1, 4]),
    ("arith", "rank * 2 > 7", [3, 5]),
    ("enum", "status == ACTIVE", [1, 3, 5]),
    ("enum_ne", "status != ACTIVE", [2, 4]),
]
ROW_SHAPE_ROWS = [
    {"id": 1, "title": "tea", "rank": 1, "email": "A@x.io", "status": "ACTIVE"},
    {"id": 2, "title": "toast", "rank": 2, "email": "b@x.io", "status": "ARCHIVED"},
    {"id": 3, "title": "tart", "rank": 4, "email": "c@x.io", "status": "ACTIVE"},
    {"id": 4, "title": "Tin", "rank": 3, "email": "a@x.io", "status": "ARCHIVED"},
    {"id": 5, "title": "bun", "rank": 5, "email": "d@x.io", "status": "ACTIVE"},
]


def shape_source(provider):
    models = "".join(
        f'model FxShape_{name} {{\n  id Int @id\n  title String\n  rank Int\n  email String\n  status FxStatus\n  @@map("fx_shape_{name}")\n  @@query.filter("f", {expression})\n}}\n'
        for name, expression, _ in ROW_SHAPES
    )
    return f'datasource db {{\n  provider = "{provider}"\n}}\nenum FxStatus {{\n  ACTIVE\n  ARCHIVED\n  @@storage(text)\n}}\n{models}'


@pytest.fixture(params=["sqlite", "postgresql"])
async def shapes(request):
    provider = request.param
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    registry = monorm.Registry()
    models = monorm.loads(shape_source(provider), registry=registry)
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    for name, _, _ in ROW_SHAPES:
        await models[f"FxShape_{name}"].objects.using(db).insert_many(ROW_SHAPE_ROWS)
    yield db, models
    await db.drop_tables()
    await db.close()


@pytest.mark.parametrize("name", [s[0] for s in ROW_SHAPES])
async def test_each_filter_shape_keeps_the_same_rows_on_both_databases(shapes, name):
    db, models = shapes
    Model = models[f"FxShape_{name}"]
    expected = next(kept for n, _, kept in ROW_SHAPES if n == name)
    assert ids(await Model.objects.using(db).order_by(Model.id)) == expected


# The string form and the native form of a filter give the same generated models (story 31).
CONVERT = """model FxConvert {
  id   Int @id
  rank Int
  @@query.defaults(filter: "rank > 1 and rank != 7", fields: ["id", "rank"], related: [])
  @@query.filter("big", "rank > 1")
}
"""


def run_cli(cwd, *args):
    return subprocess.run([sys.executable, "-m", "monorm", *args], cwd=cwd, capture_output=True, text=True, env={**os.environ, "MONORM_DATABASE_URL": ""})


@pytest.mark.skipif(not has_extension("monorm.query-defaults"), reason="requires the monorm.query-defaults package")
def test_fmt_native_converts_string_filters_and_leaves_the_generated_models_unchanged(tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text("// keep me\n" + CONVERT)

    def generated():
        for language, target in (("python", "models.py"), ("typescript", "models.ts")):
            done = run_cli(tmp_path, "--schema", str(schema), "generate", language, "-o", str(tmp_path / target))
            assert done.returncode == 0, done.stderr
        return (tmp_path / "models.py").read_text(), (tmp_path / "models.ts").read_text()

    before = generated()
    warned = run_cli(tmp_path, "check", "--schema", str(schema))
    assert warned.returncode == 0, warned.stderr
    assert warned.stderr.count("this filter is a string; write the expression:") == 2, warned.stderr
    assert "schema.prisma:5:" in warned.stderr and "schema.prisma:6:" in warned.stderr
    converted = run_cli(tmp_path, "fmt", "--native", "--schema", str(schema))
    assert converted.returncode == 0, converted.stderr
    text = schema.read_text()
    assert text.startswith("// keep me\n")
    assert '"rank > 1' not in text and "filter: (rank > 1) & (rank != 7)" in text and '@@query.filter("big", rank > 1)' in text
    assert generated() == before
    assert "warning" not in run_cli(tmp_path, "check", "--schema", str(schema)).stderr
    again = run_cli(tmp_path, "fmt", "--native", "--schema", str(schema))
    assert again.returncode == 0 and schema.read_text() == text
