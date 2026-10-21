"""`@@query_set` and `@@model_methods`: the module lookup, consumer replace, import cycle and collisions."""

import importlib
import sys
import types

import pytest

import orm
from orm import _native
from orm.model import Registry
from orm.query import QuerySet

SCHEMA = 'datasource db {\n provider = "sqlite"\n}\nmodel User {\n id Int @id\n password String\n @@query_set("UserQueries")\n @@model_methods("UserMethods")\n}\n'


def _package(root, name="orm_auth", init=True):
    (root / "pyproject.toml").write_text("[project]\nname = 'x'\n")
    package = root / name
    package.mkdir()
    if init:
        (package / "__init__.py").write_text("")
    schema = package / "schema.prisma"
    schema.write_text(SCHEMA)
    return package, schema


def test_module_lookup_goes_up_through_the_package(tmp_path):
    _, schema = _package(tmp_path)
    module, stub = _native.generate_python(str(schema))
    assert 'use_query_set(User, "orm_auth.queries:UserQueries")' in module
    assert "from orm_auth.methods import UserMethods as _UserMethods" in module
    assert "from orm_auth.queries import UserQueries as _UserObjects" in stub
    assert "class User(_UserMethods, Model):" in stub


def test_schema_outside_a_package_gives_the_top_level_module(tmp_path):
    (tmp_path / "pyproject.toml").write_text("[project]\nname = 'x'\n")
    schema = tmp_path / "schema.prisma"
    schema.write_text(SCHEMA)
    module, _ = _native.generate_python(str(schema))
    assert 'use_query_set(User, "queries:UserQueries")' in module
    assert "from methods import UserMethods as _UserMethods" in module


def test_a_namespace_package_gives_the_path_from_src(tmp_path):
    (tmp_path / "pyproject.toml").write_text("[project]\nname = 'x'\n")
    package = tmp_path / "src" / "acme" / "auth"
    package.mkdir(parents=True)
    (package / "__init__.py").write_text("")
    (package / "schema.prisma").write_text(SCHEMA)
    module, _ = _native.generate_python(str(package / "schema.prisma"))
    assert 'use_query_set(User, "acme.auth.queries:UserQueries")' in module
    assert "from acme.auth.methods import UserMethods as _UserMethods" in module


def test_a_schema_in_a_plain_subdirectory_gives_the_path_from_the_project_root(tmp_path):
    _, schema = _package(tmp_path, name="db", init=False)
    module, _ = _native.generate_python(str(schema))
    assert 'use_query_set(User, "db.queries:UserQueries")' in module


def test_a_schema_with_no_import_root_asks_for_the_full_path(tmp_path):
    root = tmp_path / "lonely"
    root.mkdir()
    (root / "schema.prisma").write_text(SCHEMA)
    with pytest.raises(_native.SchemaError, match=r"queries:UserQueries.*--query-set / --model-methods"):
        _native.generate_python(str(root / "schema.prisma"))
    module, _ = _native.generate_python(
        str(root / "schema.prisma"), {"User": "orm_auth.queries:UserQueries"}, {"User": "orm_auth.methods:UserMethods"}
    )
    assert "from orm_auth.methods import UserMethods" in module


def test_a_consumer_target_replaces_the_schema_class(tmp_path):
    package, schema = _package(tmp_path)
    module, _ = _native.generate_python(
        str(schema), {"User": "mine.queries:MyQueries"}, {"User": "mine.methods:MyMethods"}
    )
    assert 'use_query_set(User, "mine.queries:MyQueries")' in module
    assert "from mine.methods import MyMethods as _UserMethods" in module
    assert "orm_auth.queries" not in module and "orm_auth.methods" not in module
    output = package / "out.py"
    args = ["--schema", str(schema), "generate", "python", "-o", str(output)]
    assert _native.cli([*args, "--model-methods", "User=mine.methods:MyMethods"]) == 0
    assert "from mine.methods import MyMethods as _UserMethods" in output.read_text()
    assert _native.cli([*args, "--model-methods", "User=mine.methods:A", "--model-methods", "User=mine.methods:B"]) != 0
    assert _native.cli([*args, "--model-methods", "Nope=mine.methods:A"]) != 0


async def test_the_methods_module_may_import_the_models_under_type_checking(tmp_path, monkeypatch):
    package, schema = _package(tmp_path)
    (package / "methods.py").write_text(
        "from __future__ import annotations\n"
        "from typing import TYPE_CHECKING\n"
        "if TYPE_CHECKING:\n"
        "    from orm_auth.models import User\n\n"
        "class UserMethods:\n"
        "    def check_password(self: User, raw: str) -> bool:\n"
        "        return self.password == raw\n"
    )
    (package / "queries.py").write_text(
        "from __future__ import annotations\n"
        "from typing import TYPE_CHECKING\n"
        "from orm import QuerySet\n"
        "if TYPE_CHECKING:\n"
        "    from orm_auth.models import User\n\n"
        "class UserQueries(QuerySet['User']):\n"
        "    pass\n"
    )
    assert _native.cli(["--schema", str(schema), "generate", "python"]) == 0
    monkeypatch.syspath_prepend(str(tmp_path))
    registry = Registry()
    monkeypatch.setattr(orm.model, "registry", registry)
    try:
        models = importlib.import_module("orm_auth.models")
        assert models.User.__mro__[1].__name__ == "UserMethods"
        db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
        try:
            await db.create_tables()
            user = await models.User.objects.using(db).insert(id=1, password="secret")
            assert user.check_password("secret") and not user.check_password("nope")
            assert (await models.User.objects.using(db).get(models.User.id == 1)).check_password("secret")
            assert type(models.User.objects).__name__ == "UserQueries"
        finally:
            await db.close()
    finally:
        for name in [n for n in sys.modules if n == "orm_auth" or n.startswith("orm_auth.")]:
            del sys.modules[name]


@pytest.mark.parametrize("name", ["password", "delete", "objects", "_meta", "__eq__", "__hash__"])
def test_a_method_named_like_a_model_member_is_an_error(name):
    methods = type("UserMethods", (), {name: lambda self: None})
    ir = _native.compile_schema(SCHEMA)
    with pytest.raises(TypeError, match=f"User.{name}: model method collision"):
        orm.model.define(ir, registry=Registry(), model_methods={"User": methods})


async def _insert_user(models, registry):
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        return await models["User"].objects.using(db).insert(id=1, password="x")
    finally:
        await db.close()


async def test_a_methods_class_may_define_the_display_members():
    class UserMethods:
        def __str__(self):
            return f"user {self.password}"

        def __repr__(self):
            return "<User>"

    registry = Registry()
    user = await _insert_user(orm.loads(SCHEMA, registry=registry, model_methods={"User": UserMethods}), registry)
    assert str(user) == "user x" and repr(user) == "<User>"


async def test_load_and_loads_take_the_model_methods(tmp_path):
    class UserMethods:
        def shout(self):
            return self.password.upper()

    path = tmp_path / "schema.prisma"
    path.write_text(SCHEMA)
    for load in (lambda r: orm.loads(SCHEMA, registry=r, model_methods={"User": UserMethods}),
                 lambda r: orm.load(path, registry=r, model_methods={"User": UserMethods})):
        registry = Registry()
        assert (await _insert_user(load(registry), registry)).shout() == "X"
    with pytest.raises(TypeError, match="User.password: model method collision"):
        orm.loads(SCHEMA, registry=Registry(), model_methods={"User": type("M", (), {"password": 1})})


def test_a_method_named_like_an_extension_member_is_an_error(monkeypatch):
    class Instance:
        def restore(self):
            return "extension"

    class Bound(QuerySet):
        pass

    record = {
        "query_set": "Set", "instance": "Instance", "bound_query_set": "", "overrides": [],
        "query_set_members": [], "instance_members": ["restore"], "bound_query_set_members": [],
    }
    extension = types.SimpleNamespace(Set=Bound, Instance=Instance)
    monkeypatch.setattr(orm._extensions, "check", lambda extension_id, module: record)
    for name, ok in (("restore", False), ("other", True)):
        methods = type("UserMethods", (), {name: lambda self: None})
        define = lambda: orm.model.define(  # noqa: E731
            _native.compile_schema(SCHEMA), registry=Registry(),
            extensions={"fake": extension}, members={"User": ("fake",)}, model_methods={"User": methods},
        )
        if ok:
            define()
        else:
            with pytest.raises(TypeError, match=f"User.{name}: model method collision"):
                define()
