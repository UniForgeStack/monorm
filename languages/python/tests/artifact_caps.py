"""The native artifact's capabilities, plus a name for each tracking extension it contains."""

import json

from orm import _native

_TRACKING = {"orm.timestamps": "updated-at", "orm.soft-delete": "soft-delete", "orm.locking": "optimistic-locking"}


def artifact_capabilities() -> list[str]:
    artifact = json.loads(_native.native_artifact())
    return [*artifact["capabilities"], *(name for ext, name in _TRACKING.items() if ext in artifact["extensions"])]
