"""The native artifact's capabilities and extension ids."""

import json

from orm import _native
from orm._packages import resolve_extensions


def artifact_capabilities() -> list[str]:
    return json.loads(_native.native_artifact())["capabilities"]


def has_extension(*ids: str) -> bool:
    """True when the native artifact or an installed package contains every named extension, such as `orm.file-storage`.

    A missing `orm.timestamps`, `orm.soft-delete`, `orm.locking`, `orm.inherit` or `orm.query-defaults` package fails with an assert.
    """
    extensions = {**json.loads(_native.native_artifact())["extensions"], **resolve_extensions()}
    for ident in ids:
        if ident in {"orm.timestamps", "orm.soft-delete", "orm.locking", "orm.inherit", "orm.query-defaults"}:
            assert ident in extensions, f"install the {ident} package with scripts/install-extension-packages.sh"
    return all(i in extensions for i in ids)
