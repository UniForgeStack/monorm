"""Without the `orm-query-defaults` package, public syntax is rejected; neutral prepared data is accepted."""

import orm
from orm._packages import resolve_extensions

assert "orm.query-defaults" not in resolve_extensions(), "run this script in an environment without the package"
registry = orm.Registry()
plain = {
    "models": [
        {
            "name": "Plain",
            "table": "plain",
            "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": True}],
        }
    ],
    "behavior": {
        "schema_contract": 5,
        "read_defaults": [{"source": "another-extension", "name": "selection", "model": "Plain", "fields": []}],
    },
}
orm.define(plain, registry=registry)
before = registry.ir()
try:
    orm.loads('model Bad {\n id Int @id\n @@query.filter("positive", "id > 0")\n}', registry=registry)
except orm.SchemaError as error:
    assert "@query.filter" in str(error) and "rebuild" in str(error), error
    assert "@@query.*" in str(error) and "docs/README.md" in str(error), error
else:
    raise AssertionError("disabled artifact accepted @@query syntax")
assert registry.ir() == before
print("Disabled Python syntax/neutral contract: passed")
