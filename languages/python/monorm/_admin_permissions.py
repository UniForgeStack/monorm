"""The permission names of an admin site in the auth tables: sync, stale names and seeded roles.

Both steps use the auth app through its Python API, so no SQL text lives here.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from fnmatch import fnmatchcase
from typing import TYPE_CHECKING, Any, Mapping, Sequence

if TYPE_CHECKING:
    from .db import Database

__all__ = ["RoleSeedReport", "SyncReport", "resolve_pattern_grants"]


@dataclass(frozen=True)
class SyncReport:
    """What ``Admin.sync_permissions`` did."""

    created: int
    """Names that the call added to the auth tables."""
    stale: tuple[str, ...] = ()
    """Names in the auth tables, under a label of this site, that the strategy no longer lists. Reported, never deleted."""


@dataclass(frozen=True)
class RoleSeedReport:
    """What ``Admin.seed_roles`` did."""

    roles_created: tuple[str, ...] = field(default=())
    """Names of the roles that the call created."""
    grants: int = 0
    """Role and permission pairs that the call added."""


def resolve_pattern_grants(patterns: Sequence[str], names: Sequence[str], role: str) -> list[str]:
    """The names that ``patterns`` select, in the order of ``names``.

    A pattern is a glob over ``<label>.<codename>`` (``orders.*``). A leading ``-`` removes what the patterns before it added.
    A pattern that matches no name is an error: it is most often a typo.
    ``[`` and ``]`` are not allowed: a permission name never holds them, and the TypeScript host reads them as plain text.
    """
    chosen: dict[str, None] = {}
    for pattern in patterns:
        remove = pattern.startswith("-")
        glob = pattern[1:] if remove else pattern
        if "[" in glob or "]" in glob:
            raise ValueError(f"role {role!r}: the pattern {pattern!r} has [ or ]; a glob here has only * and ?")
        hits = [n for n in names if fnmatchcase(n, glob)]
        if not hits:
            raise ValueError(f"role {role!r}: the pattern {pattern!r} matches no permission name of the site")
        for n in hits:
            if remove:
                chosen.pop(n, None)
            else:
                chosen[n] = None
    return [n for n in names if n in chosen]


def _auth_model(name: str, registry: Any) -> Any:
    from monorm_auth.permissions import _active_model

    try:
        return _active_model(name, registry)
    except Exception:
        raise ValueError(f"the schema has no auth model {name}: import the auth app in schema.prisma") from None


async def sync(names: Sequence[str], labels: Sequence[str], database: Database) -> SyncReport:
    from monorm_auth.permissions import sync_permissions

    registry = database._registry
    permission = _auth_model("Permission", registry)
    created = await sync_permissions(names, using=database, registry=registry)
    rows = await permission.objects.using(database).filter(permission.label.in_(list(labels)))
    known = set(names)
    stale = sorted(f"{r.label}.{r.codename}" for r in rows if f"{r.label}.{r.codename}" not in known)
    return SyncReport(created, tuple(stale))


async def seed(roles: Mapping[str, Sequence[str]], names: Sequence[str], database: Database) -> RoleSeedReport:
    """Creates each role that is absent and adds the pairs that are absent. It changes nothing on a second run and never deletes."""
    if not roles:
        return RoleSeedReport()
    from monorm_auth.permissions import split_perm

    registry = database._registry
    wanted = {role: resolve_pattern_grants(patterns, names, role) for role, patterns in roles.items()}
    role_model = _auth_model("Role", registry)
    permission_model = _auth_model("Permission", registry)
    through = _auth_model("RolePermission", registry)
    existing = {r.name: r for r in await role_model.objects.using(database).filter(role_model.name.in_(list(roles)))}
    missing = [name for name in roles if name not in existing]
    if missing:
        await role_model.objects.using(database).insert_many([{"name": name} for name in missing]).on_conflict(role_model.name, update=False)
        existing = {r.name: r for r in await role_model.objects.using(database).filter(role_model.name.in_(list(roles)))}
    codes = {split_perm(n) for grants in wanted.values() for n in grants}
    permissions = {
        (p.label, p.codename): p
        for p in await permission_model.objects.using(database).filter(permission_model.label.in_(sorted({label for label, _ in codes})))
    }
    absent = sorted(f"{label}.{codename}" for label, codename in codes if (label, codename) not in permissions)
    if absent:
        raise ValueError(f"the permissions {absent} are not in the auth tables: run sync-permissions first")
    pairs = [
        {"role_id": existing[role].id, "permission_id": permissions[split_perm(n)].id}
        for role, grants in wanted.items()
        for n in grants
    ]
    inserted = 0
    if pairs:
        inserted = await through.objects.using(database).insert_many(pairs).on_conflict(through.role_id, through.permission_id, update=False)
    return RoleSeedReport(tuple(missing), inserted)
