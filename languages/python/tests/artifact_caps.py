"""The native artifact's capabilities and extension ids."""

import json

from orm import _native


def artifact_capabilities() -> list[str]:
    return json.loads(_native.native_artifact())["capabilities"]


def has_extension(*ids: str) -> bool:
    """True when the native artifact contains every named extension, such as `orm.locking`."""
    extensions = json.loads(_native.native_artifact())["extensions"]
    return all(i in extensions for i in ids)
