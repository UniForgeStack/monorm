"""Permission names (`"blog.add_post"`) and `sync_permissions()`."""

from __future__ import annotations

from collections.abc import Iterable
from typing import Any

from monorm import Database, Registry
from monorm import registry as default_registry

ACTIONS = ("add", "change", "delete", "view")


def split_perm(perm: str) -> tuple[str, str]:
    """`"blog.add_post"` is the label `blog` and the codename `add_post`."""
    label, dot, codename = perm.partition(".")
    if not dot or not label or not codename:
        raise ValueError(f"permission {perm!r} must have the form '<label>.<codename>'")
    return label, codename


def _active_model(name: str, registry: Registry | None = None) -> Any:
    """The model class for `name`: the model that overrides it (`@@inherit.override`), else `name` itself.

    The app does not depend on the `monorm-inherit` package, so it reads `model_overrides` itself.
    """
    from monorm.ext import behavior_value

    reg = registry if registry is not None else default_registry
    overrides: dict[str, str] = behavior_value("model_overrides", reg) or {}
    return reg.get(overrides.get(name, name))


async def sync_permissions(
    models: Iterable[type[Any]], label: str, *, using: Database | None = None, registry: Registry | None = None
) -> int:
    """Create `add`, `change`, `delete` and `view` for each model in one bulk insert; rows that exist stay.

    The permission names are `<label>.<action>_<model name in lower case>`. Returns the number of new rows.
    """
    Permission = _active_model("Permission", registry)
    rows = [
        {"label": label, "codename": f"{action}_{name}", "name": f"Can {action} {name}"}
        for name in (m.__name__.lower() for m in models)
        for action in ACTIONS
    ]
    if not rows:
        return 0
    query = Permission.objects.using(using).insert_many(rows)
    inserted: int = await query.on_conflict(Permission.label, Permission.codename, update=False)
    return inserted
