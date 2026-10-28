"""Extension runtime contracts: generated packages and member rules."""

from __future__ import annotations

import warnings
from types import ModuleType
from typing import Any, Callable


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
        module = extensions.get(ident)
        if module is None:
            module = import_runtime(ident)
            extensions[ident] = module
        installed = getattr(module, "MANIFEST_HASH", None)
        if installed != stamp["manifest_hash"]:
            warnings.warn(
                f"extension {ident}: locked manifest hash {stamp['manifest_hash']}, installed {installed}; run `monorm generate`",
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
                raise TypeError(f"extension {ident} has no {name}; run `monorm generate`")
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
