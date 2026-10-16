"""Native generators use the same explicit query-set mappings as the CLI."""

import pytest

from orm import _native


def test_generate_python_query_sets(tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text("model Post {\n id Int @id\n}\n")
    output = tmp_path / "models.py"
    args = ["--schema", str(schema), "generate", "python", "-o", str(output)]
    assert _native.cli(args) == 0
    baseline = (output.read_text(), output.with_suffix(".pyi").read_text())
    assert _native.generate_python(str(schema)) == baseline
    assert _native.generate_python(str(schema), None) == baseline
    assert _native.generate_python(str(schema), {}) == baseline

    assert _native.cli([*args, "--query-set", "Post=app.queries:PostQueries"]) == 0
    module, stub = _native.generate_python(str(schema), query_sets={"Post": "app.queries:PostQueries"})
    assert (module, stub) == (output.read_text(), output.with_suffix(".pyi").read_text())
    assert 'use_query_set(Post, "app.queries:PostQueries")' in module
    assert "from app.queries import PostQueries as _PostObjects" in stub
    assert "objects: ClassVar[_PostObjects]" in stub


@pytest.mark.parametrize("query_sets, message", [
    ({"Missing": "app.queries:PostQueries"}, "no model Missing"),
    ({"Post": "./queries.js#PostQueries"}, "module:Class"),
    ({"Post": "None:Q"}, "absolute module name"),
    ({"Post": "app.class:Q"}, "absolute module name"),
    ({"Post": "app:class"}, "module:Class"),
])
def test_generate_python_checks_query_sets(tmp_path, query_sets, message):
    schema = tmp_path / "schema.prisma"
    schema.write_text("model Post {\n id Int @id\n}\n")
    with pytest.raises(_native.SchemaError, match=message):
        _native.generate_python(str(schema), query_sets)
