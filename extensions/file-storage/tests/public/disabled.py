"""Without the `orm-file-storage` package, `@storage.file` is rejected with the namespace error."""

import orm
from orm._packages import resolve_extensions

assert "orm.file-storage" not in resolve_extensions(), "run this script in an environment without the package"
try:
    orm.loads('model Bad {\n id Int @id\n file Json? @storage.file(storage: "reports")\n}')
except orm.SchemaError as error:
    assert "@storage.file" in str(error) and "@storage.*" in str(error) and "install its package" in str(error), error
else:
    raise AssertionError("a build without the package accepted @storage.file")
print("Disabled file storage (Python): passed")
