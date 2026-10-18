"""Immutable adapter selection for module/definition setup."""

from __future__ import annotations

import json
from . import _native

_metadata = json.loads(_native.profile_metadata())
_adapters = _metadata.get("adapters")
if not isinstance(_adapters, list) or any(
    not isinstance(name, str) or _metadata["capabilities"].get(name) is not True for name in _adapters
):
    raise ImportError("native adapter/capability metadata mismatch")
ADAPTERS: frozenset[str] = frozenset(_adapters)

# The capabilities this language adapter implements; an extension lists the ones it needs as `requires`.
LANGUAGE_CAPABILITIES: frozenset[str] = frozenset({"async-field-adapter.v1"})


def check_requires(artifact: dict[str, object], declared: frozenset[str] = LANGUAGE_CAPABILITIES) -> None:
    """Fail when a selected extension requires a capability that ``declared`` lacks."""
    requires = artifact.get("requires", {})
    if not isinstance(requires, dict):
        raise ImportError("incompatible orm native artifact; its requires list is malformed")
    for extension, needed in sorted(requires.items()):
        for capability in needed:
            if capability not in declared:
                raise ImportError(
                    f"extension {extension} requires the capability {capability}, "
                    "which this orm language adapter does not provide; upgrade orm or rebuild the artifact"
                )
