"""Build the `full` wheel: tooling engine plus all first-party extensions, through `orm-extension-build`.

    python languages/python/packaging/build-full-wheel.py [wheel-dir]   # default: target/wheels

Needs `cargo` and `maturin`. Do not pass host features by hand: `full/build.json` selects the extension
crates and the manifests turn on the host features.
"""

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[3]
packaging = Path(__file__).resolve().parent / "full"
wheels = Path(sys.argv[1] if len(sys.argv) > 1 else root / "target" / "wheels").resolve()
config = packaging / "build.json"
output = (packaging / json.loads(config.read_text())["output"]).resolve()
shutil.rmtree(output, ignore_errors=True)  # the frontend needs a new directory
subprocess.run(["cargo", "run", "-q", "-p", "orm-extension-build", "--", str(config)], cwd=root, check=True)
workspace = output / "languages" / "python" / "packaging" / "full"
shutil.copytree(packaging, workspace, ignore=shutil.ignore_patterns("build.json", "__pycache__"))
env = {
    **os.environ,
    "ORM_CORE_COMPOSITION": str(output / "composition.rs"),
    "ORM_ENGINE_COMPOSITION": str(output / "engine-composition.rs"),
    "ORM_PYTHON_METHODS": str(output / "python-methods.rs"),
}
wheels.mkdir(parents=True, exist_ok=True)
subprocess.run(["maturin", "build", "--release", "--out", str(wheels)], cwd=workspace, env=env, check=True)
