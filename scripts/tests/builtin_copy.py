"""Write an outside package that declares the id of a built-in extension: a wheel and an npm package directory.

usage: python3 scripts/tests/builtin_copy.py <out-dir>
The seam installs it next to monorm and expects the "installed twice" error that names the built-in copy.
"""
import base64
import csv
import hashlib
import io
import json
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=True)
manifest = (ROOT / "extensions/timestamps/monorm-extension.toml").read_bytes()
files = {
    "acme_copy/__init__.py": b"",
    "acme_copy/monorm-extension.toml": manifest,
    "acme_copy-0.1.0.dist-info/METADATA": b"Metadata-Version: 2.1\nName: acme-copy\nVersion: 0.1.0\n",
    "acme_copy-0.1.0.dist-info/WHEEL": b"Wheel-Version: 1.0\nGenerator: builtin_copy\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    "acme_copy-0.1.0.dist-info/entry_points.txt": b"[monorm.extensions]\nmonorm.timestamps = acme_copy\n",
}
records = []
with zipfile.ZipFile(out / "acme_copy-0.1.0-py3-none-any.whl", "w") as archive:
    for name, data in files.items():
        digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
        records.append((name, f"sha256={digest}", len(data)))
        archive.writestr(name, data)
    stream = io.StringIO(newline="")
    csv.writer(stream).writerows([*records, ("acme_copy-0.1.0.dist-info/RECORD", "", "")])
    archive.writestr("acme_copy-0.1.0.dist-info/RECORD", stream.getvalue())
npm = out / "acme-copy"
npm.mkdir(exist_ok=True)
(npm / "monorm-extension.toml").write_bytes(manifest)
(npm / "index.js").write_text("export {};\n")
(npm / "package.json").write_text(json.dumps({"name": "acme-copy", "version": "0.1.0", "type": "module", "main": "index.js", "monorm": {"extension": "monorm.timestamps"}}))
print(out / "acme_copy-0.1.0-py3-none-any.whl")
print(npm)
