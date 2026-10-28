"""The native artifact's capabilities and the installed extension ids."""

import json

from orm import _native
from orm._packages import resolve_extensions


def artifact_capabilities() -> list[str]:
    return json.loads(_native.native_artifact())["capabilities"]


def has_extension(*ids: str) -> bool:
    """True when an installed package contains every named extension, such as `orm.file-storage`.

    A missing `orm.timestamps`, `orm.soft-delete`, `orm.locking`, `orm.inherit`, `orm.query-defaults`, `orm.generic`, `orm.proxy`, `orm.composition` or `orm.file-storage` package fails with an assert.
    """
    extensions = resolve_extensions()
    for ident in ids:
        if ident in {"orm.timestamps", "orm.soft-delete", "orm.locking", "orm.inherit", "orm.query-defaults", "orm.generic", "orm.proxy", "orm.composition", "orm.file-storage"}:
            assert ident in extensions, f"install the {ident} package with scripts/install-extension-packages.sh"
    return all(i in extensions for i in ids)
