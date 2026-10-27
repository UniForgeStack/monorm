"""Extension packages: the Python discovery through the `orm.extensions` entry point.

The fake extensions have ids that no real package uses, so the tests also pass in a venv with real extensions installed.
"""

import shutil
import hashlib
import importlib.util
import json
import sys
import warnings
import subprocess
from pathlib import Path

import pytest

import orm
from orm.__main__ import main as cli
from orm import _native
from orm._native import SchemaError
from orm._packages import package_dirs, resolve_extensions
from orm.migrations import Migrations

MANIFEST = 'id = "acme.fixture"\nversion = "0.1.0"\nabi = "0.2"\nlibrary = "libfixture"\n'


def install(site: Path, dist: str, entry: str | None, *, manifest: bool = True, ident: str | None = None) -> Path:
    """A package `<dist>` with `orm-extension.toml`, and a hand-written `.dist-info` that declares the entry point."""
    package = dist.replace("-", "_")
    (site / package).mkdir(parents=True)
    (site / package / "__init__.py").write_text("raise RuntimeError('an extension package must not be imported')\n")
    if manifest:
        (site / package / "orm-extension.toml").write_text(MANIFEST)
    info = site / f"{package}-0.1.dist-info"
    info.mkdir()
    (info / "METADATA").write_text(f"Metadata-Version: 2.1\nName: {dist}\nVersion: 0.1\n")
    if entry is not None:
        (info / "entry_points.txt").write_text(f"[orm.extensions]\n{ident or 'acme.fixture'} = {entry}\n")
    if ident == "acme.outside":
        (site / package / "__init__.py").write_text((OUTSIDE / "python" / "__init__.py").read_text())
        sys.modules.pop(package, None)
    return site / package / "orm-extension.toml"


@pytest.fixture
def site(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    site = tmp_path / "site-packages"
    site.mkdir()
    monkeypatch.syspath_prepend(str(site))
    return site


def test_an_installed_extension_resolves_by_id_without_importing_its_package(site: Path) -> None:
    manifest = install(site, "acme-fixture", "acme_fixture")
    assert resolve_extensions()["acme.fixture"] == str(manifest)


def test_a_dotted_module_resolves_to_its_directory(site: Path) -> None:
    (site / "acme_ns" / "inner").mkdir(parents=True)
    (site / "acme_ns" / "__init__.py").write_text("")
    (site / "acme_ns" / "inner" / "orm-extension.toml").write_text(MANIFEST)
    info = site / "acme_ns-0.1.dist-info"
    info.mkdir()
    (info / "METADATA").write_text("Metadata-Version: 2.1\nName: acme-ns\nVersion: 0.1\n")
    (info / "entry_points.txt").write_text("[orm.extensions]\nacme.ns = acme_ns.inner\n")
    assert resolve_extensions()["acme.ns"] == str(site / "acme_ns" / "inner" / "orm-extension.toml")


def test_a_package_without_a_manifest_names_the_entry_point(site: Path) -> None:
    install(site, "acme-fixture", "acme_fixture", manifest=False)
    with pytest.raises(SchemaError, match=r"entry point of `acme.fixture`.*orm-extension.toml"):
        resolve_extensions()


def test_two_packages_with_one_id_name_both_paths(site: Path) -> None:
    first = install(site, "acme-fixture", "acme_fixture")
    second = install(site, "acme-fixture-two", "acme_fixture_two")
    with pytest.raises(SchemaError) as error:
        resolve_extensions()
    assert "installed twice" in str(error.value)
    assert str(first) in str(error.value) and str(second) in str(error.value)


OUTSIDE = Path(__file__).resolve().parents[3] / "tests" / "outside-extension"


@pytest.fixture(scope="session")
def outside_library(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """The library of `tests/outside-extension`, built in its own workspace."""
    target = tmp_path_factory.mktemp("outside-target")
    subprocess.run(
        ["cargo", "build", "--offline", "--quiet", "--manifest-path", str(OUTSIDE / "Cargo.toml"), "--target-dir", str(target)],
        check=True,
    )
    return next(p for p in (target / "debug").glob("liboutside_extension.*") if p.suffix in {".dylib", ".so", ".dll"})


async def test_an_installed_outside_library_lowers_the_schema_through_load_and_loads(
    site: Path, outside_library: Path, tmp_path: Path
) -> None:
    manifest = install(site, "acme-outside", "acme_outside", ident="acme.outside")
    manifest.write_text((OUTSIDE / "orm-extension.toml").read_text())
    shutil.copy(outside_library, manifest.parent / outside_library.name)
    source = 'datasource db { provider = "sqlite" }\nmodel Post {\n id Int @id\n @@outside.tag("blue")\n}\n'
    LoadedPost = orm.loads(source, registry=orm.Registry())["Post"]
    assert LoadedPost._meta.fields["tag"].default == "blue"
    assert "LIMIT 7" in LoadedPost.objects.tagged().sql()
    schema = tmp_path / "schema.prisma"
    schema.write_text(source)
    registry = orm.Registry()
    Post = orm.load(schema, registry=registry)["Post"]
    assert "tag" in Post._meta.fields
    assert "LIMIT 7" in Post.objects.tagged().sql()
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        row = await Post.objects.using(db).insert(id=1, tag="blue")
        assert row.tag_label() == "blue"
    finally:
        await db.close()

    manifest.write_text(manifest.read_text().replace('languages = ["python", "typescript"]', 'languages = ["typescript"]'))
    with pytest.raises(SchemaError, match="does not support this language binding"):
        orm.load(schema, registry=orm.Registry())


def test_the_command_line_lowers_the_schema_with_an_installed_outside_library(
    site: Path, outside_library: Path, tmp_path: Path, capfd: pytest.CaptureFixture[str], monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = install(site, "acme-outside", "acme_outside", ident="acme.outside")
    manifest.write_text((OUTSIDE / "orm-extension.toml").read_text())
    shutil.copy(outside_library, manifest.parent / outside_library.name)
    (tmp_path / "schema.prisma").write_text('model Post {\n id Int @id\n @@outside.tag("blue")\n}\n')
    monkeypatch.chdir(tmp_path)
    assert cli(["check"]) == 0
    assert cli(["makemigrations"]) == 0
    capfd.readouterr()
    assert cli(["sqlmigrate", "1"]) == 0
    assert '"tag"' in capfd.readouterr().out

    assert cli(["generate"]) == 0
    assert cli(["check"]) == 0
    original = manifest.read_text()
    manifest.write_text(original + "\n# changed after generation\n")
    capfd.readouterr()
    assert cli(["check"]) == 1
    error = capfd.readouterr().err
    assert "acme.outside" in error and "run `orm generate`" in error
    assert "locked" in error and "installed" in error
    manifest.write_text(original)
    assert cli(["check"]) == 0


async def test_packaged_generated_models_use_runtime_without_the_pass_library(site: Path, outside_library: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    manifest = install(site, "acme-outside", "acme_outside", ident="acme.outside")
    manifest.write_bytes((OUTSIDE / "orm-extension.toml").read_bytes())
    library = manifest.parent / outside_library.name
    shutil.copy(outside_library, library)
    (tmp_path / "schema.prisma").write_text('datasource db { provider = "sqlite" }\nmodel Post {\n id Int @id\n @@outside.tag("blue")\n}\n')
    monkeypatch.chdir(tmp_path)
    assert cli(["generate"]) == 0
    # Compiled static extensions can write sources here; packaged extensions cannot.
    assert not (tmp_path / "orm_ext" / "acme_outside").exists()
    library.unlink()  # define() must need neither the pass library nor discovery.
    registry = orm.Registry()
    monkeypatch.setattr(orm.model, "registry", registry)
    spec = importlib.util.spec_from_file_location("outside_generated", tmp_path / "models.py")
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        spec.loader.exec_module(module)
    assert not caught
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        row = await module.Post.objects.using(db).insert(id=1, tag="blue")
        assert row.tag_label() == "blue"
    finally:
        await db.close()
    assert "LIMIT 7" in module.Post.objects.tagged().sql()
    import acme_outside
    assert acme_outside.MANIFEST_HASH == hashlib.sha256(manifest.read_bytes()).hexdigest()
    schema = json.loads(module._SCHEMA)
    for installed in ("changed", None):
        if installed is None:
            del acme_outside.MANIFEST_HASH
        else:
            acme_outside.MANIFEST_HASH = installed
        with pytest.warns(orm.OrmStaleModelsWarning, match="acme.outside.*run `orm generate`") as caught:
            Post = orm.define(schema, registry=orm.Registry(), extensions={"acme.outside": acme_outside})["Post"]
        assert len(caught) == 1
        assert "locked manifest hash" in str(caught[0].message)
        assert "LIMIT 7" in Post.objects.tagged().sql()
    acme_outside.MANIFEST_HASH = hashlib.sha256(manifest.read_bytes()).hexdigest()
    monkeypatch.delattr(acme_outside, "OutsideQuerySet")
    with pytest.raises(TypeError, match="extension acme.outside has no OutsideQuerySet; run `orm generate`"):
        orm.define(schema, registry=orm.Registry(), extensions={"acme.outside": acme_outside})
    schema["behavior"]["packages"]["acme.outside"]["languages"] = ["typescript"]
    with pytest.raises(SchemaError, match="does not support this language binding"):
        orm.define(schema, registry=orm.Registry(), extensions={"acme.outside": acme_outside})


GUARDED = (
    'datasource db { provider = "sqlite" }\n'
    "model Note {\n id Int @id\n text String\n start Int\n end Int\n shop_id Int\n updated_by String?\n @@outside.guard\n}\n"
)


def install_outside(site: Path, outside_library: Path) -> Path:
    manifest = install(site, "acme-outside", "acme_outside", ident="acme.outside")
    manifest.write_bytes((OUTSIDE / "orm-extension.toml").read_bytes())
    shutil.copy(outside_library, manifest.parent / outside_library.name)
    return manifest


async def test_a_hook_error_names_the_model_and_field_once(site: Path, outside_library: Path) -> None:
    install_outside(site, outside_library)
    registry = orm.Registry()
    Note = orm.loads(GUARDED, registry=registry)["Note"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        with orm.scope(shop=1, user="ann"):
            with pytest.raises(orm.QueryError) as empty:
                await Note.objects.using(db).insert(id=1, text="  ", start=1, end=2, shop_id=1)
            with pytest.raises(orm.QueryError) as ordered:
                await Note.objects.using(db).insert(id=1, text="x", start=3, end=1, shop_id=1)
        assert str(empty.value).endswith("Note.text: must not be empty")
        assert str(ordered.value).endswith("Note: start 3 must be before end 1")
        assert "extension" not in f"{empty.value}{ordered.value}"
    finally:
        await db.close()


async def test_a_value_that_the_caller_sets_goes_through_the_hook_that_assigns_the_field(site: Path, outside_library: Path) -> None:
    install_outside(site, outside_library)
    registry = orm.Registry()
    Note = orm.loads(GUARDED, registry=registry)["Note"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        notes = Note.objects.using(db)
        with orm.scope(shop=1, user="ann"):
            rows = [{"id": i, "text": "a", "start": 1, "end": 2, "shop_id": 1, "updated_by": "mallory"} for i in (1, 2, 3)]
            await notes.insert_many(rows)
            # An insert keeps the value that the caller supplies; the hook assigns on update only.
            assert {n.updated_by for n in await notes.all()} == {"mallory"}
            await (await notes.get(Note.id == 1)).update(updated_by="eve")
            assert (await notes.get(Note.id == 1)).updated_by == "ann"
            assert await notes.filter(Note.id > 1).update(updated_by="eve") == 2
            assert {n.updated_by for n in await notes.all()} == {"ann"}
        with orm.scope(shop=1):
            with pytest.raises(orm.QueryError, match="no user in the request scope"):
                await notes.filter(Note.id == 1).update(updated_by="eve")
    finally:
        await db.close()


def test_a_native_schema_without_package_directories_leaves_the_hook_bindings_unresolved(site: Path, outside_library: Path, tmp_path: Path) -> None:
    install_outside(site, outside_library)
    schema = tmp_path / "schema.prisma"
    schema.write_text(GUARDED)
    ir = _native.compile_schema_file(str(schema), {}, resolve_extensions())
    _native.Schema(ir)
    with pytest.raises(SchemaError, match="acme.outside: the schema binds its hooks, but no package directory was given"):
        _native.Schema(ir, None, {})
    assert _native.Schema(ir, None, package_dirs()) is not None
    assert Migrations(tmp_path / "migrations", schema).make() is not None


def test_define_needs_the_library_of_a_package_whose_hooks_the_schema_binds(site: Path, outside_library: Path, tmp_path: Path) -> None:
    manifest = install_outside(site, outside_library)
    source = tmp_path / "schema.prisma"
    source.write_text(GUARDED)
    schema = json.loads(_native.compile_schema_file(str(source), {}, resolve_extensions()))
    orm.define(schema, registry=orm.Registry())
    (manifest.parent / outside_library.name).unlink()
    with pytest.raises(SchemaError, match=r"extension acme\.outside: library .*No such file or directory"):
        orm.define(schema, registry=orm.Registry())


def test_hash_only_package_is_imported_by_generated_models(site: Path, outside_library: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    manifest = install(site, "acme-outside", "acme_outside", ident="acme.outside")
    manifest.write_text((OUTSIDE / "orm-extension.toml").read_text().split("\n[python]")[0])
    digest = hashlib.sha256(manifest.read_bytes()).hexdigest()
    (manifest.parent / "__init__.py").write_text(f'MANIFEST_HASH = "{digest}"\n')
    library = manifest.parent / outside_library.name
    shutil.copy(outside_library, library)
    (tmp_path / "schema.prisma").write_text('model Plain {\n id Int @id\n @@outside.tag("blue")\n}\n')
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(orm.model, "registry", orm.Registry())
    assert cli(["generate"]) == 0
    # Compiled static extensions can write sources here; packaged extensions cannot.
    assert not (tmp_path / "orm_ext" / "acme_outside").exists()
    library.unlink()
    spec = importlib.util.spec_from_file_location("hash_only_generated", tmp_path / "models.py")
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        spec.loader.exec_module(module)
    assert not caught
    assert sys.modules["acme_outside"].MANIFEST_HASH == digest
    assert not hasattr(module.Plain.objects, "tagged")


async def test_an_outside_attribute_owns_a_field_type_and_takes_a_field_out_of_the_model(site: Path, outside_library: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capfd: pytest.CaptureFixture[str]) -> None:
    manifest = install(site, "acme-outside", "acme_outside", ident="acme.outside")
    manifest.write_text((OUTSIDE / "orm-extension.toml").read_text())
    shutil.copy(outside_library, manifest.parent / outside_library.name)
    schema = tmp_path / "schema.prisma"
    schema.write_text('datasource db { provider = "sqlite" }\nmodel Order {\n id Int @id\n price Money @outside.money\n memo String @outside.note("remember")\n}\n')
    monkeypatch.chdir(tmp_path)
    assert cli(["check"]) == 1
    assert "orm identities" in capfd.readouterr().err
    assert cli(["identities"]) == 0
    assert cli(["generate"]) == 0
    assert json.loads(schema.with_suffix(".identities.json").read_text())["entries"][0]["model"] == "Order"
    Order = orm.load(schema, registry=(registry := orm.Registry()))["Order"]
    assert list(Order._meta.fields) == ["id", "price_amount", "price_currency"]
    assert registry.ir()["behavior"]["extension_data"]["acme.outside"]["notes"]["Order.memo"] == "remember"
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await Order.objects.using(db).insert_many([{"id": 1, "price_amount": 250, "price_currency": "EUR"}])
        [row] = await Order.objects.using(db).all()
        assert (row.price_amount, row.price_currency) == (250, "EUR")
    finally:
        await db.close()


async def test_an_outside_view_reads_and_writes_through_the_row_filter_and_warns_with_its_own_code(
    site: Path, outside_library: Path, capfd: pytest.CaptureFixture[str]
) -> None:
    manifest = install(site, "acme-outside", "acme_outside", ident="acme.outside")
    manifest.write_text((OUTSIDE / "orm-extension.toml").read_text())
    shutil.copy(outside_library, manifest.parent / outside_library.name)
    source = (
        'datasource db { provider = "sqlite" }\n'
        "model Ticket {\n id Int @id\n status String\n note String?\n}\n"
        'model OpenTicket {\n id Int @id\n status String\n note String\n @@outside.view(Ticket, "open")\n}\n'
    )
    registry = orm.Registry()
    models = orm.loads(source, registry=registry)
    Ticket, OpenTicket = models["Ticket"], models["OpenTicket"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await Ticket.objects.using(db).insert_many(
            [
                {"id": 1, "status": "open", "note": None},
                {"id": 2, "status": "closed", "note": "kept"},
                {"id": 3, "status": "open", "note": "seen"},
            ]
        )
        capfd.readouterr()
        rows = await OpenTicket.objects.using(db).order_by(OpenTicket.id)
        assert [r.id for r in rows] == [1, 3]
        warnings = [json.loads(line) for line in capfd.readouterr().err.splitlines() if line.startswith('{"code":"acme.outside.shape"')]
        assert [(w["model"], w["field"], w["occurrence_count"]) for w in warnings] == [("OpenTicket", "note", 1)]
        assert await OpenTicket.objects.using(db).filter(OpenTicket.id == 2).count() == 0
        await OpenTicket.objects.using(db).insert(id=4, status="open", note="new")
        assert (await Ticket.objects.using(db).filter(Ticket.id == 4).count()) == 1
        await OpenTicket.objects.using(db).filter(OpenTicket.id == 2).update(note="changed")
        await OpenTicket.objects.using(db).filter(OpenTicket.id == 2).delete()
        [closed] = await Ticket.objects.using(db).filter(Ticket.id == 2)
        assert closed.note == "kept"
    finally:
        await db.close()
