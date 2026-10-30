"""Stage the built-in extension packages and the auth app into `languages/python/`, so `maturin build` ships them in the `monorm` wheel.

Each extension becomes a top-level package `<module>/` with its runtime source, `MANIFEST_HASH`, `monorm-extension.toml`
and the library of this platform. The libraries come from `--libraries DIR` (laid out `<name>/<platform>/<file>` as `assemble-bundle.mjs --collect` writes them, or
`<platform>/<file>` as the `monorm-extension-package library` mode writes them) or are built here with `cargo`.

    python languages/python/packaging/stage.py [--libraries DIR]
"""

import argparse
import hashlib
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
TARGET = ROOT / "languages" / "python"
EXTENSIONS = ("timestamps", "soft-delete", "locking", "inherit", "query-defaults", "generic", "proxy", "model-composition", "file-storage")
AUTH = ROOT / "apps" / "auth" / "python" / "monorm_auth"
# Platform key, library file prefix and suffix; the keys match `monorm-extension-package`.
PLATFORMS = {("Darwin", "arm64"): ("darwin-arm64", "lib", ".dylib"), ("Linux", "x86_64"): ("linux-x64", "lib", ".so"), ("Linux", "aarch64"): ("linux-arm64", "lib", ".so"), ("Windows", "AMD64"): ("win32-x64", "", ".dll")}
IGNORE = shutil.ignore_patterns("__pycache__", "*.pyc")


def host() -> tuple[str, str, str]:
    key = (platform.system(), platform.machine())
    if key not in PLATFORMS:
        raise SystemExit(f"no platform key for {key}; the keys are {sorted(PLATFORMS)}")
    return PLATFORMS[key]


def module_of(ident: str) -> str:
    return ident.replace(".", "_").replace("-", "_")


def library_file(library: str, prefix: str, suffix: str) -> str:
    return library if "." in library else f"{prefix}{library}{suffix}"


def build_library(directory: Path, out: Path) -> None:
    subprocess.run(
        ["cargo", "run", "-q", "-p", "monorm-extension-package", "--", "library", str(directory), "--out", str(out)],
        cwd=ROOT,
        check=True,
    )


def stage_extension(name: str, libraries: Path, key: str, prefix: str, suffix: str) -> str:
    directory = ROOT / "extensions" / name
    raw = (directory / "monorm-extension.toml").read_bytes()
    manifest = tomllib.loads(raw.decode())
    module = module_of(manifest["id"])
    destination = TARGET / module
    destination.mkdir()
    source = (manifest.get("python") or {}).get("source")
    if source:
        shutil.copytree(ROOT / "extensions" / name / source, destination, dirs_exist_ok=True, ignore=IGNORE)
    entry = destination / "__init__.py"
    text = entry.read_text() if entry.exists() else ""
    # Replace a checked-in fixture hash; the built constant always describes the shipped bytes.
    text = re.sub(r"^MANIFEST_HASH\s*=.*\n?", "", text, flags=re.M)
    entry.write_text(f'{text}\nMANIFEST_HASH = "{hashlib.sha256(raw).hexdigest()}"\n')
    (destination / "py.typed").touch()
    (destination / "monorm-extension.toml").write_bytes(raw)
    file = library_file(manifest["library"], prefix, suffix)
    base = libraries / name if (libraries / name).is_dir() else libraries
    shutil.copy2(base / key / file, destination / file)
    return module


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--libraries", type=Path, help="prebuilt libraries, laid out <platform>/<file>")
    args = parser.parse_args()
    key, prefix, suffix = host()
    for old in TARGET.glob("monorm_*/"):
        shutil.rmtree(old)
    with tempfile.TemporaryDirectory(prefix="monorm-stage-") as temporary:
        libraries = args.libraries
        if libraries is None:
            libraries = Path(temporary)
            for name in EXTENSIONS:
                build_library(ROOT / "extensions" / name, libraries)
        modules = [stage_extension(name, libraries, key, prefix, suffix) for name in EXTENSIONS]
    shutil.copytree(AUTH, TARGET / "monorm_auth", ignore=IGNORE)
    print("staged", *modules, "monorm_auth", file=sys.stderr)


if __name__ == "__main__":
    main()
