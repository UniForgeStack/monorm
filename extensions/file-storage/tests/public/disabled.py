"""Without the `monorm-file-storage` package, `@storage.file` is rejected with the namespace error."""

import monorm
from monorm._packages import resolve_extensions

assert "monorm.file-storage" not in resolve_extensions(), "run this script in an environment without the package"
try:
    monorm.loads('model Bad {\n id Int @id\n file Json? @storage.file(storage: "reports")\n}')
except monorm.SchemaError as error:
    assert "@storage.file" in str(error) and "@storage.*" in str(error) and "install its package" in str(error), error
else:
    raise AssertionError("a build without the package accepted @storage.file")
print("Disabled file storage (Python): passed")
