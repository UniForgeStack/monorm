"""Finds the installed extension packages: a `monorm.extensions` entry point named like the extension id.

The value is a module (`monorm_tracking`); its directory holds `monorm-extension.toml`.
Discovery reads metadata; definition imports the selected runtime packages.
"""

from __future__ import annotations

import importlib.metadata
import importlib.util
import os
from pathlib import Path
from types import ModuleType

from ._native import SchemaError

MANIFEST = "monorm-extension.toml"
BUILT_IN = "monorm"


def _manifest_of(entry: importlib.metadata.EntryPoint) -> Path:
    module = entry.value.partition(":")[0].strip()
    top, *inner = module.split(".")
    # find_spec of a top-level package does not run it; it also finds the source of an editable install.
    spec = importlib.util.find_spec(top) if top.isidentifier() else None
    roots = list(spec.submodule_search_locations or ()) if spec else []
    path = Path(roots[0], *inner, MANIFEST) if roots else Path(os.devnull)
    if not path.is_file():
        raise SchemaError(
            f"the `monorm.extensions` entry point of `{entry.name}` is {entry.value!r}, which is not a package with an {MANIFEST} file"
        )
    return path


def _owner(entry: importlib.metadata.EntryPoint) -> str:
    return entry.dist.name if entry.dist else "an unknown package"


def _is_built_in(owner: str) -> bool:
    return owner.lower().replace("_", "-") == BUILT_IN


def resolve_extensions() -> dict[str, str]:
    """``{extension id: manifest file}`` of every installed extension package."""
    found: dict[str, tuple[str, str]] = {}
    for entry in importlib.metadata.entry_points(group="monorm.extensions"):
        copy = (str(_manifest_of(entry)), _owner(entry))
        if entry.name in found and found[entry.name][0] != copy[0]:
            raise SchemaError(_installed_twice(entry.name, found[entry.name], copy))
        found[entry.name] = copy
    return {ident: path for ident, (path, _) in found.items()}


def _installed_twice(ident: str, first: tuple[str, str], second: tuple[str, str]) -> str:
    """Each copy is ``(manifest file, owner package)``."""
    if not any(_is_built_in(owner) for _, owner in (first, second)):
        return f"extension `{ident}` is installed twice: {first[0]} and {second[0]}; uninstall one of the packages"
    shown = " and ".join(f"{f'built into {BUILT_IN}' if _is_built_in(owner) else owner}: {path}" for path, owner in (first, second))
    outside = [owner for _, owner in (first, second) if not _is_built_in(owner)]
    return f"extension `{ident}` is installed twice: {shown}; uninstall {outside[0] if outside else 'one of the packages'}"


def package_dirs() -> dict[str, str]:
    """``{extension id: package directory}`` of every installed extension package."""
    return {ident: str(Path(manifest).parent) for ident, manifest in resolve_extensions().items()}


def runtime_modules() -> dict[str, str]:
    """Installed import names, without importing extension code."""
    return {entry.name: entry.value.partition(":")[0].strip() for entry in importlib.metadata.entry_points(group="monorm.extensions")}


def import_runtime(ident: str) -> ModuleType:
    name = runtime_modules().get(ident)
    if name is None:
        raise SchemaError(f"extension {ident}: runtime package is not installed")
    return importlib.import_module(name)
