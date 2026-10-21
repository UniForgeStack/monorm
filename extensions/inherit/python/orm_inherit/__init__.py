"""The model that overrides a model: ``model_for("User")`` gives ``MyUser`` after ``@@inherit.override("User")``."""

from __future__ import annotations

from typing import Any

from orm import QuerySet, Registry
from orm.ext import ModelT, behavior_value

__all__ = ["InheritInstance", "InheritQuerySet", "model_for"]


class InheritQuerySet(QuerySet[ModelT]):
    """No members: the extension changes the schema, not the query set."""


class InheritInstance:
    """No members: the extension changes the schema, not the instance."""


def model_for(name: str, registry: Registry | None = None) -> Any:
    """The model class that is active for ``name``: the model that overrides it, else ``name`` itself."""
    from orm import registry as default

    reg = registry if registry is not None else default
    overrides: dict[str, str] = behavior_value("model_overrides", reg) or {}
    return reg.get(overrides.get(name, name))
