"""Finds the installed extension packages: an `orm.extensions` entry point named like the extension id.

The value is a module (`orm_tracking`); its directory holds `orm-extension.toml`.
Discovery reads metadata; definition imports the selected runtime packages.
"""

from __future__ import annotations

import importlib.metadata
import importlib.util
import os
from pathlib import Path
from types import ModuleType

from ._native import SchemaError

MANIFEST = "orm-extension.toml"


def _manifest_of(entry: importlib.metadata.EntryPoint) -> Path:
    module = entry.value.partition(":")[0].strip()
    top, *inner = module.split(".")
    # find_spec of a top-level package does not run it; it also finds the source of an editable install.
    spec = importlib.util.find_spec(top) if top.isidentifier() else None
    roots = list(spec.submodule_search_locations or ()) if spec else []
    path = Path(roots[0], *inner, MANIFEST) if roots else Path(os.devnull)
    if not path.is_file():
        raise SchemaError(
            f"the `orm.extensions` entry point of `{entry.name}` is {entry.value!r}, which is not a package with an {MANIFEST} file"
        )
    return path


def resolve_extensions() -> dict[str, str]:
    """``{extension id: manifest file}`` of every installed extension package."""
    found: dict[str, str] = {}
    for entry in importlib.metadata.entry_points(group="orm.extensions"):
        path = str(_manifest_of(entry))
        if entry.name in found and found[entry.name] != path:
            raise SchemaError(
                f"extension `{entry.name}` is installed twice: {found[entry.name]} and {path}; uninstall one of the packages"
            )
        found[entry.name] = path
    return found


def package_dirs() -> dict[str, str]:
    """``{extension id: package directory}`` of every installed extension package."""
    return {ident: str(Path(manifest).parent) for ident, manifest in resolve_extensions().items()}


def runtime_modules() -> dict[str, str]:
    """Installed import names, without importing extension code."""
    return {entry.name: entry.value.partition(":")[0].strip() for entry in importlib.metadata.entry_points(group="orm.extensions")}


def import_runtime(ident: str) -> ModuleType:
    name = runtime_modules().get(ident)
    if name is None:
        raise SchemaError(f"extension {ident}: runtime package is not installed")
    return importlib.import_module(name)
