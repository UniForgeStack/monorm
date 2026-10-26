"""Extension runtime contracts: generated packages, static artifacts and member rules."""

from __future__ import annotations

import functools
import hashlib
import json
import warnings
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


class OrmStaleModelsWarning(UserWarning):
    """An installed extension differs from the generated schema's manifest lock."""


def packages(ir: dict[str, Any], extensions: dict[str, ModuleType]) -> dict[str, dict[str, Any]]:
    """Read generated package contracts, importing runtime modules only when needed."""
    stamps = ir.get("behavior", {}).get("packages", {})
    if not stamps:
        return {}
    from ._packages import import_runtime
    from .model import Model
    from .query import BoundQuerySet, QuerySet

    host_bases = set((*Model.__mro__, *QuerySet.__mro__, *BoundQuerySet.__mro__))
    found: dict[str, dict[str, Any]] = {}
    order: list[str] = []
    visiting: set[str] = set()

    def visit(ident: str) -> None:
        if ident in order:
            return
        if ident in visiting:
            raise TypeError(f"cyclic packaged extension dependencies at {ident}")
        visiting.add(ident)
        for dep in stamps[ident].get("dependencies", {}):
            if dep in stamps:
                visit(dep)
        visiting.remove(ident)
        order.append(ident)

    for ident in stamps:
        visit(ident)
    for ident in order:
        stamp = stamps[ident]
        if ident in records():
            raise TypeError(f"extension {ident} is in both the generated packages and the native artifact")
        module = extensions.get(ident)
        if module is None:
            module = import_runtime(ident)
            extensions[ident] = module
        installed = getattr(module, "MANIFEST_HASH", None)
        if installed != stamp["manifest_hash"]:
            warnings.warn(
                f"extension {ident}: locked manifest hash {stamp['manifest_hash']}, installed {installed}; run `orm generate`",
                OrmStaleModelsWarning, stacklevel=3,
            )
        section = stamp.get("language_members", {}).get("python")
        if section is None:
            continue
        record = dict(section)
        for export, key in (("query_set", "query_set_members"), ("instance", "instance_members"), ("bound_query_set", "bound_query_set_members")):
            name = section.get(export)
            cls = getattr(module, name, None) if name else None
            if name and not isinstance(cls, type):
                raise TypeError(f"extension {ident} has no {name}; run `orm generate`")
            record[key] = sorted({name for base in cls.__mro__ if base not in host_bases
                                  for name in vars(base) if not name.startswith("_")}) if cls else []
        found[ident] = record
    return found


def package_members(ir: dict[str, Any], records: dict[str, dict[str, Any]]) -> dict[str, tuple[str, ...]]:
    behavior = ir.get("behavior", {})
    stamps = behavior.get("packages", {})
    return {
        model["name"]: tuple(ident for ident in records if any(
            d["model"] == model["name"] and d["attribute"] in stamps[ident]["attributes"]
            for d in behavior.get("declarations", [])
        ) or any(r.get("model") == model["name"] and r.get("source") == ident
                 for key in ("read_defaults", "delete_rewrites", "write_assignments", "instance_guards") for r in behavior.get(key, [])))
        for model in ir["models"]
    }
