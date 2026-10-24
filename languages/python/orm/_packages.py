"""Finds the installed extension packages: an `orm.extensions` entry point named like the extension id.

The value is a module (`orm_tracking`); its directory holds `orm-extension.toml`.
No extension package is imported. The Rust loader selects the extensions that a schema uses.
"""

from __future__ import annotations

import importlib.metadata
import importlib.util
import os
from pathlib import Path

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
