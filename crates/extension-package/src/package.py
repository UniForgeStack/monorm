"""Assemble a wheel with only the Python standard library; compile TS with tsc."""
import base64
import csv
import hashlib
import io
import json
import re
import shutil
import subprocess
import sys
import sysconfig
import tempfile
import zipfile
from pathlib import Path

config = json.load(sys.stdin)
root, out = Path(config["dir"]), Path(config["out"])
manifest = config["manifest"]
ident, version = manifest["id"], manifest["version"]
if not re.fullmatch(r"[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+", ident):
    raise ValueError("extension id must be a dotted package name")
if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
    raise ValueError("package version must be major.minor.patch")
module = ident.replace(".", "_").replace("-", "_")
dist = ident.replace(".", "-").replace("_", "-")
scope, name = ident.split(".", 1)
npm_name = f"@{scope}/{name}"
raw = (root / "orm-extension.toml").read_bytes()
digest = hashlib.sha256(raw).hexdigest()
library = Path(config["library"])
lib_name = manifest["library"]
if not Path(lib_name).suffix:
    lib_name = ("" if sys.platform == "win32" else "lib") + lib_name + library.suffix


def copy_runtime(language, destination):
    section = manifest.get(language)
    destination.mkdir(parents=True, exist_ok=True)
    if section:
        source = (root / section["source"]).resolve()
        if not source.is_relative_to(root):
            raise ValueError("runtime source must be inside the extension crate")
        shutil.copytree(source, destination, dirs_exist_ok=True, ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    entry = destination / ("__init__.py" if language == "python" else "index.ts")
    text = entry.read_text() if entry.exists() else ""
    # Replace a checked-in fixture hash; the built constant always describes the shipped bytes.
    constant = "MANIFEST_HASH" if language == "python" else "manifestHash"
    text = re.sub(rf'^(?:export const )?{constant}\s*=.*\n?', "", text, flags=re.M)
    entry.write_text(text + f'\n{constant} = "{digest}"\n' if language == "python" else text + f'\nexport const {constant} = "{digest}";\n')
    (destination / "orm-extension.toml").write_bytes(raw)
    shutil.copy2(library, destination / lib_name)


with tempfile.TemporaryDirectory(prefix="orm-extension-package-") as temporary:
    stage = Path(temporary)
    package = stage / module
    copy_runtime("python", package)
    (package / "py.typed").touch()
    info = stage / f"{module}-{version}.dist-info"
    info.mkdir()
    (info / "METADATA").write_text(f"Metadata-Version: 2.1\nName: {dist}\nVersion: {version}\nRequires-Python: >=3.11\nRequires-Dist: orm==0.1.0\n")
    tag = "py3-none-" + sysconfig.get_platform().replace("-", "_").replace(".", "_")
    (info / "WHEEL").write_text(f"Wheel-Version: 1.0\nGenerator: orm-extension-package\nRoot-Is-Purelib: false\nTag: {tag}\n")
    (info / "entry_points.txt").write_text(f"[orm.extensions]\n{ident} = {module}\n")
    records = []
    wheel = out / f"{module}-{version}-{tag}.whl"
    with zipfile.ZipFile(wheel, "w", zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(stage.rglob("*")):
            if not path.is_file():
                continue
            data = path.read_bytes()
            relative = path.relative_to(stage).as_posix()
            hashed = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
            records.append((relative, "sha256=" + hashed, len(data)))
            archive.writestr(relative, data)
        record = f"{info.name}/RECORD"
        records.append((record, "", ""))
        stream = io.StringIO(newline="")
        csv.writer(stream).writerows(records)
        archive.writestr(record, stream.getvalue())
    # Build away from the destination, so a failed compiler leaves the last package intact.
    npm = stage / "npm"
    copy_runtime("typescript", npm)
    sources = sorted(str(p) for p in npm.rglob("*.ts"))
    subprocess.run([config["tsc"], *sources, "--outDir", str(npm), "--module", "ESNext", "--moduleResolution", "Bundler", "--target", "ES2022", "--declaration", "--noCheck"], check=True)
    (npm / "package.json").write_text(json.dumps({
        "name": npm_name, "version": version, "type": "module",
        "main": "./index.js", "types": "./index.d.ts",
        "exports": {".": {"types": "./index.d.ts", "default": "./index.js"}},
        "orm": {"extension": ident}, "peerDependencies": {"orm": "0.1.0"},
    }, indent=2) + "\n")
    if (out / "npm").exists():
        shutil.rmtree(out / "npm")
    shutil.copytree(npm, out / "npm")
    print(wheel)
    print(out / "npm")
