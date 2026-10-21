"""Finds the schema of each installed app that a schema imports with ``import "app:<name>"``.

Used by ``orm generate``, ``orm migrate``, ``orm.load(path)`` and ``Migrations(path)``.
An app declares its schema in an ``orm.apps`` entry point named like its distribution
(``orm-auth = "orm_auth:schema.prisma"``: the package, a colon, the file name; `pyproject.toml` accepts only this form). No app package is imported.
"""

from __future__ import annotations

import importlib.metadata
import importlib.util
import os
import re
from pathlib import Path

from ._native import SchemaError

_IMPORT = re.compile(r'^\s*import\s+"([^"]+)"', re.MULTILINE)


def _schema_of(name: str) -> Path | None:
    """The schema file of app `name`; None when no such package is installed."""
    matches = [e for e in importlib.metadata.entry_points(group="orm.apps") if e.name == name]
    if not matches:
        try:
            importlib.metadata.distribution(name)
        except importlib.metadata.PackageNotFoundError:
            return None
        raise SchemaError(
            f'import "app:{name}": the `{name}` package declares no schema; '
            f'it needs an `orm.apps` entry point named `{name}` whose value is `<package>:<schema file>`, such as `orm_auth:schema.prisma`'
        )
    entry = matches[0]
    module, _, file = entry.value.partition(":")
    top, *inner = module.split(".")
    # find_spec of a top-level package does not run it; it also finds the source of an editable install.
    spec = importlib.util.find_spec(top) if top.isidentifier() and file else None
    roots = list(spec.submodule_search_locations or ()) if spec else []
    path = Path(roots[0], *inner, file) if roots else Path(os.devnull)
    if not path.is_file():
        raise SchemaError(f'import "app:{name}": the `orm.apps` entry point of `{name}` is {entry.value!r}, which is not a schema file in an installed package')
    return path


def resolve_apps(schema: str | os.PathLike[str]) -> dict[str, str]:
    """``{name: schema file}`` of every app the schema, its imports and their apps import.
    An app that is not installed is left out: the schema loader reports it."""
    apps: dict[str, str] = {}
    pending = [Path(schema).resolve()]
    seen: set[Path] = set()
    while pending:
        file = pending.pop()
        if file in seen:
            continue
        seen.add(file)
        try:
            text = file.read_text(encoding="utf-8")
        except OSError:
            continue  # the loader reports an unreadable file
        for target in _IMPORT.findall(text):
            if target.startswith("app:"):
                name = target[len("app:") :]
                if name not in apps and (found := _schema_of(name)) is not None:
                    apps[name] = str(found)
                    pending.append(found)
            elif target.endswith(".prisma"):
                pending.append((file.parent / target).resolve())
    return apps
