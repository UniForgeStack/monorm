"""Extension language code at runtime: the check against the native artifact and the member rules."""

from __future__ import annotations

import functools
import hashlib
import json
from pathlib import Path
from types import ModuleType
from typing import Any, Callable

from . import _native

_HEADER = "# orm-ext "
_HEADER_LINES = 3


@functools.cache
def records() -> dict[str, dict[str, Any]]:
    """The artifact's Python ``language_members`` record of each extension, by extension id."""
    artifact = json.loads(_native.native_artifact())
    return {r["extension"]: r for r in artifact.get("language_members", ()) if r["language"] == "python"}


def _digest(files: list[tuple[str, bytes]]) -> str:
    sha = hashlib.sha256()
    for name, content in sorted(files, key=lambda f: f[0].encode()):
        raw = name.encode()
        sha.update(len(raw).to_bytes(8, "little") + raw + len(content).to_bytes(8, "little") + content)
    return sha.hexdigest()


def check(extension: str, module: ModuleType) -> dict[str, Any]:
    """The artifact record of ``extension``, after a check that the generated files of ``module`` match it."""
    record = records().get(extension)
    if record is None:
        raise TypeError(
            f"generated models use extension {extension}, which the native artifact does not have; run `orm generate`"
        )
    root = Path(str(module.__file__)).parent
    files: list[tuple[str, bytes]] = []
    for path in sorted(root.rglob("*")):
        if path.suffix not in (".py", ".pyi") or "__pycache__" in path.parts:
            continue
        lines = path.read_bytes().split(b"\n", _HEADER_LINES)
        second = lines[1].decode(errors="replace") if len(lines) > 1 else ""
        if len(lines) <= _HEADER_LINES or not second.startswith(_HEADER):
            raise TypeError(f"{path}: not a generated extension file for {extension}; run `orm generate`")
        if second != f"{_HEADER}{extension} {record['version']} {record['hash']}":
            raise TypeError(f"{path}: generated for another build of extension {extension}; run `orm generate`")
        files.append((path.relative_to(root).as_posix(), lines[_HEADER_LINES]))
    if _digest(files) != record["hash"]:
        raise TypeError(f"the generated files of extension {extension} in {root} were edited; run `orm generate`")
    return record


def members(
    model: str,
    taken: set[str],
    extensions: list[tuple[str, dict[str, Any]]],
    host_query_set: Callable[[str], bool],
    host_instance: Callable[[str], bool],
    host_bound: Callable[[str], bool],
) -> None:
    """Reject a member name that a host member, a field, a relation or an earlier extension has.

    ``extensions`` are in chain order. A declared override may repeat a host member or an
    earlier extension's member, never a field or a relation.
    """
    given: dict[str, str] = {}
    for extension, record in extensions:
        overrides = set(record["overrides"])
        for kind, names, host in (
            ("query set", record["query_set_members"], host_query_set),
            ("instance", record["instance_members"], host_instance),
            ("bound query set", record["bound_query_set_members"], host_bound),
        ):
            for name in names:
                if name in taken:
                    raise TypeError(
                        f"{model}.{name}: extension {extension} {kind} member collides with a field or a relation"
                    )
                if name in overrides:
                    continue
                if name in given:
                    raise TypeError(
                        f"{model}.{name}: extension {extension} {kind} member collides with a member of extension {given[name]}"
                    )
                if host(name):
                    raise TypeError(
                        f"{model}.{name}: extension {extension} {kind} member collides with a host member; declare it as an override"
                    )
        for name in (*record["query_set_members"], *record["instance_members"]):
            given.setdefault(name, extension)
