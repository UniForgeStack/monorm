"""Without the producer, public syntax is rejected; neutral prepared data is accepted."""

import json

import orm
from orm import _native as module

assert "query-defaults" not in json.loads(module.native_artifact())["capabilities"]
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
        "schema_contract": 2,
        "read_defaults": [{"source": "another-extension", "name": "selection", "model": "Plain", "fields": []}],
    },
}
orm.define(plain, registry=registry)
before = registry.ir()
try:
    orm.loads('model Bad {\n id Int @id\n @@query.filter("id > 0")\n}', registry=registry)
except orm.SchemaError as error:
    assert "@query.filter" in str(error) and "rebuild" in str(error), error
else:
    raise AssertionError("disabled artifact accepted @@query syntax")
assert registry.ir() == before
print("Disabled Python syntax/neutral contract: passed")
