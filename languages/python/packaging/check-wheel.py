"""Check the payload of the one `monorm` wheel: the engine, the library of each built-in extension, and auth.

    python languages/python/packaging/check-wheel.py <wheel>
"""

import re
import sys
from pathlib import Path
from zipfile import ZipFile

MODULES = (
    "monorm_timestamps monorm_soft_delete monorm_locking monorm_inherit monorm_query_defaults "
    "monorm_generic monorm_proxy monorm_composition monorm_file_storage"
).split()
LIBRARY = re.compile(r"[^/]*\.(so|pyd|dylib|dll)$")

wheel = Path(sys.argv[1])
assert re.fullmatch(r"monorm-[^-]+-.*(macosx|manylinux|win)[^-]*\.whl", wheel.name), f"{wheel.name} is not a platform wheel of monorm"
with ZipFile(wheel) as archive:
    names = archive.namelist()
    libraries = [n for n in names if LIBRARY.search(n)]
    engine = [n for n in libraries if n.startswith("monorm/_native.")]
    assert len(engine) == 1, engine
    assert "monorm/_native.py" not in names and "monorm/_profiles.py" not in names
    for module in MODULES:
        assert f"{module}/monorm-extension.toml" in names and f"{module}/__init__.py" in names, module
        own = [n for n in libraries if n.startswith(f"{module}/")]
        assert len(own) == 1 and "/" not in own[0][len(module) + 1 :], (module, own)
    assert len(libraries) == 1 + len(MODULES), libraries
    assert "monorm_auth/schema.prisma" in names and "monorm_auth/py.typed" in names
    assert not any("monorm_native_" in n for n in names)
    entry_points = next(archive.read(n).decode() for n in names if n.endswith(".dist-info/entry_points.txt"))
    assert entry_points.count("monorm_") >= len(MODULES) + 1 and "[monorm.apps]" in entry_points
print(f"wheel contents verified: {wheel.name}")
