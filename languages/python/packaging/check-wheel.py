"""Check native namespace separation and the absence of native payload in monorm."""

from pathlib import Path
import sys
from zipfile import ZipFile

wheels = Path(sys.argv[1])
profile = sys.argv[2]
thin = next(wheels.glob("monorm-*.whl"))
with ZipFile(thin) as archive:
    assert not any(name.endswith((".so", ".pyd", ".dylib", ".dll")) for name in archive.namelist())
    assert "monorm/_native.py" in archive.namelist()
    assert not any("monorm_native_" in name for name in archive.namelist())
native = next(wheels.glob(f"monorm_native_{profile}-*.whl"))
with ZipFile(native) as archive:
    payload = [name for name in archive.namelist() if name.endswith((".so", ".pyd"))]
    assert len(payload) == 1 and payload[0].startswith(f"monorm_native_{profile}/_native.")
    assert not any(name.startswith("monorm/") for name in archive.namelist())
print(f"wheel contents verified: {profile}")
